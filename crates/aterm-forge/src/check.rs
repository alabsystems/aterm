// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cargo forge check` — THE GATE VERB, wired as `xtask gate forge`.
//!
//! [`check_report`] is the symbol the roster calls. It answers one question —
//! *is aterm's third-party surface still the surface this repository says it
//! is?* — with NO COMPILATION and NO NETWORK: one `cargo tree` resolution per
//! cell plus a few hundred file reads, because it sits inside `gate all` and on
//! the pre-push path.
//!
//! # Why patch liveness is the obligation that justifies this gate
//!
//! An UNUSED `[patch.crates-io]` entry is a cargo WARNING at exit 0. It emits
//! nothing at all from `cargo metadata`, and `cargo build` stays green. So a
//! trust fork can be silently disabled — by an upstream major bump, by a new
//! dependency that requires the next major, by a mistyped path — while every
//! other gate in this repository keeps passing and the UNFIXED upstream code
//! compiles into the product.
//!
//! MEASURED ON THIS TREE, and since FIXED — the case this gate was built on.
//! On 2026-08-22 the `linux` cell resolved BOTH `winnow 0.7.15` — the fork at
//! `vendor/winnow`, which exists to fix an `offset_from` underflow — AND an
//! unpatched `winnow 1.0.3` from the registry, reached by
//! `accesskit_winit → accesskit_unix → zbus → zbus_macros (proc-macro) →
//! proc-macro-crate → toml_edit 0.25 → winnow 1.0.3`. The forked fix was absent
//! from the copy that ran inside the compiler on every Linux build, and
//! `[OB-12]` below was the only check in this repository that said so.
//!
//! Dropping the `a11y-accesskit` default feature removed `accesskit_unix`, the
//! only edge pulling zbus, and the sibling with it: as of 2026-08-25 no cell
//! resolves an unpatched sibling of any fork. That is the CLEAN state, and it
//! is what the tests below assert — a gate whose tests demanded the defect
//! still be there would go red the day someone fixed it. `[OB-12]` stays armed
//! because the next major bump or new dependency can reintroduce it silently,
//! at cargo exit 0, and `tests/red_fixtures.rs` keeps a planted violation to
//! prove the detector still bites.
//!
//! # The obligations
//!
//! Numbering CONTINUES [`crate::attest`]'s, the way `aterm_census` numbers one
//! obligation namespace across its four verbs: `[OB-1]`..`[OB-10]` are attest's
//! and are reported here by delegation, never re-implemented.
//!
//! | tag | obligation | source of truth |
//! |---|---|---|
//! | `[OB-1]`..`[OB-10]` | provenance, versions, license, NOTICE, markers, ignore rules | [`crate::attest::report`] |
//! | `[OB-11]` | every `[patch.crates-io]` path fork is REVIEWED | `aterm_census::scan_set::REVIEWED_VENDORED_CRATES` |
//! | `[OB-12]` | every fork is LIVE in the resolved graph, with no unpatched sibling | [`crate::resolve`] |
//! | `[OB-13]` | every CARVED path is still absent | `vendor/forge.toml` `[[carved]]` |
//! | `[OB-14]` | the measured surface conforms to its ratchet | `tools/forge-budget.tsv` |
//!
//! # What it deliberately does NOT re-check
//!
//! Both REVERSE directions of the registration obligation — a
//! `REVIEWED_VENDORED_CRATES` row whose package has LEFT the patch table, and a
//! `vendor/` directory no patch entry claims — are already fail-closed
//! elsewhere: the first at BUILD time in the census's scan-set derivation
//! (`crates/aterm-census/src/scan_set.rs`), the second in attest's `[OB-1]`.
//! Re-implementing either here would give the repository two definitions of one
//! obligation and no way to say which is authoritative.

use crate::model::{Cell, Graph, PkgId};
use crate::{Outcome, PRECISION_NOTE, attest, budget, resolve};
use aterm_census::scan_set::{REVIEWED_VENDORED_CRATES, VendoredMode};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The policy file, which also carries the CARVE LEDGER (`[[carved]]` rows).
const POLICY_FILE: &str = "vendor/forge.toml";
/// The lower-only ratchet.
const BUDGET_FILE: &str = "tools/forge-budget.tsv";

/// What one `cargo tree` resolution yields: the graph, plus the directories
/// cargo disclosed for the packages that have one.
type Resolved = (Graph, BTreeMap<PkgId, PathBuf>);

/// A `[patch.crates-io]` entry redirecting a crates.io package at a local path
/// — the only patch shape this repository uses, and the only one whose liveness
/// a resolved graph can decide.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PatchEntry {
    /// The crates.io package being replaced (the table key).
    name: String,
    /// The declared replacement path, exactly as written (repo-relative).
    path: String,
}

