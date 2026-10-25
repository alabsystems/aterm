// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cfg(anchor_rules)`: which anchor attributes this crate writes
//! (crates/aterm-spec-anchor/anchor_rules.rs). And `CODEX_MEASURED_BUILDS`
//! (`src/harness/codex_steps.rs`): the builds the `codex-alab-*` rollout reductions' own lines
//! name (`# codex <build> · lab reduce …`, `src/harness/testdata/steps/codex-alab-*.jsonl`), read
//! from the files the checkout holds: a commit of a build's reductions (`lab codex-measure`)
//! measures it, and a release, built from a clean clone, lists the committed ones.

use std::fmt::Write as _;
use std::io::BufRead as _;
use std::path::PathBuf;

#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

/// The step testdata, beside the manifest.
const STEPS: &str = "src/harness/testdata/steps";

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    anchor_rules::emit();
    measured_builds();
}

/// Write `$OUT_DIR/codex_measured_builds.rs`: every build a `codex-alab-*` reduction's first
/// line names, sorted, once each. The name is the codex-alab rule's own: it is the stem
/// `aterm_phase::anchors::rollout_reduction_stem` made of that build (the step fold's tests hold
/// each name to its line's build), so this reads no rule of its own. A reduction that opens with
/// no build, or none at all, fails the build: an empty list would leave every codex tab
/// unmeasured.
fn measured_builds() {
    println!("cargo::rerun-if-changed={STEPS}");
    let dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it")).join(STEPS);
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e} (the step testdata)", dir.display()));
    let mut builds = Vec::new();
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(name.starts_with("codex-") && name.ends_with(".jsonl")) {
            continue;
        }
        let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut head = String::new();
        std::io::BufReader::new(file)
            .read_line(&mut head)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let build = head
            .strip_prefix("# codex ")
            .and_then(|rest| rest.split(' ').next())
            .filter(|b| !b.is_empty() && *b != "-")
            .unwrap_or_else(|| {
                panic!("{name}: a rollout reduction opens with `# codex <build> · lab reduce`")
            });
        if name.starts_with("codex-alab-") {
            builds.push(build.to_owned());
        }
    }
    builds.sort();
    builds.dedup();
    assert!(
        !builds.is_empty(),
        "{}: no codex-alab rollout reduction, so no build would be measured",
        dir.display()
    );
    let mut code = String::from("&[\n");
    for b in &builds {
        let _ = writeln!(code, "    {b:?},");
    }
    code.push_str("]\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("codex_measured_builds.rs");
    std::fs::write(&out, code).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
}
