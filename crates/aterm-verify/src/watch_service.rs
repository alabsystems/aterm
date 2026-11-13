// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A main-lane controller survives its deploying worktree and the terminal that installed it.

use super::Options;
use crate::launch_agent::xml;
use std::fs;
use std::path::Path;

const LABEL: &str = "systems.alab.aterm-verify-watch";

fn manifest(exe: &Path, root: &Path, home: &Path, path: &str, interval: u64) -> String {
    // Capacity owns Background rank and Release cancellation. The checks must
    // exercise default/boosted QoS, which Standard/Background launchd jobs cap.
    let exe = xml(&exe.to_string_lossy());
    let root = xml(&root.to_string_lossy());
    let home = xml(&home.to_string_lossy());
    let path = xml(path);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{exe}</string><string>watch</string><string>--workspace</string><string>{root}</string><string>--interval</string><string>{interval}</string></array>
<key>WorkingDirectory</key><string>{root}</string>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>{home}</string><key>PATH</key><string>{path}</string></dict>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>60</integer>
<key>ExitTimeOut</key><integer>60</integer>
<key>ProcessType</key><string>Interactive</string>
<key>StandardOutPath</key><string>{root}/.git/watch-service.log</string>
<key>StandardErrorPath</key><string>{root}/.git/watch-service.err</string>
</dict></plist>
"#
    )
}

pub(super) fn install(opts: &Options, cwd: &Path) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("persistent watch installation requires macOS launchd".into());
    }
    if opts.once || opts.branch != "main" {
        return Err("the persistent main lane requires main and cannot use --once".into());
    }
    let root = super::prepare_workspace(opts, cwd)?;
    let home = std::env::var_os("HOME").ok_or("no HOME")?;
    let home = Path::new(&home);
    // Immutable deployments: never overwrite a possibly executing Mach-O. The service
    // cannot depend on the caller's worktree, which is removed after landing.
    let token = aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?;
    let deployment = home
        .join(".local/state/aterm-verify/.ubd/watch/controllers")
        .join(token);
    fs::create_dir_all(&deployment).map_err(|e| e.to_string())?;
    let exe = deployment.join("aterm-verify");
    fs::copy(std::env::current_exe().map_err(|e| e.to_string())?, &exe)
        .map_err(|e| e.to_string())?;
    let text = manifest(
        &exe,
        &root,
        home,
        &std::env::var("PATH").unwrap_or_default(),
        opts.interval.as_secs(),
    );
    // The controller sets no ExitTimeOut: launchd ends it within its default 20 s.
    let target =
        crate::launch_agent::replace(home, LABEL, &text, std::time::Duration::from_secs(60))?;
    println!(
        "verify watch installed: {target}\ncheckout={}\nlog={}/.git/watch-service.log\nverdicts=refs/aterm/judged",
        root.display(),
        root.display()
    );
    Ok(())
}

#[cfg(test)]
#[path = "watch_service_tests.rs"]
mod tests;