/// One `[[carved]]` row: a path this repository has DELETED from its
/// third-party surface and undertakes to keep deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Carved {
    path: String,
    reason: String,
}

/// The gate's verdict. `could_not_run` is kept apart from `ok` because a gate
/// that could not run must never be read as a gate that passed:
/// [`check_report`] folds it into RED (fail-closed), while [`run`] maps it onto
/// exit code 3 so a broken checkout is not mistaken for a policy failure.
struct Verdict {
    ok: bool,
    log: String,
    could_not_run: Option<String>,
}

/// THE GATE SYMBOL — what `crates/xtask/src/gate.rs` calls. Runs every
/// obligation over `root` across the whole cell matrix and returns
/// `(green, transcript)`.
///
/// Fail-closed: an unreadable manifest, an unresolvable cell or a missing
/// `cargo` all produce `false`, never `true`.
pub fn check_report(root: &Path) -> (bool, String) {
    let v = report_over(root, &resolve::default_cells());
    (v.ok, v.log)
}

/// The verb behind `cargo forge check [--cell NAME]...`.
///
/// `Err` is reserved for could-not-run (exit 3): a bad `--cell`, an unreadable
/// workspace, a cell cargo refused to resolve. A policy violation is
/// `Ok(Outcome { ok: false, .. })` (exit 1), because the two need different
/// answers from a human.
pub fn run(root: &Path, cells: &[String]) -> Result<Outcome, String> {
    let selected = resolve::select(&resolve::default_cells(), cells)?;
    let v = report_over(root, &selected);
    match v.could_not_run {
        // The reason FIRST — `main` prefixes it with "could not run:" — and the
        // whole transcript after it, because the obligations that DID run are
        // still the most useful thing on the screen.
        Some(why) => Err(format!("{why}\n\nthe transcript up to that point:\n{}", v.log)),
        None => Ok(Outcome { ok: v.ok, log: v.log }),
    }
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

fn report_over(root: &Path, cells: &[Cell]) -> Verdict {
    // Canonicalise ONCE, here. Every cargo invocation below runs with
    // `current_dir(root)` and passes `--manifest-path <root>/Cargo.toml`, so a
    // RELATIVE `--root` would be resolved twice and name a path that does not
    // exist ("manifest path `target/x/Cargo.toml` does not exist" — measured).
    // `canon` falls back to the literal path, so a root that does not exist
    // still appears in the diagnostics exactly as the caller typed it.
    let root = &canon(root);
    let mut log = String::new();
    let mut fails = 0usize;
    let mut notes = 0usize;
    let mut could_not_run: Option<String> = None;

    let _ = writeln!(
        log,
        "=== gate forge (third-party surface: provenance, patch liveness, ratchet) ==="
    );
    let _ = writeln!(log, "  root: {}", root.display());
    let _ = writeln!(
        log,
        "  cells: {}",
        cells.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
    );

    // -- [OB-1]..[OB-10] provenance, license, NOTICE, markers ---------------
    let _ = writeln!(
        log,
        "  [OB-1..OB-10] PROVENANCE, LICENSE & NOTICE — delegated to `cargo forge attest`:"
    );
    let (attest_ok, attest_log) = attest::report(root);
    for line in attest_log.lines() {
        let _ = writeln!(log, "    {line}");
    }
    if !attest_ok {
        fails += 1;
        let _ = writeln!(
            log,
            "  ✗ FAIL [OB-1..OB-10] the provenance/license attestation above is RED. Fix: run \
             `cargo forge attest` and clear each obligation it names — the vendored copies are \
             REDISTRIBUTED source, so these are license obligations, not style ones."
        );
    }

    // -- the patch table, which OB-11 and OB-12 both read -------------------
    let manifest = root.join("Cargo.toml");
    let (patches, non_path) = match read_manifest_patches(root) {
        Ok(v) => v,
        Err(e) => {
            fails += 1;
            could_not_run.get_or_insert(e.clone());
            let _ = writeln!(log, "  ✗ FAIL [OB-11] {e}");
            (Vec::new(), Vec::new())
        }
    };
    for name in &non_path {
        notes += 1;
        let _ = writeln!(
            log,
            "    • NOTE [OB-11 scope] `[patch.crates-io].{name}` is not a PATH patch (git or \
             version). forge tracks path forks; nothing here inspects that entry."
        );
    }

    // -- [OB-11] every path fork is REVIEWED ---------------------------------
    let _ = writeln!(
        log,
        "  [OB-11] REVIEW REGISTRATION — every `[patch.crates-io]` path fork has a \
         REVIEWED_VENDORED_CRATES row:"
    );
    let mut missing_on_disk = false;
    for e in &patches {
        match REVIEWED_VENDORED_CRATES.iter().find(|r| r.package == e.name) {
            None => {
                fails += 1;
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-11] `{}` is patched to `{}` but has NO row in \
                     aterm_census::scan_set::REVIEWED_VENDORED_CRATES — a fork nobody reviewed \
                     links into the process. Fix: review the fork, then add \
                     `VendoredCrate {{ package: \"{}\", path: \"{}\", mode: … }}` with its audit \
                     note to crates/aterm-census/src/scan_set.rs.",
                    e.name, e.path, e.name, e.path
                );
            }
            Some(r) if r.path != e.path => {
                fails += 1;
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-11] `{}` is REVIEWED at path `{}` but the patch table \
                     redirects it to `{}` — one of the two is stale, and the reviewed copy is \
                     then not the shipped one. Fix: make the two strings equal (root Cargo.toml \
                     `[patch.crates-io]`, or the row in crates/aterm-census/src/scan_set.rs).",
                    e.name, r.path, e.path
                );
            }
            Some(r) => {
                let _ = writeln!(log, "    ✓ {} → {} ({})", e.name, e.path, mode_label(&r.mode));
            }
        }
        if !root.join(&e.path).join("Cargo.toml").is_file() {
            fails += 1;
            missing_on_disk = true;
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-11] `{}` is patched to `{}`, which has no Cargo.toml on disk — \
                 cargo cannot resolve this workspace at all. Fix: restore the vendored copy, or \
                 delete the `[patch.crates-io].{}` line.",
                e.name, e.path, e.name
            );
        }
    }
    if patches.is_empty() {
        let _ = writeln!(log, "    (no path forks declared in {})", manifest.display());
    } else {
        notes += 1;
        let _ = writeln!(
            log,
            "    • NOTE [OB-11 scope] only the FORWARD direction is scored here, and it is \
             scored against the compiled-in registry for ANY root: a path fork no reviewed row \
             names is unreviewed wherever it sits. The reverse directions belong to attest \
             [OB-1] and to the census scan-set derivation at build time."
        );
    }

    // -- [OB-12] patch liveness, per cell -----------------------------------
    let _ = writeln!(
        log,
        "  [OB-12] PATCH LIVENESS — every fork IS the package the graph resolves, with no \
         unpatched sibling (an unused `[patch]` is a cargo WARNING at exit 0):"
    );
    let mut live_in: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    if missing_on_disk {
        let _ = writeln!(
            log,
            "    SKIPPED — a declared patch path is missing on disk (above); no resolution is \
             meaningful until it is restored."
        );
    } else {
        for cell in cells {
            let mut cell_log = String::new();
            let resolved = resolve::graph_and_paths(root, cell, &mut cell_log);
            if !cell_log.is_empty() {
                notes += 1;
                log.push_str(&cell_log);
            }
            let (graph, paths) = match resolved {
                Ok(v) => v,
                Err(e) => {
                    fails += 1;
                    could_not_run.get_or_insert(e.clone());
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-12] cell `{}` ({}) did not resolve — the gate cannot judge \
                         a graph it does not have.\n    {e}",
                        cell.name, cell.triple
                    );
                    continue;
                }
            };
            for e in &patches {
                let want = canon(&root.join(&e.path));
                let (live, siblings) = classify(&graph, &paths, &e.name, &want);
                if live {
                    live_in.entry(e.name.as_str()).or_default().push(cell.name.clone());
                }
                for sib in &siblings {
                    fails += 1;
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-12] cell `{}` ({}) resolves an UNPATCHED `{}` alongside \
                         the fork: `{}` comes from {}, not from `{}`, so the fork's fix is \
                         ABSENT from the copy that compiles. Fix: run `cargo tree --target {} \
                         --edges normal -i {}` to find the requiring edge, then either bump the \
                         fork to that major and repoint `[patch.crates-io].{}`, or change the \
                         dependency that demands it.",
                        cell.name,
                        cell.triple,
                        e.name,
                        sib.0.spec(),
                        sib.1.as_ref().map_or_else(
                            || "the registry".to_string(),
                            |p| format!("`{}`", p.display())
                        ),
                        e.path,
                        cell.triple,
                        sib.0.spec(),
                        e.name
                    );
                }
            }
        }
    }

    // Absence is judged ACROSS cells: a fork live nowhere is a dead patch; one
    // live somewhere and absent elsewhere is a platform fact, not a defect.
    if !missing_on_disk {
        let mut build_probe: Option<Result<Resolved, String>> = None;
        for e in &patches {
            let live = live_in.get(e.name.as_str()).map_or(0, Vec::len);
            if live == cells.len() && !cells.is_empty() {
                let _ = writeln!(log, "    ✓ {} live in all {} cell(s)", e.name, cells.len());
                continue;
            }
            if live > 0 {
                notes += 1;
                let _ = writeln!(
                    log,
                    "    • NOTE [OB-12] `{}` is live in {} of {} cell(s) ({}) — a fork only some \
                     targets pull in. Not a failure; recorded so a SHRINKING cell set is \
                     visible.",
                    e.name,
                    live,
                    cells.len(),
                    live_in.get(e.name.as_str()).map_or(String::new(), |v| v.join(", "))
                );
                continue;
            }
            // Live in no cell's `--edges normal` graph. A build-time-only fork
            // (pkg-config today) is EXPECTED to be invisible there — but
            // "expected to be invisible" is exactly how a dead patch hides, so
            // prove it in the build closure instead of waving it through.
            let build_only = REVIEWED_VENDORED_CRATES
                .iter()
                .find(|r| r.package == e.name)
                .is_some_and(|r| matches!(r.mode, VendoredMode::BuildDepOnly { .. }));
            let Some(cell) = cells.first() else { continue };
            let probe = build_probe.get_or_insert_with(|| build_closure(root, cell));
            match probe {
                Ok((graph, paths)) => {
                    let want = canon(&root.join(&e.path));
                    let (blive, bsibs) = classify(graph, paths, &e.name, &want);
                    if blive && build_only {
                        let _ = writeln!(
                            log,
                            "    ✓ {} live in the BUILD closure of cell `{}` (reviewed \
                             build-dep-only; invisible to `--edges normal` by design)",
                            e.name, cell.name
                        );
                    } else if blive {
                        notes += 1;
                        let _ = writeln!(
                            log,
                            "    • NOTE [OB-12] `{}` is live only in the BUILD closure of cell \
                             `{}`, but its reviewed row does not classify it build-dep-only. \
                             Fix: re-classify it `VendoredMode::BuildDepOnly` (with the \
                             verification) in crates/aterm-census/src/scan_set.rs.",
                            e.name, cell.name
                        );
                    } else {
                        fails += 1;
                        let _ = writeln!(
                            log,
                            "  ✗ FAIL [OB-12] DEAD PATCH: `{}` is patched to `{}` but resolves \
                             in NO cell — not in any `--edges normal` graph and not in the build \
                             closure of `{}`. cargo reports this as a warning at exit 0, so \
                             nothing else in this repository would notice. Fix: run `cargo tree \
                             --edges all -i {}` to see whether anything still requires it, then \
                             either delete `[patch.crates-io].{}` (with its vendored copy, its \
                             NOTICE line and its REVIEWED_VENDORED_CRATES row) or restore the \
                             dependency edge that used it.",
                            e.name, e.path, cell.name, e.name, e.name
                        );
                    }
                    for sib in &bsibs {
                        fails += 1;
                        let _ = writeln!(
                            log,
                            "  ✗ FAIL [OB-12] the BUILD closure of cell `{}` resolves an \
                             UNPATCHED `{}` where the fork should be. Fix: as above, for `{}`.",
                            cell.name,
                            sib.0.spec(),
                            sib.0.spec()
                        );
                    }
                }
                Err(why) => {
                    fails += 1;
                    could_not_run.get_or_insert(why.clone());
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-12] `{}` is in no `--edges normal` graph and the build \
                         closure of cell `{}` could not be resolved to check it: {why}",
                        e.name, cell.name
                    );
                }
            }
        }
    }

    // -- [OB-13] the carve ledger --------------------------------------------
    let _ = writeln!(
        log,
        "  [OB-13] CARVE LEDGER — every path {POLICY_FILE} records as DELETED is still gone:"
    );
    let carved = match read_carve_ledger(root) {
        Ok(v) => v,
        Err(e) => {
            fails += 1;
            let _ = writeln!(log, "  ✗ FAIL [OB-13] {e}");
            Vec::new()
        }
    };
    if carved.is_empty() {
        let _ = writeln!(
            log,
            "    (no `[[carved]]` rows in {POLICY_FILE}: nothing has been carved yet)"
        );
    }
    for c in &carved {
        if root.join(&c.path).exists() {
            fails += 1;
            let _ = writeln!(
                log,
                "  ✗ FAIL [OB-13] `{}` is recorded as CARVED but EXISTS again on disk. Carving \
                 is a ratchet: reinstating a carved path silently re-enlarges the surface this \
                 repository undertook to shrink, and no other gate looks at it.\n      ledger \
                 reason: {}\n      Fix: delete the path again, or — if the reinstatement is \
                 intended — remove its `[[carved]]` row from {POLICY_FILE} in the SAME commit, \
                 so the ledger and the tree never disagree.",
                c.path,
                if c.reason.is_empty() { "(none recorded)" } else { c.reason.as_str() }
            );
        } else {
            let _ = writeln!(log, "    ✓ {} still absent", c.path);
        }
    }

    // -- [OB-14] the ratchet --------------------------------------------------
    let _ = writeln!(log, "  [OB-14] RATCHET — the measured surface conforms to {BUDGET_FILE}:");
    if root.join(BUDGET_FILE).is_file() {
        match budget::run(root, false, None) {
            Ok(out) => {
                for line in out.log.lines() {
                    let _ = writeln!(log, "    {line}");
                }
                if !out.ok {
                    fails += 1;
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-14] the ratchet above is RED. Fix: shrink the surface, or \
                         — if the growth is deliberate — run `cargo forge budget --update \
                         --allow-regress \"<reason of 80+ characters>\"`, which writes the reason \
                         into {BUDGET_FILE} and reprints it on every run thereafter."
                    );
                }
            }
            Err(e) => {
                fails += 1;
                could_not_run.get_or_insert(e.clone());
                let _ = writeln!(log, "  ✗ FAIL [OB-14] the ratchet could not be evaluated: {e}");
            }
        }
    } else {
        notes += 1;
        let _ = writeln!(
            log,
            "    • NOTE [OB-14] no ratchet rows yet ({BUDGET_FILE} absent) — this surface is \
             MEASURED but not yet RATCHETED, so nothing here stops it growing. Fix: run \
             `cargo forge budget --update` to seed the ceilings from today's measurement."
        );
    }

    // -- verdict --------------------------------------------------------------
    if fails == 0 {
        let _ = writeln!(
            log,
            "gate forge: GREEN — {} path fork(s) reviewed and live across {} cell(s) with no \
             unpatched sibling; {} carved path(s) still absent; provenance attested; {notes} \
             note(s).",
            patches.len(),
            cells.len(),
            carved.len()
        );
    } else {
        let _ = writeln!(
            log,
            "gate forge: FAILED — {fails} obligation(s) violated (every ✗ line above names its \
             fix)."
        );
        log.push_str(PRECISION_NOTE);
        log.push('\n');
    }
    Verdict { ok: fails == 0, log, could_not_run }
}

