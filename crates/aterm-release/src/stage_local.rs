// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Official offline transport into the existing updater, never an apply route.

use crate::ledger::{Error, Result};

#[cfg(target_os = "macos")]
fn parse(args: &[String]) -> Result<aterm_update::LocalStageOptions> {
    let mut assets = None;
    let mut host_pid = None;
    let mut build = None;
    let mut commit = None;
    let mut stage = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--stage" {
            if stage {
                return Err(Error::new("--stage given twice"));
            }
            stage = true;
            continue;
        }
        let value = it
            .next()
            .ok_or_else(|| Error::new(format!("{arg} needs a value")))?;
        match arg.as_str() {
            "--assets" if assets.is_none() => assets = Some(value.into()),
            "--host-pid" if host_pid.is_none() => {
                host_pid =
                    Some(value.parse::<i32>().map_err(|_| {
                        Error::new("--host-pid needs the actual running release PID")
                    })?)
            }
            "--expect-build" if build.is_none() => {
                build = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| Error::new("--expect-build needs a build number"))?,
                )
            }
            "--expect-commit" if commit.is_none() => commit = Some(value.clone()),
            _ => {
                return Err(Error::new(format!(
                    "unknown or repeated stage-local flag: {arg}"
                )));
            }
        }
    }
    let commit = commit.ok_or_else(|| Error::new("--expect-commit required"))?;
    if !aterm_update::commit_matches(&commit, &commit) {
        return Err(Error::new(
            "--expect-commit needs the exact clean sealed source stamp",
        ));
    }
    Ok(aterm_update::LocalStageOptions {
        assets: assets.ok_or_else(|| Error::new("--assets required"))?,
        host_pid: host_pid
            .filter(|pid| *pid > 0)
            .ok_or_else(|| Error::new("--host-pid required"))?,
        expected_build: build.ok_or_else(|| Error::new("--expect-build required"))?,
        expected_commit: commit,
        stage,
    })
}

pub fn run(args: &[String]) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let result = aterm_update::stage_local(&parse(args)?).map_err(Error::new)?;
        println!("{result}");
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        Err(Error::new("stage-local requires the macOS bundle updater"))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn explicit_stage_and_exact_installed_identity_are_required() {
        let args = [
            "--assets",
            "/private/cut",
            "--host-pid",
            "123",
            "--expect-build",
            "42",
            "--expect-commit",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ]
        .map(str::to_string)
        .to_vec();
        assert!(!parse(&args).unwrap().stage);
        let mut stage = args.clone();
        stage.push("--stage".into());
        assert!(parse(&stage).unwrap().stage);
        stage.push("--stage".into());
        assert!(parse(&stage).is_err());
        for remove in [0, 2, 4, 6] {
            let mut missing = args.clone();
            missing.drain(remove..remove + 2);
            assert!(parse(&missing).is_err());
        }
        let mut installed_stamp = args.clone();
        installed_stamp[7] = "d63937ff7a2c".into();
        assert_eq!(
            parse(&installed_stamp).unwrap().expected_commit,
            "d63937ff7a2c"
        );
        let mut short = args;
        short[7] = "abc".into();
        assert!(parse(&short).is_err());
    }
}
