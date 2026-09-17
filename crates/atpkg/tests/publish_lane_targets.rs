// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! PUBLISH-LANE TARGET COVERAGE: a lane that ships an OS must ship every triple this
//! client can ask that OS for.
//!
//! THE CLASS THIS GUARDS. [`atpkg::TARGETS`] is the set of triples a program may carry
//! `[[artifact]]` rows for, and a client picks the row naming `cli::current_triple`. A
//! program with NO row for the running triple is the canonical `unavailable on <target>`
//! state — a CLEAN, fail-closed, silent skip (§6). That is the right behaviour for a
//! member nobody has built yet, and it is exactly what makes the defect below invisible:
//! a publish lane that can only ever produce ONE triple does not error, does not warn,
//! and reports "done" on the builder while an entire architecture sits unserved forever.
//! Nothing on the builder's own machine can see it.
//!
//! THE LIVE INSTANCE, found 2026-09-16. `tools/linux-auto-atpkg.sh` — the whole atpkg
//! side of the Linux lane — carried ONE Linux triple per run (first the literal
//! `x86_64-unknown-linux-gnu`, then, from 2026-09-16, the builder's own host triple) and
//! spelled that scalar into every decision, pack, staged asset, manifest-row check and
//! both uploads. `aarch64-unknown-linux-gnu` has been in `TARGETS` and in
//! `current_triple`'s `cfg` ladder the whole time, and the fleet has exactly ONE Linux
//! builder (m17-tower, x86_64) — so an arm64 Linux box asked for a row nothing could
//! produce, and the lane's decision table never printed the words even once.
//!
//! THE LAW, machine-checked here. For every row of [`LANES`]:
//!
//!   1. the lane declares its triples as a LIST (`<VAR>=(`), never a scalar;
//!   2. it FILLS that list from the shared shipped-target list rather than retyping one:
//!      `ATPKG_TARGETS_ALL` in `tools/atpkg-publish-lib.sh` is the shell's copy of
//!      [`atpkg::TARGETS`], and this test holds the two equal for the lane's own OS, with
//!      more than one triple in it. Adding a Linux triple to `TARGETS` therefore reaches
//!      the lane by itself — and fails this test until the shell copy follows;
//!   3. no other line of the lane NAMES one of those triples literally outside a comment.
//!      Rule 3 is the one that keeps rules 1 and 2 honest: a loop over the list means
//!      nothing if some later step still spells one triple into a path, an asset name or
//!      an upload.
//!
//! WHY IN `cargo test -p atpkg`. `tools/test-linux-auto-atpkg.sh` is the behavioural twin
//! — it RUNS the lane against a stubbed channel and reads its decision table on both host
//! architectures. This file is the cheap standing check that rides along with the crate's
//! own tests on every target and every box, needs no bash, no stubs and no subprocess, and
//! is the half that notices when the CLIENT's target list grows. Hermetic: reads committed
//! files under `CARGO_MANIFEST_DIR` only.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One shipping lane: the script, the `TARGETS` OS substring it is responsible for, and
/// the shell array it must declare. Add a row when a lane starts shipping an OS — the
/// rows are what makes rule 2 bite on the next triple anyone adds to `TARGETS`.
const LANES: &[(&str, &str, &str)] = &[("tools/linux-auto-atpkg.sh", "-linux-", "LINUX_TRIPLES")];

/// The shell's copy of [`atpkg::TARGETS`], and the variable the lanes derive from.
const SHARED_LIST: (&str, &str) = ("tools/atpkg-publish-lib.sh", "ATPKG_TARGETS_ALL");

fn repo_root() -> PathBuf {
    // …/crates/atpkg -> …/crates -> …
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/atpkg must sit two levels under the workspace root")
        .to_path_buf();
    assert!(
        root.join("tools").is_dir(),
        "repo root {} has no tools/ — if the publish lanes moved, update \
         crates/atpkg/tests/publish_lane_targets.rs to follow them",
        root.display()
    );
    root
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {} ({e})", path.display()))
}

/// `ATPKG_TARGETS_ALL="a b c"` as the shell sees it: one line, one double-quoted word list.
fn shared_targets() -> Vec<String> {
    let (script, var) = SHARED_LIST;
    let src = read(script);
    let open = format!("{var}=\"");
    let line = src
        .lines()
        .find(|l| l.starts_with(&open))
        .unwrap_or_else(|| panic!("{script} declares no `{var}=\"…\"` at the start of a line"));
    line.strip_prefix(&open)
        .and_then(|r| r.split_once('"'))
        .map(|(inner, _)| inner.split_whitespace().map(str::to_string).collect())
        .unwrap_or_else(|| panic!("{script}: `{var}=\"` is not closed on its own line: {line}"))
}