// ---------------------------------------------------------------------------
// Pieces
// ---------------------------------------------------------------------------

/// Split the nodes named `name` into "the fork is live" and the UNPATCHED
/// siblings resolving under the same name. A sibling is any node of that name
/// that is NOT the declared path package — a registry copy (no path at all), or
/// a second path package somewhere else.
fn classify(
    graph: &Graph,
    paths: &BTreeMap<PkgId, PathBuf>,
    name: &str,
    want: &Path,
) -> (bool, Vec<(PkgId, Option<PathBuf>)>) {
    let mut live = false;
    let mut siblings = Vec::new();
    for id in graph.nodes.iter().filter(|n| n.name == name) {
        let dir = paths.get(id);
        if dir.is_some_and(|d| canon(d) == want) {
            live = true;
        } else {
            siblings.push((id.clone(), dir.cloned()));
        }
    }
    (live, siblings)
}

/// `[patch.crates-io]`, split into path forks and everything else.
fn read_manifest_patches(root: &Path) -> Result<(Vec<PatchEntry>, Vec<String>), String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).map_err(|e| {
        format!(
            "cannot read {}: {e} — `cargo forge check` must run against a workspace root. Fix: \
             pass `--root <workspace dir>`.",
            manifest.display()
        )
    })?;
    parse_patch_table(&text, &manifest)
}

