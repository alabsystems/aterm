// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Executable instruments for frozen-cutter handover and credential-free source judgment.

use super::*;
use std::ffi::OsString;

#[cfg(debug_assertions)]
pub(crate) fn cutter_probe_if_asked(args: &[OsString]) -> Option<Result<(), String>> {
    if args.first()?.as_os_str() != "--cutter-child-probe" {
        return None;
    }
    Some(run(args))
}

#[cfg(debug_assertions)]
fn run(args: &[OsString]) -> Result<(), String> {
    let stage = args
        .get(1)
        .and_then(|arg| arg.to_str())
        .ok_or("no probe stage")?;
    if stage == "child" {
        for name in [
            crate::release_tools::FREEZER_ENV,
            crate::release_tools::CONTRACT_ENV,
            crate::release_tools::REFREEZE_ENV,
            crate::cut_timing::EXEC_PID_ENV,
        ] {
            if std::env::var_os(name).is_some() {
                return Err(format!("freezer envelope leaked: {name}"));
            }
        }
        return match crate::cutter::running_stamp() {
            Ok(crate::cutter::Stamp::Proved(commit)) => {
                println!("child Proved {commit}; freezer envelope absent");
                Ok(())
            }
            stamp => Err(format!("child is no commit's proved cutter: {stamp:?}")),
        };
    }
    let repo = std::env::current_dir().map_err(|error| error.to_string())?;
    let mode = args
        .get(2)
        .and_then(|arg| arg.to_str())
        .ok_or("no probe mode")?;
    let mut real = Real::new(&repo, Some(&repo), true, false)?;
    if stage == "exec" {
        let cutter = PathBuf::from(args.get(3).ok_or("no frozen image")?);
        real.argv = vec!["--cutter-child-probe".into(), "parent".into(), mode.into()];
        real.argv.extend(args.iter().skip(4).cloned());
        let mut cmd = real.cutter_command(&cutter);
        crate::release_tools::Recovery::Initial.hand(
            &mut cmd,
            "probe",
            "probe",
            Some(std::ffi::OsStr::new("probe")),
        );
        // This is the same command constructor and native exec as exec_cutter, without
        // acquiring release resources or starting any publication work.
        cmd.env(
            crate::cut_timing::EXEC_PID_ENV,
            std::process::id().to_string(),
        );
        return Err(cmd.exec().to_string());
    }
    if stage != "parent" {
        return Err(format!("unknown probe stage {stage}"));
    }
    match crate::cutter::running_stamp() {
        Ok(crate::cutter::Stamp::Proved(commit)) => println!("parent Proved {commit}"),
        stamp => return Err(format!("parent is not proved: {stamp:?}")),
    }
    // Ensure the receiver really got exec's envelope before testing its removal in a child.
    for name in [
        crate::release_tools::FREEZER_ENV,
        crate::release_tools::CONTRACT_ENV,
        crate::release_tools::REFREEZE_ENV,
    ] {
        if std::env::var_os(name).is_none() {
            return Err(format!("exec did not hand {name}"));
        }
    }
    let exe = if mode == "foreign" {
        Ok(PathBuf::from(args.get(3).ok_or("no foreign image")?))
    } else {
        std::env::current_exe()
    };
    let prover = Prover {
        bin: repo.clone(),
        commit: "probe".into(),
        source: "probe".into(),
    };
    let mut child = real.cut_command_of(
        exe,
        &["--cutter-child-probe".into(), "child".into()],
        &prover,
    )?;
    if mode == "scrubbed" {
        child.env_remove(crate::release_tools::RECORD_ENV);
    } else if mode == "envelope" {
        child.env(crate::release_tools::FREEZER_ENV, "leaked");
    } else if !matches!(mode, "proved" | "foreign") {
        return Err(format!("unknown probe mode {mode}"));
    }
    let out = aterm_uds::deadline::output_within(&mut child, std::time::Duration::from_secs(60))
        .map_err(|error| error.to_string())?;
    print!("{}", String::from_utf8_lossy(&out.stdout));
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

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
    let blocks = real.commit_gates(&candidate, dry);
    let mut refusals: Vec<_> = blocks.iter().map(|r| r.why.clone()).collect();
    let mut unavailable = blocks.iter().any(|r| r.could_not_run);
    let mut input = None;
    if blocks.is_empty() || dry {
        if !dry {
            real.seal_inputs()?;
        }
        input = Some(match real.input_checks(base, &commit, &prover) {
            Ok(note) => note,
            Err(refusal) => {
                unavailable |= refusal.could_not_run;
                refusals.push(refusal.why.clone());
                refusal.why
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