/// (1) + (2): the lane's list is a LIST, it is filled from the shared target list, and
/// that shared list is this client's own `TARGETS` for the lane's OS.
#[test]
fn every_shipping_lane_covers_its_whole_target_family() {
    let (shared_script, shared_var) = SHARED_LIST;
    let shared = shared_targets();
    for (script, os, var) in LANES {
        let src = read(script);

        let want: BTreeSet<&str> = atpkg::TARGETS
            .iter()
            .copied()
            .filter(|t| t.contains(os))
            .collect();
        assert!(
            want.len() >= 2,
            "TARGETS carries {} triple(s) matching {os:?}; this test only says something \
             when an OS is served on more than one architecture — if the target list \
             shrank, say so deliberately here",
            want.len()
        );

        // (1) a list, never a scalar. `LINUX_TRIPLE="<one triple>"` is the defect itself.
        let decl = format!("{var}=(");
        assert!(
            src.lines().any(|l| l.starts_with(&decl)),
            "{script} declares no `{var}=( … )` at the start of a line.\nA shipping lane's \
             triples must be a LIST a loop can walk: a scalar `{var}=\"<one triple>\"` is \
             precisely the defect this test exists for — it strands every other \
             architecture in a silent `unavailable on <target>` skip that reads clean from \
             the builder."
        );

        // (2a) filled from the shared list, not retyped.
        assert!(
            src.contains(&format!("${shared_var}")),
            "{script} does not derive {var} from ${shared_var} ({shared_script}).\nA \
             retyped triple list is a list that drifts: the point is that the next Linux \
             triple added to atpkg::TARGETS arrives in this lane by itself, instead of \
             waiting for someone to notice a literal."
        );

        // (2b) …and the shared list IS this client's TARGETS, for this lane's OS.
        let got: BTreeSet<&str> = shared
            .iter()
            .map(String::as_str)
            .filter(|t| t.contains(os))
            .collect();
        assert_eq!(
            got, want,
            "{shared_script}'s {shared_var} does not match the {os:?} triples of \
             atpkg::TARGETS, so {script} derives the wrong set.\n  shell says: {got:?}\n  \
             TARGETS says: {want:?}\nA triple in TARGETS that no lane publishes is a client \
             asking forever for a row nothing can produce — the `unavailable on <target>` \
             state is silent, so nothing else will ever report this."
        );
    }
}

/// (3): no step of the lane may name one of its triples literally. A loop over the list
/// is worth nothing if a path, an asset name or an upload still hardcodes one triple.
#[test]
fn a_shipping_lane_names_no_triple_outside_its_declaration() {
    for (script, os, _var) in LANES {
        let src = read(script);
        let family: Vec<&str> = atpkg::TARGETS
            .iter()
            .copied()
            .filter(|t| t.contains(os))
            .collect();

        let mut offenders: Vec<String> = Vec::new();
        for (n, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            // Comments are where this file WANTS the triples named: the prose that
            // explains the lane has to be able to say which architectures it serves.
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if family.iter().any(|t| line.contains(t)) {
                offenders.push(format!("  {}:{}: {}", script, n + 1, line.trim()));
            }
        }
        assert!(
            offenders.is_empty(),
            "{script} spells a target triple into its executable lines instead of walking \
             the derived list:\n{}\nEvery decision, pack, staged asset, manifest-row check \
             and upload must run once per triple. A single hardcoded spelling here is how \
             an entire architecture stops shipping without one error message.",
            offenders.join("\n")
        );
    }
}

/// The behavioural twin must exist and be runnable: this file reads the lane's TEXT, and
/// text alone cannot prove the lane's decision table changed with it.
#[test]
fn the_behavioural_suite_for_the_linux_lane_is_present() {
    let suite = repo_root().join("tools/test-linux-auto-atpkg.sh");
    assert!(
        suite.is_file(),
        "tools/test-linux-auto-atpkg.sh is missing — it is the half of this guard that \
         RUNS tools/linux-auto-atpkg.sh (stubbed channel, DRY_RUN, both host \
         architectures) and reads the decision table it prints. Without it, the checks \
         above only prove the source text looks right."
    );
}