fn parse_patch_table(text: &str, whence: &Path) -> Result<(Vec<PatchEntry>, Vec<String>), String> {
    let doc = text.parse::<toml_edit::DocumentMut>().map_err(|e| {
        format!("{} is not valid TOML: {e} — fix the manifest, then re-run.", whence.display())
    })?;
    let Some(table) = doc
        .get("patch")
        .and_then(|p| p.get("crates-io"))
        .and_then(toml_edit::Item::as_table_like)
    else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut forks = Vec::new();
    let mut other = Vec::new();
    for (name, item) in table.iter() {
        match item.get("path").and_then(toml_edit::Item::as_str) {
            Some(path) => {
                forks.push(PatchEntry { name: name.to_string(), path: path.to_string() });
            }
            None => other.push(name.to_string()),
        }
    }
    forks.sort_by(|a, b| a.name.cmp(&b.name));
    other.sort();
    Ok((forks, other))
}

/// The `[[carved]]` rows of `vendor/forge.toml`. An ABSENT policy file is not
/// an error (nothing has been carved yet); an unreadable or malformed one is.
fn read_carve_ledger(root: &Path) -> Result<Vec<Carved>, String> {
    let file = root.join(POLICY_FILE);
    if !file.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    parse_carve_ledger(&text, &file)
}

