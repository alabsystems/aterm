// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The temporal-model registry for `aterm-spec` — the build-gate opt-in target.
//!
//! This is aterm's adoption of Trust's build-time temporal gate (see Trust's
//! `docs/TY_ANNOTATION_FEATURE.md`): `tcargo trust build` (and `tcargo trust
//! temporal`) run this example as PART OF the build, model-checking every derived
//! `Model` with the embedded ty checker and FAILING the build (nonzero exit) on
//! any violated, vacuous, or undecidable verdict. It is also runnable directly as
//! a CI/local backstop:
//!
//! ```text
//! TY_BIN=$HOME/trust/first-party/ty/target/release/ty \
//!   cargo run -p aterm-spec --example trust_models
//! ```
//!
//! aterm keeps its own (API-identical) `aterm_spec::derive` generator rather than
//! depending on `trust-spec-temporal`; this example reuses `Model::to_tla` /
//! `to_cfg` / `to_cfg_with` and the same prove-AND-catch discipline as the
//! `derived_ring_ty.rs` test (prove at the committed config; require a
//! counterexample at `Buggy = 1` when the model declares that dial), but as a
//! process that exits FAIL-CLOSED so the build gate can fold its result.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use aterm_spec::derive::{
    Model, cursor_model, evict_full_model, kernel_model, read_image_seq_model, recording_model,
    ring_model, snapshot_model, subscribe_model, tier_residency_model, transact_model,
    window_routing_model,
};

/// Locate the embedded ty checker (same precedence as the derived-ty test).
fn find_ty() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TY_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        for rel in [
            "trust/first-party/ty/target/release/ty",
            "trust/build/host/stage2/bin/ty",
            "ty/target/release/ty",
        ] {
            let p = PathBuf::from(&home).join(rel);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let out = Command::new("sh")
        .arg("-c")
        .arg("command -v ty")
        .output()
        .ok()?;
    if out.status.success() {
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

/// `ty check` exits 0 even on a violation, so the verdict is parsed from stdout.
fn proved_exhaustive(out: &str) -> bool {
    out.contains("No errors found") && !out.contains("is violated")
}
fn invariant_violated(out: &str) -> bool {
    out.contains("is violated")
}

#[derive(Debug)]
#[allow(dead_code)] // payload strings are surfaced via the Debug print of each row
enum Verdict {
    Proved,
    ProvedNoDial,
    Failed(String),
    Vacuous,
    Unknown(String),
}
impl Verdict {
    fn is_proved(&self) -> bool {
        matches!(self, Verdict::Proved | Verdict::ProvedNoDial)
    }
}

/// Run `ty check spec --config cfg`, returning combined stdout+stderr.
fn run_ty(ty: &PathBuf, spec: &PathBuf, cfg: &PathBuf) -> std::io::Result<String> {
    let out = Command::new(ty)
        .arg("check")
        .arg(spec)
        .arg("--config")
        .arg(cfg)
        .output()?;
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// Model-check one model: prove at the committed config, and (if it declares a
/// `Buggy` dial) require a counterexample at `Buggy = 1`. Fail-closed.
fn check_model(ty: &PathBuf, m: &Model) -> Verdict {
    let dir =
        std::env::temp_dir().join(format!("aterm-temporal-{}-{}", m.name, std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return Verdict::Unknown("could not create temp dir".into());
    }
    let spec = dir.join(format!("{}.tla", m.name));
    if std::fs::write(&spec, m.to_tla()).is_err() {
        return Verdict::Unknown("could not write .tla".into());
    }
    let ok_cfg = dir.join(format!("{}.ok.cfg", m.name));
    if std::fs::write(&ok_cfg, m.to_cfg()).is_err() {
        return Verdict::Unknown("could not write ok cfg".into());
    }
    let ok = match run_ty(ty, &spec, &ok_cfg) {
        Ok(o) => o,
        Err(e) => return Verdict::Unknown(format!("ty spawn failed: {e}")),
    };
    if invariant_violated(&ok) {
        let inv = m
            .invariants
            .first()
            .map(|i| i.name.to_string())
            .unwrap_or_default();
        return Verdict::Failed(inv);
    }
    if !proved_exhaustive(&ok) {
        return Verdict::Unknown(format!("not exhaustively proved: {}", ok.trim()));
    }
    if !m.consts.iter().any(|(n, _)| *n == "Buggy") {
        return Verdict::ProvedNoDial;
    }
    let bug_cfg = dir.join(format!("{}.bug.cfg", m.name));
    if std::fs::write(&bug_cfg, m.to_cfg_with(&[("Buggy", 1)])).is_err() {
        return Verdict::Unknown("could not write bug cfg".into());
    }
    match run_ty(ty, &spec, &bug_cfg) {
        Ok(o) if invariant_violated(&o) => Verdict::Proved,
        Ok(o) if proved_exhaustive(&o) => Verdict::Vacuous,
        Ok(o) => Verdict::Unknown(format!("Buggy=1 inconclusive: {}", o.trim())),
        Err(e) => Verdict::Unknown(format!("ty spawn failed (Buggy=1): {e}")),
    }
}

fn main() -> ExitCode {
    let models: &[Model] = &[
        ring_model(),
        cursor_model(),
        subscribe_model(),
        transact_model(),
        kernel_model(),
        snapshot_model(),
        read_image_seq_model(),
        evict_full_model(),
        tier_residency_model(),
        recording_model(),
        window_routing_model(),
    ];

    let Some(ty) = find_ty() else {
        eprintln!(
            "aterm temporal gate: embedded ty checker not found (set TY_BIN or build \
             $HOME/trust/first-party/ty) — FAILING fail-closed (a missing checker must never read ok)."
        );
        return ExitCode::from(1);
    };
    eprintln!(
        "aterm temporal gate: model-checking {} derived model(s) with the embedded ty checker...",
        models.len()
    );
    let mut all_ok = true;
    for m in models {
        let v = check_model(&ty, m);
        let ok = v.is_proved();
        all_ok &= ok;
        eprintln!(
            "  {} {:<22} {v:?}",
            if ok { "ok  " } else { "FAIL" },
            m.name
        );
    }
    if all_ok {
        eprintln!(
            "aterm temporal gate: all {} model(s) PROVED by embedded ty.",
            models.len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "aterm temporal gate: one or more models NOT proved — failing the build (fail-closed)."
        );
        ExitCode::from(1)
    }
}
