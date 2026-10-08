// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified forge check`: the gate verb, wired as `xtask gate forge`.
//!
//! [`check_report`] answers whether aterm's third-party surface is still the
//! surface this repository declares, with no compilation and no network: one
//! `cargo tree` resolution per cell plus file reads, since the merge contract
//! runs it every time.
//!
//! # Why patch liveness matters
//!
//! An UNUSED `[patch.crates-io]` entry is a cargo WARNING at exit 0. It emits
//! nothing at all from `cargo metadata`, and `cargo build` stays green. So a
//! trust fork can be silently disabled — by an upstream major bump, by a new
//! dependency that requires the next major, by a mistyped path — while every
//! other gate in this repository keeps passing and the UNFIXED upstream code
//! compiles into the product.
//!
//! It happened here: the `linux` cell once resolved both the `vendor/winnow`
//! fork and an unpatched registry `winnow` a major ahead (through zbus →
//! proc-macro-crate → toml_edit), so the unfixed copy ran inside the compiler
//! on every Linux build and only `[OB-12]` said so; retiring `toml_edit`
//! removed that fork. `[OB-12]` stays armed because any major bump or
//! new dependency can shadow a remaining fork at cargo exit 0;
//! `tests/red_fixtures.rs` plants a violation to prove the detector bites.
//!
//! # The obligations
//!
//! Numbering continues [`crate::attest`]'s: `[OB-1]`..`[OB-10]` are attest's
//! and are reported here by delegation.
//!
//! | tag | obligation | source of truth |
//! |---|---|---|
//! | `[OB-1]`..`[OB-10]` | provenance, versions, license, NOTICE, markers, ignore rules | [`crate::attest::report`] |
//! | `[OB-11]` | every `[patch.crates-io]` path fork is REVIEWED | `aterm_census::scan_set::REVIEWED_VENDORED_CRATES` |
//! | `[OB-12]` | every fork is LIVE in the resolved graph, with no unpatched sibling | [`crate::resolve`] |
//! | `[OB-13]` | every CARVED path is still absent | `vendor/forge.toml` `[[carved]]` |
//! | `[OB-14]` | the measured surface conforms to its ratchet | `tools/forge-budget.tsv` |
//! | `[OB-15]` | no `[patch]` CAPTURES a differential oracle | `[dev-dependencies]` + `Cargo.lock` |
//! | `[OB-17]` | the fork ledger parses, names the measured cell matrix, and records exactly the vendored forks — path, version, license, §4(b) flag and census class | `vendor/forge.toml` `[forge]` + `[[fork]]` via [`crate::policy`] |
//!
//! # Not re-checked here
//!
//! A `REVIEWED_VENDORED_CRATES` row whose package left the patch table fails
//! the census's scan-set build (`crates/aterm-census/src/scan_set.rs`); a
//! `vendor/` directory no patch entry claims fails attest's `[OB-1]`.

use crate::model::{Cell, Graph, PkgId};
use crate::{Outcome, PRECISION_NOTE, attest, budget, policy, resolve};
use aterm_census::scan_set::{REVIEWED_VENDORED_CRATES, VendoredMode};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The policy file, which also carries the carve ledger (`[[carved]]` rows).
const POLICY_FILE: &str = "vendor/forge.toml";
/// The lower-only ratchet.
const BUDGET_FILE: &str = "tools/forge-budget.tsv";

/// What one `cargo tree` resolution yields: the graph, plus the directories
/// cargo disclosed for the packages that have one.
type Resolved = (Graph, BTreeMap<PkgId, PathBuf>);

/// A `[patch.crates-io]` entry redirecting a crates.io package at a local path:
/// the only patch shape this repository uses.
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

/// The gate's verdict. `could_not_run` is kept apart from `ok`:
/// [`check_report`] folds it into red, while [`run`] maps it onto exit code 3.
struct Verdict {
    ok: bool,
    log: String,
    could_not_run: Option<String>,
}

/// The gate entry point called by `crates/xtask/src/gate.rs`. Runs every
/// obligation over `root` across the cell matrix and returns
/// `(green, transcript)`.
///
/// Fail-closed: an unreadable manifest, an unresolvable cell or a missing
/// `cargo` all produce `false`.
pub fn check_report(root: &Path) -> (bool, String) {
    let v = report_over(root, &resolve::default_cells());
    (v.ok, v.log)
}

