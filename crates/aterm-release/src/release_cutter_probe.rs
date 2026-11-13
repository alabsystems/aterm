// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Credential-free source judgment through the actual frozen cutter.

use super::*;
use std::ffi::OsString;

/// Source-only step 1 instrument: the actual frozen image and production gates, with
/// no credential, fleet claim, qualification, publication or signing entry constructed.
pub(crate) fn step_one_probe_if_asked(args: &[OsString]) -> Option<Result<(), String>> {
    if args.first()?.as_os_str() != "--step-one-source-measure" {
        return None;
    }
    Some(step_one(args))
}

fn step_one(args: &[OsString]) -> Result<(), String> {
    if args.len() != 4 {
        return Err("source instrument takes dry|real BASE ENGINE only".into());
    }
    let repo = std::env::current_dir().map_err(|e| e.to_string())?;
    let mode = args[1].to_str().ok_or("invalid mode")?;
    let dry = match mode {
        "dry" => true,
        "real" => false,
        _ => return Err("select dry or real".into()),
    };
    let base = args
        .get(2)
        .and_then(|arg| arg.to_str())
        .ok_or("no input base")?;
    let engine = Path::new(args.get(3).ok_or("no read-only engine source")?);
    let mut real = Real::new(&repo, Some(engine), dry, false)?;
    // Only policy/readiness readers are entered; never sync or execute this engine.
    real.engine = engine.to_owned();
    let commit = real.head();
    crate::gates::own_frozen_cutter(&commit).map_err(|not| not.why)?;
    let candidate = Candidate {
        version: real.version_at(&commit).ok_or("no candidate version")?,
        commit: commit.clone(),
        why: "step 1 source instrument".into(),
        bump_from: None,
        carry: Vec::new(),
    };
    let prover = real.prover(None)?;
    let started = std::time::Instant::now();
    real.fetch_engine_origin()
        .map_err(|refusal| format!("publication engine snapshot refused: {refusal:?}"))?;
    let blocks = real.commit_gates(&candidate, dry).found;
    let mut refusals: Vec<_> = blocks.iter().map(|r| r.why.clone()).collect();
    let mut unavailable = blocks.iter().any(|r| r.could_not_run);
    let mut input = None;
    if blocks.is_empty() || dry {
        if !dry {
            real.seal_inputs()?;
        }
        input = Some(match real.input_checks(base, &commit, &prover) {
            Ok(note) => note,
            Err(found) => {
                unavailable |= matches!(found, crate::release::InputFinding::Unanswered(_));
                refusals.push(found.why().to_owned());
                found.why().to_owned()
            }
        });
    }
    println!(
        "step-one-measure {}",
        aterm_json::json!({
            "mode":mode, "commit":commit, "wall_s":started.elapsed().as_secs_f64(),
            "verdict": if unavailable { "could-not-run" } else if refusals.is_empty() { "pass" } else { "refuse" },
            "refusals":refusals, "input":input,
            "scope":"production source gates only; machine/credential questions excluded; no release started",
        })
    );
    Ok(())
}
