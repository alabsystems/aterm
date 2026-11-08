// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Debug-only executable seam for the frozen cutter's real exec and child boundaries.

use super::*;
use std::ffi::OsString;

pub(crate) fn cutter_probe_if_asked(args: &[OsString]) -> Option<Result<(), String>> {
    if args.first()?.as_os_str() != "--cutter-child-probe" {
        return None;
    }
    Some(run(args))
}

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
    let mut real = Real::new(&repo, Some(&repo), true, false)?;
    let mode = args
        .get(2)
        .and_then(|arg| arg.to_str())
        .ok_or("no probe mode")?;
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