fn parse_carve_ledger(text: &str, whence: &Path) -> Result<Vec<Carved>, String> {
    let doc = text.parse::<toml_edit::DocumentMut>().map_err(|e| {
        format!("{} is not valid TOML: {e} — fix the policy file, then re-run.", whence.display())
    })?;
    let Some(item) = doc.get("carved") else { return Ok(Vec::new()) };

    // `[[carved]]` array-of-tables is the written form; an inline array of
    // tables is accepted too, because a hand-edited policy file may use either
    // and refusing one of them would be a parser opinion, not an obligation.
    let mut rows: Vec<(Option<String>, String)> = Vec::new();
    if let Some(tables) = item.as_array_of_tables() {
        for t in tables {
            rows.push((
                t.get("path").and_then(toml_edit::Item::as_str).map(ToString::to_string),
                t.get("reason").and_then(toml_edit::Item::as_str).unwrap_or_default().to_string(),
            ));
        }
    } else if let Some(array) = item.as_array() {
        for v in array {
            let Some(t) = v.as_inline_table() else {
                return Err(format!(
                    "{}: a `carved` entry is not a table. Fix: write each row as `[[carved]]` \
                     with `path = \"…\"` and `reason = \"…\"`.",
                    whence.display()
                ));
            };
            rows.push((
                t.get("path").and_then(toml_edit::Value::as_str).map(ToString::to_string),
                t.get("reason").and_then(toml_edit::Value::as_str).unwrap_or_default().to_string(),
            ));
        }
    } else {
        return Err(format!(
            "{}: `carved` is not an array of tables. Fix: write each carved path as a \
             `[[carved]]` section with `path = \"…\"` and `reason = \"…\"`.",
            whence.display()
        ));
    }

    let mut out = Vec::new();
    for (i, (path, reason)) in rows.into_iter().enumerate() {
        let Some(path) = path else {
            return Err(format!(
                "{}: `[[carved]]` row {} has no `path` key — a ledger row that names nothing \
                 cannot be checked. Fix: add `path = \"<repo-relative path>\"`.",
                whence.display(),
                i + 1
            ));
        };
        if path.is_empty() || Path::new(&path).is_absolute() || path.split('/').any(|s| s == "..") {
            return Err(format!(
                "{}: `[[carved]]` row {} has path `{path}` — carved paths are REPO-RELATIVE and \
                 may not escape the tree. Fix: write it relative to the workspace root, e.g. \
                 `vendor/winit/src/platform_impl/orbital`.",
                whence.display(),
                i + 1
            ));
        }
        out.push(Carved { path, reason });
    }
    Ok(out)
}