/// The verb behind `targo --unverified forge check [--cell NAME]...`.
///
/// `Err` is could-not-run (exit 3): a bad `--cell`, an unreadable workspace, a
/// cell cargo refused to resolve. A policy violation is
/// `Ok(Outcome { ok: false, .. })` (exit 1).
pub fn run(root: &Path, cells: &[String]) -> Result<Outcome, String> {
    let selected = resolve::select(&resolve::default_cells(), cells)?;
    let v = report_over(root, &selected);
    match v.could_not_run {
        // The reason first (`main` prefixes "could not run:"), then the
        // transcript of the obligations that did run.
        Some(why) => Err(format!(
            "{why}\n\nthe transcript up to that point:\n{}",
            v.log
        )),
        None => Ok(Outcome {
            ok: v.ok,
            log: v.log,
        }),
    }
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

fn report_over(root: &Path, cells: &[Cell]) -> Verdict {
    // Canonicalise once: cargo runs with `current_dir(root)` and
    // `--manifest-path <root>/Cargo.toml`, so a relative root would resolve
    // twice. `canon` falls back to the literal path for diagnostics.
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
        cells
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // -- [OB-1]..[OB-10] provenance, license, NOTICE, markers ---------------
    let _ = writeln!(
        log,
        "  [OB-1..OB-10] PROVENANCE, LICENSE & NOTICE — delegated to `targo --unverified forge attest`:"
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
             `targo --unverified forge attest` and clear each obligation it names — the vendored copies are \
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

    // -- [OB-11] every vendored path fork is reviewed ------------------------
    //
    // Only third-party source needs a REVIEWED_VENDORED_CRATES row; a patch
    // pointing at `crates/` is a workspace member. The classification lives in
    // `aterm_census::scan_set::classify_patch_target` so check, attest and the
    // census agree. First-party targets must still exist on disk and be live
    // per cell ([OB-12]).
    let _ = writeln!(
        log,
        "  [OB-11] REVIEW REGISTRATION — every VENDORED `[patch.crates-io]` path fork has a \
         REVIEWED_VENDORED_CRATES row (first-party targets under crates/ are named, not \
         registered — they are not third-party code):"
    );
    let mut missing_on_disk = false;
    for e in &patches {
        let first_party = matches!(
            aterm_census::scan_set::classify_patch_target(&e.name, &e.path, root),
            Ok(aterm_census::scan_set::PatchTargetKind::FirstParty)
        );
        if first_party {
            match REVIEWED_VENDORED_CRATES
                .iter()
                .find(|r| r.package == e.name)
            {
                Some(r) => {
                    fails += 1;
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-11] `{}` is patched to first-party `{}` but ALSO carries \
                         a REVIEWED_VENDORED_CRATES row (registered at `{}`) — that registry \
                         records third-party source this repository redistributes and must \
                         keep reviewing, and a workspace member is neither. A false \
                         provenance claim is as wrong as a missing one. Fix: delete the row \
                         from crates/aterm-census/src/scan_set.rs.",
                        e.name, e.path, r.path
                    );
                }
                None => {
                    let _ = writeln!(
                        log,
                        "    ✓ {} → {} (first-party workspace member; no review row owed)",
                        e.name, e.path
                    );
                }
            }
            if !root.join(&e.path).join("Cargo.toml").is_file() {
                fails += 1;
                missing_on_disk = true;
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-11] `{}` is patched to `{}`, which has no Cargo.toml on \
                     disk — cargo cannot resolve this workspace at all. Fix: restore the \
                     crate, or delete the `[patch.crates-io].{}` line.",
                    e.name, e.path, e.name
                );
            }
            continue;
        }
        match REVIEWED_VENDORED_CRATES
            .iter()
            .find(|r| r.package == e.name)
        {
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
                let _ = writeln!(
                    log,
                    "    ✓ {} → {} ({})",
                    e.name,
                    e.path,
                    mode_label(&r.mode)
                );
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
        let _ = writeln!(
            log,
            "    (no path forks declared in {})",
            manifest.display()
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
                    live_in
                        .entry(e.name.as_str())
                        .or_default()
                        .push(cell.name.clone());
                }
                for sib in &siblings {
                    fails += 1;
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-12] cell `{}` ({}) resolves an UNPATCHED `{}` alongside \
                         the fork: `{}` comes from {}, not from `{}`, so the fork's fix is \
                         ABSENT from the copy that compiles. Fix: run `targo tree --target {} \
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

    // Absence is judged across cells: a fork live nowhere is a dead patch; one
    // live in some cells is a platform fact. Counted for the green verdict.
    let (mut live_all, mut live_some, mut live_build) = (0usize, 0usize, 0usize);
    if !missing_on_disk {
        let mut build_probe: Option<Result<Resolved, String>> = None;
        for e in &patches {
            let live = live_in.get(e.name.as_str()).map_or(0, Vec::len);
            if live == cells.len() && !cells.is_empty() {
                live_all += 1;
                let _ = writeln!(log, "    ✓ {} live in all {} cell(s)", e.name, cells.len());
                continue;
            }
            if live > 0 {
                live_some += 1;
                notes += 1;
                let _ = writeln!(
                    log,
                    "    • NOTE [OB-12] `{}` is live in {} of {} cell(s) ({}) — a fork only some \
                     targets pull in. Not a failure; recorded so a SHRINKING cell set is \
                     visible.",
                    e.name,
                    live,
                    cells.len(),
                    live_in
                        .get(e.name.as_str())
                        .map_or(String::new(), |v| v.join(", "))
                );
                continue;
            }
            // Live in no cell's `--edges normal` graph. A build-time-only fork
            // (pkg-config) is expected there, so prove it in the build closure.
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
                    if blive {
                        live_build += 1;
                    }
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
                             nothing else in this repository would notice. Fix: run `targo tree \
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
                if c.reason.is_empty() {
                    "(none recorded)"
                } else {
                    c.reason.as_str()
                }
            );
        } else {
            let _ = writeln!(log, "    ✓ {} still absent", c.path);
        }
    }

    // -- [OB-14] the ratchet --------------------------------------------------
    let _ = writeln!(
        log,
        "  [OB-14] RATCHET — the measured surface conforms to {BUDGET_FILE}:"
    );
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
                         — if the growth is deliberate — run `targo --unverified forge budget --update \
                         --allow-regress \"<reason of 80+ characters>\"`, which records the reason \
                         in {BUDGET_FILE}."
                    );
                }
            }
            Err(e) => {
                fails += 1;
                could_not_run.get_or_insert(e.clone());
                let _ = writeln!(
                    log,
                    "  ✗ FAIL [OB-14] the ratchet could not be evaluated: {e}"
                );
            }
        }
    } else {
        notes += 1;
        let _ = writeln!(
            log,
            "    • NOTE [OB-14] no ratchet rows yet ({BUDGET_FILE} absent) — this surface is \
             MEASURED but not yet RATCHETED, so nothing here stops it growing. Fix: run \
             `targo --unverified forge budget --update` to seed the ceilings from today's measurement."
        );
    }

    // -- [OB-15] the oracle trap ---------------------------------------------
    let _ = writeln!(
        log,
        "  [OB-15] ORACLE CAPTURE — no `[patch.crates-io]` entry silently redirects a \
         `[dev-dependencies]` differential oracle at the implementation it exists to check:"
    );
    {
        let mut judged = 0usize;
        for e in &patches {
            let oracles = dev_dependency_oracles(root, &e.name);
            if oracles.is_empty() {
                continue;
            }
            judged += 1;
            let lock = match attest::lock_entries(root, &e.name) {
                Ok(v) => v,
                Err(err) => {
                    fails += 1;
                    could_not_run.get_or_insert(err.clone());
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-15] `{}` is patched AND held as a dev-dep oracle, but \
                         Cargo.lock could not be read, so capture cannot be decided.\n    {err}",
                        e.name
                    );
                    continue;
                }
            };
            match oracle_verdict(&lock) {
                OracleVerdict::Escapes(sibling) => {
                    notes += 1;
                    let holders: Vec<String> = oracles
                        .iter()
                        .map(|(m, r)| format!("{m} pins `{r}`"))
                        .collect();
                    let _ = writeln!(
                        log,
                        "    • NOTE [OB-15] `{}` is patched AND is a differential oracle, and \
                         the oracle ESCAPES: the lock still carries registry {} {sibling}, so \
                         the oracle compares against real upstream. Held by: {}. This is the \
                         deliberate version-pin escape — it costs the oracle testing a \
                         DIFFERENT upstream version than the one replaced, and it survives only \
                         while the pin stays unsatisfiable by the shim.",
                        e.name,
                        e.name,
                        holders.join("; ")
                    );
                }
                OracleVerdict::Captured => {
                    fails += 1;
                    let holders: Vec<String> = oracles
                        .iter()
                        .map(|(m, r)| format!("{m} (`{r}`)"))
                        .collect();
                    let _ = writeln!(
                        log,
                        "  ✗ FAIL [OB-15] `{}` is patched to `{}` AND is kept as a \
                         `[dev-dependencies]` differential ORACLE by {}. `[patch.crates-io]` \
                         applies to dev-dependencies too, and Cargo.lock carries NO \
                         registry-sourced `{}` — so the oracle now compares the implementation \
                         against ITSELF. It will go on passing and stop meaning anything. Fix: \
                         drop the patch, or pin the oracle at a version the shim cannot satisfy \
                         (the escape `[OB-15]` reports as a NOTE) so a real registry copy stays \
                         in the lock.",
                        e.name,
                        e.path,
                        holders.join(", "),
                        e.name
                    );
                }
            }
        }
        if judged == 0 {
            let _ = writeln!(
                log,
                "    OK — no patched package is held as a dev-dependency oracle ({} patch \
                 entries examined).",
                patches.len()
            );
        }
    }

    // -- [OB-17] the fork ledger ----------------------------------------------
    let _ = writeln!(
        log,
        "  [OB-17] FORK LEDGER — {POLICY_FILE} parses fail-closed, names the cell matrix forge \
         measures, and records every vendored fork as the tree has it:"
    );
    let ledger = ledger_findings(root);
    for why in &ledger {
        let _ = writeln!(log, "  ✗ FAIL [OB-17] {why}");
    }
    fails += ledger.len();
    if ledger.is_empty() {
        let _ = writeln!(
            log,
            "    OK — header and fork blocks agree with default_cells(), [patch.crates-io], \
             [OB-7]'s license reading and REVIEWED_VENDORED_CRATES."
        );
    }

    // -- verdict --------------------------------------------------------------
    // Forks (third-party, under review) and first-party targets (workspace
    // members) are counted separately.
    let first_party_count = patches
        .iter()
        .filter(|e| {
            matches!(
                aterm_census::scan_set::classify_patch_target(&e.name, &e.path, root),
                Ok(aterm_census::scan_set::PatchTargetKind::FirstParty)
            )
        })
        .count();
    if fails == 0 {
        let _ = writeln!(
            log,
            "{}",
            green_verdict(&GreenCounts {
                forks: patches.len() - first_party_count,
                first_party: first_party_count,
                cells: cells.len(),
                live_all,
                live_some,
                live_build,
                carved: carved.len(),
                notes,
            })
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
    Verdict {
        ok: fails == 0,
        log,
        could_not_run,
    }
}

// ---------------------------------------------------------------------------
// Pieces
// ---------------------------------------------------------------------------

/// What a green `gate forge` run measured, for [`green_verdict`].
struct GreenCounts {
    /// Vendored forks (third-party source under review).
    forks: usize,
    /// First-party patch targets (workspace members).
    first_party: usize,
    cells: usize,
    /// Patch entries live in every cell's normal graph.
    live_all: usize,
    /// Live in some cells' normal graphs and absent from others.
    live_some: usize,
    /// Live in no normal graph, only in the first cell's build closure.
    live_build: usize,
    carved: usize,
    notes: usize,
}

/// The sentence a green run ends on: where the patch targets are live, as
/// measured.
fn green_verdict(c: &GreenCounts) -> String {
    format!(
        "gate forge: GREEN — {} vendored fork(s) + {} first-party patch target(s): {} live in \
         all {} cells, {} in some, {} build-only; no unpatched sibling; {} carved path(s) \
         absent; provenance attested; {} note(s).",
        c.forks, c.first_party, c.live_all, c.cells, c.live_some, c.live_build, c.carved, c.notes
    )
}

/// `[OB-17]`: one line per way the fork ledger disagrees with the tree.
///
/// The ledger header must be [`resolve::default_cells`]; its `[[fork]]` blocks
/// must be exactly the vendored `[patch.crates-io]` entries, with the path,
/// version and license their manifests carry, the §4(b) flag `[OB-7]` derives,
/// and the census class REVIEWED_VENDORED_CRATES gives. An absent ledger is a
/// finding only once a vendored fork exists.
fn ledger_findings(root: &Path) -> Vec<String> {
    let vendored: Vec<policy::PatchEntry> = match policy::patch_entries(root) {
        Ok(all) => all
            .into_iter()
            .filter(policy::PatchEntry::is_vendored)
            .collect(),
        Err(e) => return vec![e],
    };
    if !root.join(POLICY_FILE).is_file() {
        return if vendored.is_empty() {
            Vec::new()
        } else {
            vec![format!(
                "{POLICY_FILE} is missing, but {} vendored fork(s) are patched in — the ledger \
                 is where their redistribution obligations are recorded. Fix: restore it, one \
                 `[[fork]]` block per vendored `[patch.crates-io]` entry.",
                vendored.len()
            )]
        };
    }
    let ledger = match policy::load(root) {
        Ok(p) => p,
        Err(e) => return vec![e],
    };
    let mut out = Vec::new();
    let measured = resolve::default_cells();
    if ledger.forge.cells != measured {
        let names = |cells: &[Cell]| {
            cells
                .iter()
                .map(|c| format!("{} ({}, {})", c.name, c.triple, c.package))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push(format!(
            "{POLICY_FILE}'s [forge] cells name [{}], but forge measures [{}]. A ledger row \
             nothing measures reads as an audited cell and is not one. Fix: make the block \
             equal `resolve::default_cells()`, or fix default_cells.",
            names(&ledger.forge.cells),
            names(&measured)
        ));
    }
    for p in &vendored {
        let Some(f) = ledger.forks.iter().find(|f| f.name == p.name) else {
            out.push(format!(
                "vendored fork `{}` ({}) has no `[[fork]]` block in {POLICY_FILE} — its \
                 redistribution obligations are recorded nowhere. Fix: add the block, with the \
                 reason it exists in the comment above it.",
                p.name, p.path
            ));
            continue;
        };
        let tree = (
            p.path.as_str(),
            p.manifest_version.as_str(),
            p.license.as_str(),
        );
        let recorded = (f.path.as_str(), f.version.as_str(), f.license.as_str());
        if recorded != tree {
            out.push(format!(
                "`[[fork]] {}` records path/version/license {recorded:?}, but the tree has \
                 {tree:?}. Fix: update the block to match the vendored manifest — the ledger is \
                 the record of what is redistributed, so it may not describe another copy.",
                p.name
            ));
        }
        // `apache_notice` says whether §4(b) notices are OWED; `[OB-7]` decides that
        // from the license alone, so the flag may only ever agree with it.
        let owed = attest::apache_is_mandatory(&p.license);
        if f.apache_notice != owed {
            out.push(format!(
                "`[[fork]] {}` records apache_notice = {}, but its license `{}` makes the \
                 Apache-2.0 §4(b) notices {} — which is what `[OB-7]` enforces. Fix: set \
                 apache_notice = {owed}.",
                p.name,
                f.apache_notice,
                p.license,
                if owed {
                    "OWED"
                } else {
                    "NOT owed (a non-Apache arm is available)"
                }
            ));
        }
        // The census classification is REVIEWED_VENDORED_CRATES's; a fork with no
        // row there is `[OB-11]`'s finding, so only a row that exists is compared.
        if let Some(reviewed) = REVIEWED_VENDORED_CRATES
            .iter()
            .find(|v| v.package == p.name)
        {
            let census = match &reviewed.mode {
                VendoredMode::Scanned { namespace, .. } => {
                    (policy::CensusMode::Scanned, Some(*namespace))
                }
                VendoredMode::BuildDepOnly { .. } => (policy::CensusMode::BuildDepOnly, None),
            };
            let recorded = (f.census_mode, f.census_namespace.as_deref());
            if recorded != census {
                out.push(format!(
                    "`[[fork]] {}` records census mode/namespace {recorded:?}, but \
                     REVIEWED_VENDORED_CRATES classifies it {census:?}. Fix: copy the \
                     registry's classification — the registry is the one the census runs on.",
                    p.name
                ));
            }
        }
    }
    for f in &ledger.forks {
        if !vendored.iter().any(|p| p.name == f.name) {
            out.push(format!(
                "`[[fork]] {}` ({}) records a fork no `[patch.crates-io]` entry vendors. Fix: \
                 delete the stale block.",
                f.name, f.path
            ));
        }
    }
    out
}

/// Split the nodes named `name` into "the fork is live" and the unpatched
/// siblings: any node of that name that is not the declared path package.
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
            "cannot read {}: {e} — `targo --unverified forge check` must run against a workspace root. Fix: \
             pass `--root <workspace dir>`.",
            manifest.display()
        )
    })?;
    parse_patch_table(&text, &manifest)
}

