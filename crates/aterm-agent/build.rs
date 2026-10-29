// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cfg(anchor_rules)`: which anchor attributes this crate writes
//! (crates/aterm-spec-anchor/anchor_rules.rs). And the measured builds of both step folds, each
//! read from the files the checkout holds: `MEASURED_BUILDS` (`src/harness/steps.rs`), the
//! builds the Claude Code transcript reductions' own lines name (`# claude <build> · lab reduce
//! …`, `src/harness/testdata/steps/<build>.jsonl`), and `CODEX_MEASURED_BUILDS`
//! (`src/harness/codex_steps.rs`), the builds the `codex-alab-*` rollout reductions' own lines
//! name (`# codex <build> · lab reduce …`). A commit of a build's reductions (`lab
//! claude-measure`, `lab codex-measure`) measures it, and a release, built from a clean clone,
//! lists the committed ones.

use std::fmt::Write as _;
use std::io::BufRead as _;
use std::path::PathBuf;

#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

/// The step testdata, beside the manifest.
const STEPS: &str = "src/harness/testdata/steps";

/// One step fold's reductions in the step testdata, and the list `build.rs` writes of them.
struct Family {
    /// The program word a reduction's own line opens with (`# <program> <build> · lab reduce`).
    program: &'static str,
    /// Whether the file `name` is one of its reductions (every such file opens with a build).
    holds: fn(&str) -> bool,
    /// Whether a reduction named `name` measures the build it names.
    measures: fn(&str) -> bool,
    /// The const the list is, as a refusal names it.
    list: &'static str,
    /// The file below `$OUT_DIR` the list is written to.
    out: &'static str,
}

/// A Codex rollout's reduction: `codex-<build>[-<leg>].jsonl`.
fn is_codex(name: &str) -> bool {
    name.starts_with("codex-") && name.ends_with(".jsonl")
}

/// A Claude Code transcript's reduction: every other `.jsonl` there (`<build>.jsonl`).
fn is_claude(name: &str) -> bool {
    name.ends_with(".jsonl") && !name.starts_with("codex-")
}

/// Only a codex-alab build's reductions measure it: the stem
/// `aterm_phase::anchors::rollout_reduction_stem` makes of a codex-alab build (the codex step
/// fold's tests hold each name to its line's build), so this reads no rule of its own.
fn is_alab(name: &str) -> bool {
    name.starts_with("codex-alab-")
}

/// Every Claude Code reduction measures its build (the step fold's tests hold its name to it).
fn every(_: &str) -> bool {
    true
}

const FAMILIES: [Family; 2] = [
    Family {
        program: "claude",
        holds: is_claude,
        measures: every,
        list: "MEASURED_BUILDS",
        out: "claude_measured_builds.rs",
    },
    Family {
        program: "codex",
        holds: is_codex,
        measures: is_alab,
        list: "CODEX_MEASURED_BUILDS",
        out: "codex_measured_builds.rs",
    },
];

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    anchor_rules::emit();
    println!("cargo::rerun-if-changed={STEPS}");
    for family in &FAMILIES {
        measured_builds(family);
    }
}

/// Write `$OUT_DIR/<family.out>`: every build a reduction of `family` that measures it names in
/// its first line, sorted, once each. A reduction that opens with no build, or none at all,
/// fails the build: an empty list would leave every tab of that program unmeasured.
fn measured_builds(family: &Family) {
    let dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it")).join(STEPS);
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e} (the step testdata)", dir.display()));
    let opens = format!("# {} ", family.program);
    let mut builds = Vec::new();
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(family.holds)(name) {
            continue;
        }
        let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut head = String::new();
        std::io::BufReader::new(file)
            .read_line(&mut head)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let build = head
            .strip_prefix(&opens)
            .and_then(|rest| rest.split(' ').next())
            .filter(|b| !b.is_empty() && *b != "-")
            .unwrap_or_else(|| {
                panic!(
                    "{name}: a {} reduction opens with `{opens}<build> · lab reduce`",
                    family.program
                )
            });
        if (family.measures)(name) {
            builds.push(build.to_owned());
        }
    }
    builds.sort();
    builds.dedup();
    assert!(
        !builds.is_empty(),
        "{}: no {} reduction measures a build, so {} would be empty",
        dir.display(),
        family.program,
        family.list
    );
    let mut code = String::from("&[\n");
    for b in &builds {
        let _ = writeln!(code, "    {b:?},");
    }
    code.push_str("]\n");
    let out =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join(family.out);
    std::fs::write(&out, code).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}