/// The `--edges normal,build` closure of one cell.
///
/// Used ONLY to decide whether a fork absent from every shipped graph is
/// genuinely alive as a build-time dependency (`pkg-config` is) or is a dead
/// patch. `--no-dedupe` is deliberately NOT passed: this probe asks for package
/// IDENTITY, never for edges, and the deduped node set is IDENTICAL — measured
/// on this tree, 2026-08-22: 232 distinct nodes either way for
/// `mac-arm --edges normal,build`, and 307 either way for `linux --edges
/// normal` — at a quarter of the cost (`--no-dedupe` prints 379,170 lines for
/// the linux cell against 886).
fn build_closure(root: &Path, cell: &Cell) -> Result<Resolved, String> {
    // `CARGO` is set by cargo itself, so a nested invocation uses the toolchain
    // that launched forge rather than whatever `cargo` is on PATH.
    let exe = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(exe);
    cmd.arg("tree")
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .arg("-p")
        .arg(&cell.package)
        .arg("--edges")
        .arg("normal,build")
        .arg("--target")
        .arg(&cell.triple)
        .arg("--prefix")
        .arg("depth")
        .arg("--locked")
        .arg("--offline")
        .current_dir(root);
    let out = cmd.output().map_err(|e| {
        format!(
            "could not execute `cargo tree`: {e} — install a cargo on PATH, or set CARGO to the \
             binary to use"
        )
    })?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    resolve::parse_tree(&text)
}

fn mode_label(mode: &VendoredMode) -> &'static str {
    match mode {
        VendoredMode::Scanned { .. } => "reviewed, scanned in-process",
        VendoredMode::BuildDepOnly { .. } => "reviewed, build-dep only",
    }
}