fn parse_patch_table(text: &str, whence: &Path) -> Result<(Vec<PatchEntry>, Vec<String>), String> {
    let doc = text.parse::<aterm_toml::edit::DocumentMut>().map_err(|e| {
        format!(
            "{} is not valid TOML: {e} — fix the manifest, then re-run.",
            whence.display()
        )
    })?;
    let Some(table) = doc
        .get("patch")
        .and_then(|p| p.get("crates-io"))
        .and_then(aterm_toml::edit::Item::as_table_like)
    else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut forks = Vec::new();
    let mut other = Vec::new();
    for (name, item) in table.iter() {
        match item.get("path").and_then(aterm_toml::edit::Item::as_str) {
            Some(path) => {
                forks.push(PatchEntry {
                    name: name.to_string(),
                    path: path.to_string(),
                });
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
    let doc = text.parse::<aterm_toml::edit::DocumentMut>().map_err(|e| {
        format!(
            "{} is not valid TOML: {e} — fix the policy file, then re-run.",
            whence.display()
        )
    })?;
    let Some(item) = doc.get("carved") else {
        return Ok(Vec::new());
    };

    // `[[carved]]` array-of-tables is the written form; an inline array of
    // tables is accepted too.
    let mut rows: Vec<(Option<String>, String)> = Vec::new();
    if let Some(tables) = item.as_array_of_tables() {
        for t in tables {
            rows.push((
                t.get("path")
                    .and_then(aterm_toml::edit::Item::as_str)
                    .map(ToString::to_string),
                t.get("reason")
                    .and_then(aterm_toml::edit::Item::as_str)
                    .unwrap_or_default()
                    .to_string(),
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
                t.get("path")
                    .and_then(aterm_toml::edit::Value::as_str)
                    .map(ToString::to_string),
                t.get("reason")
                    .and_then(aterm_toml::edit::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
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
/// Used only to decide whether a fork absent from every shipped graph is alive
/// as a build-time dependency (`pkg-config`) or is a dead patch. Without
/// `--no-dedupe`: only package identity is needed, and the deduped node set is
/// the same at a fraction of the output.
fn build_closure(root: &Path, cell: &Cell) -> Result<Resolved, String> {
    // `CARGO` is set by targo itself, so a nested invocation uses the toolchain
    // that launched forge; unset, it is Trust's `targo`, never a stock `cargo`.
    let exe = std::env::var_os("CARGO").unwrap_or_else(|| "targo".into());
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
            "could not execute `targo tree`: {e} — install the Trust toolchain (`aterm pkg \
             install trust`), or set CARGO to the targo to use"
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

/// Canonicalise for comparison, falling back to the literal path when the
/// filesystem cannot.
fn canon(p: &Path) -> PathBuf {
    crate::resolve::strip_verbatim_prefix(
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()),
    )
}

/// What the lock says about a patched package that is also a differential
/// oracle. Only a registry-sourced entry beside the source-less patched one
/// keeps the oracle pointed at real upstream.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OracleVerdict {
    /// A registry sibling survives at this version — the oracle still compares
    /// against upstream.
    Escapes(String),
    /// Every entry is the patch. The oracle compares the shim to itself.
    Captured,
}

/// Decide capture from `Cargo.lock`'s entries for one name, as
/// `(version, has_source)` from [`attest::lock_entries`]. Not a semver
/// computation: cargo already decided it when writing the lock.
fn oracle_verdict(lock: &[(String, bool)]) -> OracleVerdict {
    match lock.iter().find(|(_, has_source)| *has_source) {
        Some((version, _)) => OracleVerdict::Escapes(version.clone()),
        None => OracleVerdict::Captured,
    }
}

/// Every workspace manifest that keeps `name` as a `[dev-dependencies]` entry
/// stating a registry version requirement, as `(manifest, requirement)`.
///
/// A dev-dependency is how a retired crate is held as a differential oracle.
/// Path, git and `workspace = true` entries are EXCLUDED and the exclusion is
/// load-bearing: they state no registry requirement, so there is no upstream
/// for a patch to capture. `aterm-gui`'s `winit = { workspace = true }`
/// dev-dep is the live example — it inherits the patched fork on purpose and
/// is not an oracle.
fn dev_dependency_oracles(root: &Path, name: &str) -> Vec<(String, String)> {
    use aterm_toml::edit::{DocumentMut, Item, TableLike};

    let mut manifests = vec![root.join("Cargo.toml")];
    if let Ok(entries) = std::fs::read_dir(root.join("crates")) {
        let mut members: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("Cargo.toml"))
            .filter(|p| p.is_file())
            .collect();
        members.sort();
        manifests.extend(members);
    }

    let mut out = Vec::new();
    for manifest in manifests {
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(doc) = text.parse::<DocumentMut>() else {
            continue;
        };

        // `[dev-dependencies]`, plus every `[target.'cfg(..)'.dev-dependencies]`
        // — a target-gated oracle is still an oracle on the cells it applies to.
        let mut tables: Vec<&dyn TableLike> = Vec::new();
        if let Some(t) = doc.get("dev-dependencies").and_then(Item::as_table_like) {
            tables.push(t);
        }
        if let Some(targets) = doc.get("target").and_then(Item::as_table_like) {
            for (_, gated) in targets.iter() {
                if let Some(t) = gated
                    .as_table_like()
                    .and_then(|g| g.get("dev-dependencies"))
                    .and_then(Item::as_table_like)
                {
                    tables.push(t);
                }
            }
        }

        for table in tables {
            // Iterate the WHOLE table rather than looking `name` up directly.
            // A dev-dependency key is not the package name when the entry
            // renames it (`arrayvec_upstream = { package = "arrayvec", .. }`);
            // a direct `table.get("arrayvec")` would miss that and report "no
            // oracle, safe to patch".
            for (key, entry) in table.iter() {
                let declared = entry
                    .as_table_like()
                    .and_then(|t| t.get("package"))
                    .and_then(Item::as_str)
                    .unwrap_or(key);
                if declared != name {
                    continue;
                }
                if let Some(req) = entry.as_str() {
                    out.push((rel(root, &manifest), req.to_string()));
                    continue;
                }
                let Some(entry) = entry.as_table_like() else {
                    continue;
                };
                if entry.get("path").is_some()
                    || entry.get("git").is_some()
                    || entry.get("workspace").is_some()
                {
                    continue;
                }
                if let Some(req) = entry.get("version").and_then(Item::as_str) {
                    out.push((rel(root, &manifest), req.to_string()));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A manifest path relative to the repo root, for a message a reader can act on.
fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::repo_root;

    /// Partition the real patch table the way [OB-11] does.
    fn real_patches() -> (Vec<PatchEntry>, Vec<PatchEntry>) {
        let (forks, _) = read_manifest_patches(&repo_root()).expect("root manifest reads");
        forks.into_iter().partition(|f| {
            !matches!(
                aterm_census::scan_set::classify_patch_target(&f.name, &f.path, &repo_root()),
                Ok(aterm_census::scan_set::PatchTargetKind::FirstParty)
            )
        })
    }

    /// The green sentence says where the patch targets are live, as measured.
    #[test]
    fn the_green_verdict_counts_partial_and_build_only_forks_apart() {
        let v = green_verdict(&GreenCounts {
            forks: 7,
            first_party: 2,
            cells: 8,
            live_all: 7,
            live_some: 1,
            live_build: 1,
            carved: 3,
            notes: 1,
        });
        assert_eq!(
            v,
            "gate forge: GREEN — 7 vendored fork(s) + 2 first-party patch target(s): 7 live in \
             all 8 cells, 1 in some, 1 build-only; no unpatched sibling; 3 carved path(s) \
             absent; provenance attested; 1 note(s)."
        );
        assert!(!v.contains("all live"), "{v}");
    }

    /// The patch table is read as path patches, partitioned. The
    /// `vendor/<name>` path shape is asserted for the vendored arm only.
    #[test]
    fn the_root_patch_table_is_read_as_path_forks() {
        let (_, other) = read_manifest_patches(&repo_root()).expect("root manifest reads");
        assert!(
            other.is_empty(),
            "this repo patches only path forks: {other:?}"
        );
        let (vendored, first_party) = real_patches();
        let names: Vec<&str> = vendored.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["indexmap", "libm", "pkg-config", "smol_str", "winit"]
        );
        for f in &vendored {
            assert_eq!(
                f.path,
                format!("vendor/{}", f.name),
                "declared path for {}",
                f.name
            );
        }
        let fp: Vec<(&str, &str)> = first_party
            .iter()
            .map(|f| (f.name.as_str(), f.path.as_str()))
            .collect();
        assert_eq!(
            fp,
            [
                ("arrayvec", "crates/aterm-arrayvec"),
                ("cfg-if", "crates/aterm-cfg-if"),
                ("core_maths", "crates/aterm-core-maths"),
                ("libc", "crates/aterm-libc"),
                ("log", "crates/aterm-log-shim"),
                ("once_cell", "crates/aterm-once-cell"),
                ("profiling", "crates/aterm-profiling"),
                ("tracing", "crates/aterm-tracing"),
            ]
        );
    }

    #[test]
    fn every_vendored_path_fork_in_this_repo_is_registered_for_review() {
        let (vendored, _) = real_patches();
        for f in &vendored {
            let row = REVIEWED_VENDORED_CRATES
                .iter()
                .find(|r| r.package == f.name)
                .unwrap_or_else(|| panic!("`{}` has no REVIEWED_VENDORED_CRATES row", f.name));
            assert_eq!(row.path, f.path, "reviewed path for {}", f.name);
        }
    }

    /// The other direction, and the one this change is FOR: a first-party
    /// patch target must NOT be registered as a reviewed vendored fork. The
    /// registry is the repository's record of third-party code it owes a
    /// standing review; putting our own crate in it would be a false entry
    /// that no later reader could tell from a true one.
    #[test]
    fn no_first_party_patch_target_is_registered_as_a_vendored_fork() {
        let (_, first_party) = real_patches();
        assert!(
            !first_party.is_empty(),
            "the partition is vacuous if there is no first-party target to test it on"
        );
        for f in &first_party {
            assert!(
                REVIEWED_VENDORED_CRATES.iter().all(|r| r.package != f.name),
                "`{}` is a workspace member and must not carry a REVIEWED_VENDORED_CRATES row",
                f.name
            );
            assert!(
                repo_root().join(&f.path).join("Cargo.toml").is_file(),
                "`{}` is patched to `{}`, which must exist on disk",
                f.name,
                f.path
            );
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
        assert_eq!(
            forks[0],
            PatchEntry {
                name: "alpha".into(),
                path: "vendor/alpha".into()
            }
        );
        assert_eq!(other, ["beta", "gamma"]);
    }

    #[test]
    fn a_manifest_with_no_patch_table_yields_no_forks() {
        let (forks, other) =
            parse_patch_table("[package]\nname = \"x\"\n", Path::new("x")).unwrap();
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
        assert!(
            err.contains("path = "),
            "the refusal must say what to type: {err}"
        );
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
        assert!(
            err.contains("mac-arm"),
            "the refusal must list the real cells: {err}"
        );
    }

    // -- [OB-15] the oracle trap ---------------------------------------------

    /// The rule itself, over the only input that decides it. A registry sibling
    /// in the lock is what keeps an oracle honest; its ABSENCE is capture.
    #[test]
    fn a_registry_sibling_is_what_separates_an_escaped_oracle_from_a_captured_one() {
        // The live `arrayvec` shape: the fork is source-less, and a registry
        // copy survives at the version the oracle pins.
        let escaped = vec![("0.7.8".to_owned(), false), ("0.7.7".to_owned(), true)];
        assert_eq!(
            oracle_verdict(&escaped),
            OracleVerdict::Escapes("0.7.7".to_owned())
        );

        // ARMED: delete the registry sibling — the same oracle is now comparing
        // the shim against itself, and the verdict must flip.
        let captured = vec![("0.7.8".to_owned(), false)];
        assert_eq!(oracle_verdict(&captured), OracleVerdict::Captured);

        // A package with no lock entries at all cannot be shown to escape.
        assert_eq!(oracle_verdict(&[]), OracleVerdict::Captured);
    }

    /// The oracle SCANNER finds holders, including a RENAMED one: a scanner
    /// that keyed on the manifest KEY instead of the `package` field would miss
    /// `arrayvec_upstream = { package = "arrayvec", version = "=0.7.7" }` and
    /// report "no oracle, safe to patch".
    #[test]
    fn the_oracle_scanner_finds_plain_and_renamed_holders() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("root manifest");
        for (name, body) in [
            (
                "renamed",
                "[dev-dependencies]\narrayvec_upstream = { package = \"arrayvec\", version = \"=0.7.7\" }\n",
            ),
            (
                "plain",
                "[target.'cfg(unix)'.dev-dependencies]\nmemchr = \"2\"\n",
            ),
            (
                "inherited",
                "[dev-dependencies]\narrayvec = { workspace = true }\nmemchr = { path = \"x\" }\n",
            ),
        ] {
            std::fs::create_dir_all(root.join("crates").join(name)).expect("crate dir");
            std::fs::write(root.join("crates").join(name).join("Cargo.toml"), body)
                .expect("member manifest");
        }
        let arrayvec = dev_dependency_oracles(root, "arrayvec");
        assert!(
            arrayvec.len() == 1 && arrayvec[0].0.contains("renamed") && arrayvec[0].1 == "=0.7.7",
            "the RENAMED oracle, and only it, must be found: {arrayvec:?}"
        );
        let memchr = dev_dependency_oracles(root, "memchr");
        assert!(
            memchr.len() == 1 && memchr[0].0.contains("plain") && memchr[0].1 == "2",
            "the target-gated oracle, and only it, must be found: {memchr:?}"
        );
    }

    /// A `workspace = true` dev-dependency is not an oracle: `aterm-gui`'s
    /// `winit = { workspace = true, features = ["aterm-test-key-event"] }`
    /// inherits the patched fork on purpose, and counting it would fail
    /// `[OB-15]` forever.
    #[test]
    fn an_inherited_dev_dependency_is_not_an_oracle() {
        let root = repo_root();
        let holders = dev_dependency_oracles(&root, "winit");
        assert!(
            holders.is_empty(),
            "winit's dev-dep is `workspace = true` (the patched fork), not a registry \
             oracle, but the scanner claimed: {holders:?}"
        );
    }

    /// Every patched package that is an oracle escapes capture today.
    #[test]
    fn no_patch_in_this_repo_currently_captures_an_oracle() {
        let root = repo_root();
        let (forks, _) = read_manifest_patches(&root).expect("root manifest reads");
        for e in &forks {
            let holders = dev_dependency_oracles(&root, &e.name);
            if holders.is_empty() {
                continue;
            }
            let lock = attest::lock_entries(&root, &e.name).expect("Cargo.lock reads");
            assert!(
                matches!(oracle_verdict(&lock), OracleVerdict::Escapes(_)),
                "`{}` is patched to `{}` and held as an oracle by {holders:?}, but no \
                 registry-sourced entry survives in Cargo.lock — the oracle is CAPTURED",
                e.name,
                e.path
            );
        }
    }

    /// The wgpu differential oracle is an optional normal dependency of
    /// aterm-gpu behind `wgpu-oracle` (activated by a self-dev-dependency), a
    /// shape `dev_dependency_oracles` cannot see. Until the scanner learns
    /// feature-activated oracles, this refuses a `[patch.crates-io] wgpu` row
    /// directly.
    #[test]
    fn the_wgpu_oracle_rides_a_shape_ob15_cannot_see_so_the_patch_row_is_pinned_shut() {
        let root = repo_root();
        let (forks, _) = read_manifest_patches(&root).expect("root manifest reads");
        assert!(
            !forks.iter().any(|e| e.name == "wgpu"),
            "[patch.crates-io] carries a `wgpu` row. The wgpu differential \
             oracle is a feature-activated optional dep (aterm-gpu's \
             `wgpu-oracle`, enabled by its self-dev-dependency) — OB-15's \
             dev-dep scanner cannot see it, so this patch would point the \
             oracle at the implementation it exists to check and NOTHING \
             would go red. Teach `dev_dependency_oracles` the \
             feature-activated shape and arm it before removing this pin."
        );
    }
}
