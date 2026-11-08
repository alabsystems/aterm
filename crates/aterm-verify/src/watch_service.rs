// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A main-lane controller survives its deploying worktree and the terminal that installed it.

use super::Options;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const LABEL: &str = "systems.alab.aterm-verify-watch";

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

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

fn await_unloaded(
    mut present: impl FnMut() -> Result<bool, String>,
    mut wait: impl FnMut() -> bool,
) -> Result<(), String> {
    while present()? {
        if !wait() {
            return Err("previous watch service did not unload within one minute".into());
        }
    }
    Ok(())
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
    let agents = home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents).map_err(|e| e.to_string())?;
    // Immutable deployments: never overwrite a possibly executing Mach-O. The service
    // cannot depend on the caller's worktree, which is removed after landing.
    let token = aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?;
    let deployment = home
        .join(".local/state/aterm-verify/watch/controllers")
        .join(token);
    fs::create_dir_all(&deployment).map_err(|e| e.to_string())?;
    let exe = deployment.join("aterm-verify");
    fs::copy(std::env::current_exe().map_err(|e| e.to_string())?, &exe)
        .map_err(|e| e.to_string())?;
    let plist = agents.join(format!("{LABEL}.plist"));
    let text = manifest(
        &exe,
        &root,
        home,
        &std::env::var("PATH").unwrap_or_default(),
        opts.interval.as_secs(),
    );
    let output = aterm_uds::spawnfd::command("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("cannot resolve launchd user domain".into());
    }
    let domain = format!("gui/{}", String::from_utf8_lossy(&output.stdout).trim());
    let target = format!("{domain}/{LABEL}");
    // An absent unit is normal on first install. Bootstrap's result is authoritative.
    let _ = aterm_uds::spawnfd::command("/bin/launchctl")
        .args(["bootout", &target])
        .output();
    // bootout acknowledges removal before the old job has necessarily exited.
    // Bootstrap cannot replace that job until launchd has finished removing it.
    let until = Instant::now() + Duration::from_secs(60);
    await_unloaded(
        || {
            let output = aterm_uds::spawnfd::command("/bin/launchctl")
                .args(["print", &target])
                .output()
                .map_err(|e| e.to_string())?;
            if output.status.success() {
                Ok(true)
            } else if output.status.code() == Some(113) {
                Ok(false)
            } else {
                Err(String::from_utf8_lossy(&output.stderr).into_owned())
            }
        },
        || {
            std::thread::sleep(Duration::from_millis(100));
            Instant::now() < until
        },
    )?;
    fs::write(&plist, text).map_err(|e| e.to_string())?;
    let status = aterm_uds::spawnfd::command("/bin/launchctl")
        .arg("bootstrap")
        .arg(&domain)
        .arg(&plist)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("launchd bootstrap {status}"));
    }
    println!(
        "verify watch installed: {target}\ncheckout={}\nlog={}/.git/watch-service.log\nverdicts=refs/aterm/judged",
        root.display(),
        root.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_controller_is_independent_of_the_deploying_terminal() {
        let text = manifest(
            Path::new("/state/controllers/123/aterm-verify"),
            Path::new("/state/check & judge"),
            Path::new("/Users//a"),
            "/bin:/custom",
            17,
        );
        // Negative controls: omitting restart or substituting the worker's cwd/executable
        // would lose the lane when that worker exits and removes its worktree.
        assert!(text.contains("<key>KeepAlive</key><true/>"));
        assert!(text.contains("<key>RunAtLoad</key><true/>"));
        assert!(text.contains("<string>/state/controllers/123/aterm-verify</string>"));
        assert!(
            text.contains("<key>WorkingDirectory</key><string>/state/check &amp; judge</string>")
        );
        assert!(text.contains("<key>ProcessType</key><string>Interactive</string>"));
        assert!(text.contains("<key>ExitTimeOut</key><integer>60</integer>"));
        assert!(text.contains("<string>17</string>"));
        assert!(text.contains("<key>PATH</key><string>/bin:/custom</string>"));
        assert_eq!(xml("<&\"'>"), "&lt;&amp;&quot;&apos;&gt;");
    }

    #[test]
    fn replacement_waits_for_removal_and_never_bootstraps_over_a_live_job() {
        let mut states = [true, true, false].into_iter();
        let mut waits = 0;
        await_unloaded(
            || Ok(states.next().unwrap()),
            || {
                waits += 1;
                true
            },
        )
        .unwrap();
        assert_eq!(waits, 2);
        assert!(states.next().is_none());
        assert!(await_unloaded(|| Ok(true), || false).is_err());
        assert!(
            await_unloaded(
                || Err("permission refused".into()),
                || panic!("must preserve refusal")
            )
            .is_err()
        );
    }
}