/// Canonicalise for comparison, falling back to the literal path when the file
/// system cannot (a path that does not exist compares by its spelling, which is
/// the honest answer).
fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-forge sits two levels under the workspace root")
            .to_path_buf()
    }

    #[test]
    fn the_root_patch_table_is_read_as_path_forks() {
        let (forks, other) = read_manifest_patches(&repo_root()).expect("root manifest reads");
        let names: Vec<&str> = forks.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["indexmap", "libm", "pkg-config", "smol_str", "winit", "winnow"]);
        assert!(other.is_empty(), "this repo patches only path forks: {other:?}");
        for f in &forks {
            assert_eq!(f.path, format!("vendor/{}", f.name), "declared path for {}", f.name);
        }
    }

    #[test]
    fn every_path_fork_in_this_repo_is_registered_for_review() {
        let (forks, _) = read_manifest_patches(&repo_root()).unwrap();
        for f in &forks {
            let row = REVIEWED_VENDORED_CRATES
                .iter()
                .find(|r| r.package == f.name)
                .unwrap_or_else(|| panic!("`{}` has no REVIEWED_VENDORED_CRATES row", f.name));
            assert_eq!(row.path, f.path, "reviewed path for {}", f.name);
        }
    }

    #[test]
    fn a_git_or_version_patch_is_not_counted_as_a_path_fork() {
        let text = "\
[patch.crates-io]
alpha = { path = \"vendor/alpha\" }
beta = { git = \"https://example.invalid/beta\" }
gamma = { version = \"1.2.3\" }
";
        let (forks, other) = parse_patch_table(text, Path::new("/w/Cargo.toml")).unwrap();
        assert_eq!(forks.len(), 1);
        assert_eq!(forks[0], PatchEntry { name: "alpha".into(), path: "vendor/alpha".into() });
        assert_eq!(other, ["beta", "gamma"]);
    }

    #[test]
    fn a_manifest_with_no_patch_table_yields_no_forks() {
        let (forks, other) = parse_patch_table("[package]\nname = \"x\"\n", Path::new("x")).unwrap();
        assert!(forks.is_empty() && other.is_empty());
    }

    #[test]
    fn the_carve_ledger_reads_both_the_table_and_the_inline_form() {
        let tables = "\
[[carved]]
path = \"vendor/winit/src/platform_impl/orbital\"
reason = \"aterm ships no Orbital GUI\"

[[carved]]
path = \"vendor/libm/src/math/arch\"
reason = \"no arch intrinsics reach the shipped build\"
";
        let rows = parse_carve_ledger(tables, Path::new("/w/vendor/forge.toml")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].path, "vendor/winit/src/platform_impl/orbital");
        assert_eq!(rows[1].reason, "no arch intrinsics reach the shipped build");

        let inline = "carved = [ { path = \"vendor/a/b\", reason = \"r\" } ]\n";
        let rows = parse_carve_ledger(inline, Path::new("f")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "vendor/a/b");
    }

    #[test]
    fn a_policy_file_with_no_carved_rows_is_empty_not_an_error() {
        let rows = parse_carve_ledger("[[fork]]\nname = \"winnow\"\n", Path::new("f")).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn a_carved_row_without_a_path_refuses_and_names_the_fix() {
        let err = parse_carve_ledger("[[carved]]\nreason = \"x\"\n", Path::new("f")).unwrap_err();
        assert!(err.contains("no `path` key"), "{err}");
        assert!(err.contains("path = "), "the refusal must say what to type: {err}");
    }

    #[test]
    fn a_carved_path_that_escapes_the_tree_refuses() {
        for bad in ["/etc/passwd", "../outside", "vendor/../../x"] {
            let text = format!("[[carved]]\npath = \"{bad}\"\n");
            let err = parse_carve_ledger(&text, Path::new("f")).unwrap_err();
            assert!(err.contains("REPO-RELATIVE"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_carved_key_that_is_not_an_array_of_tables_refuses() {
        let err = parse_carve_ledger("carved = \"vendor/x\"\n", Path::new("f")).unwrap_err();
        assert!(err.contains("[[carved]]"), "{err}");
    }

    #[test]
    fn classify_separates_the_fork_from_its_unpatched_siblings() {
        let text = "\
0root v0.1.0 (/w/crates/root)
1winnow v0.7.15 (/w/vendor/winnow)
1dep v1.0.0
2winnow v1.0.3
";
        let (graph, paths) = resolve::parse_tree(text).unwrap();
        let (live, sibs) = classify(&graph, &paths, "winnow", Path::new("/w/vendor/winnow"));
        assert!(live, "the path package at the declared dir is the fork");
        assert_eq!(sibs.len(), 1);
        assert_eq!(sibs[0].0, PkgId::new("winnow", "1.0.3"));
        assert!(sibs[0].1.is_none(), "the sibling came from the registry");

        // A fork that resolves from somewhere ELSE is not live, and the copy
        // that did resolve is the sibling.
        let (live, sibs) = classify(&graph, &paths, "winnow", Path::new("/w/forks/winnow"));
        assert!(!live);
        assert_eq!(sibs.len(), 2);
    }

    #[test]
    fn an_unknown_cell_is_could_not_run_not_a_policy_failure() {
        let Err(err) = run(&repo_root(), &["macos".to_string()]) else {
            panic!("an unknown cell must be could-not-run, never a verdict");
        };
        assert!(err.contains("mac-arm"), "the refusal must list the real cells: {err}");
    }

    /// EVERY cell resolves ONLY the forked version of each vendored crate.
    ///
    /// POLARITY, deliberately inverted 2026-08-25. This test used to assert
    /// that the linux cell DID carry an unpatched `winnow 1.0.3` beside the
    /// 0.7.15 fork, reached via `accesskit_winit → accesskit_unix → zbus →
    /// zbus_macros (proc-macro) → proc-macro-crate → toml_edit 0.25`. That was
    /// true when it was written; dropping the `a11y-accesskit` default feature
    /// removed `accesskit_unix`, the only edge pulling zbus, and with it the
    /// last unpatched sibling in any cell.
    ///
    /// A passing test must not require a defect to be PRESENT. Pinned that way,
    /// fixing the defect turns the suite red and the fix looks like a
    /// regression — precisely backwards. So this now asserts the CLEAN state,
    /// which is the property worth defending: the graph carries no unpatched
    /// sibling of any vendored fork, and if one reappears this reds.
    ///
    /// Nothing is lost on the detection side. The logic that FINDS a sibling is
    /// proved by `tests/red_fixtures.rs::an_unpatched_sibling_version_reds_the
    /// _forge_verb`, which plants a synthetic violation in a scratch tree and
    /// requires `cargo forge check` to go RED on it — a fixture, so it keeps
    /// working whatever the real graph does.
    #[test]
    fn no_cell_carries_an_unpatched_sibling_of_a_vendored_fork() {
        let root = repo_root();
        let (forks, _) = read_manifest_patches(&root).unwrap();
        assert!(!forks.is_empty(), "there is nothing to check if nothing is vendored");
        for cell in resolve::default_cells() {
            let mut log = String::new();
            let (graph, paths) = resolve::graph_and_paths(&root, &cell, &mut log)
                .unwrap_or_else(|e| panic!("cell `{}` must resolve offline: {e}", cell.name));
            let mut found: Vec<String> = Vec::new();
            let mut live = 0usize;
            for f in &forks {
                let (is_live, sibs) =
                    classify(&graph, &paths, &f.name, &canon(&root.join(&f.path)));
                live += usize::from(is_live);
                found.extend(sibs.iter().map(|s| s.0.spec()));
            }
            found.sort();
            let empty: [&str; 0] = [];
            assert_eq!(
                found, empty,
                "cell `{}` resolves an unpatched sibling beside a vendored fork — \
                 `cargo forge blame <name>` names the edge that drags it in",
                cell.name
            );
            assert!(live > 0, "cell `{}` reaches no fork at all — the patch table is dead \
                 there, and a cell with no forks would pass this vacuously", cell.name);
        }
    }

    /// The gate must report every obligation, and NAME any sibling the graphs
    /// actually carry — a report that finds one and prints a summary without it
    /// is worse than no gate. The expectation is DERIVED from the live graphs,
    /// so it neither requires a defect to exist (today none does, see
    /// `no_cell_carries_an_unpatched_sibling_of_a_vendored_fork`) nor goes
    /// quiet if one reappears.
    #[test]
    fn the_gate_reports_every_obligation_and_names_any_unpatched_sibling() {
        let root = repo_root();
        let (forks, _) = read_manifest_patches(&root).unwrap();
        let cells = resolve::default_cells();
        let mut expected: Vec<(String, String)> = Vec::new();
        for cell in &cells {
            let mut log = String::new();
            let Ok((graph, paths)) = resolve::graph_and_paths(&root, cell, &mut log) else {
                continue;
            };
            for f in &forks {
                let (_, sibs) = classify(&graph, &paths, &f.name, &canon(&root.join(&f.path)));
                expected.extend(sibs.iter().map(|s| (cell.name.clone(), s.0.spec())));
            }
        }
        let (ok, log) = check_report(&root);
        for (cell, spec) in &expected {
            assert!(
                log.contains(spec.as_str()),
                "[OB-3] must name the unpatched `{spec}` resolving in cell `{cell}`:\n{log}"
            );
        }
        if !expected.is_empty() {
            assert!(!ok, "a gate that found an unpatched sibling cannot be GREEN:\n{log}");
            assert!(log.contains(PRECISION_NOTE), "a RED report carries the precision note");
        }
        for tag in ["[OB-1..OB-10]", "[OB-11]", "[OB-12]", "[OB-13]", "[OB-14]"] {
            assert!(log.contains(tag), "every obligation must report; `{tag}` did not:\n{log}");
        }
    }
}
