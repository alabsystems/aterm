// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A per-user launchd agent that outlives the terminal and the worktree that installed it: the
//! main lane's controller (`verify watch --install`) and the release pump (`ship release --pump
//! --install`). The caller writes the property list; this replaces the loaded job with it, or
//! removes it. How long a loaded job may take to end is its caller's to say: launchd ends a job
//! with SIGTERM and waits its `ExitTimeOut` before SIGKILL, so a wait shorter than that gives up
//! on a job that is still ending as designed.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `value` as property-list text.
#[must_use]
pub fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Wait until `present` says the job is gone, `wait` pausing between looks and saying when to
/// give up.
/// # Errors
/// The look's own refusal, or the job still loaded when `wait` gives up.
pub fn await_unloaded(
    mut present: impl FnMut() -> Result<bool, String>,
    mut wait: impl FnMut() -> bool,
) -> Result<(), String> {
    while present()? {
        if !wait() {
            return Err("the previous service did not unload in the time it was given".into());
        }
    }
    Ok(())
}

/// The agent's property list under `home`.
#[must_use]
pub fn plist(home: &Path, label: &str) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{label}.plist"))
}

fn domain() -> Result<String, String> {
    let output = aterm_uds::spawnfd::command("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("cannot resolve launchd user domain".into());
    }
    Ok(format!(
        "gui/{}",
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

/// Unload `target` and wait at most `within` until launchd has finished removing it. An absent
/// job is normal.
fn unload(target: &str, within: Duration) -> Result<(), String> {
    let _ = aterm_uds::spawnfd::command("/bin/launchctl")
        .args(["bootout", target])
        .output();
    // bootout acknowledges removal before the old job has necessarily exited.
    // Bootstrap cannot replace that job until launchd has finished removing it.
    let until = Instant::now() + within;
    await_unloaded(
        || {
            let output = aterm_uds::spawnfd::command("/bin/launchctl")
                .args(["print", target])
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
    )
    .map_err(|why| format!("{why} ({} s)", within.as_secs()))
}

/// Replace the user agent `label` with the property list `text`: the loaded job is unloaded and
/// waited out for at most `within` (its `ExitTimeOut` and a margin), the list written, and
/// launchd bootstrapped with it. Returns the job's target (`gui/<uid>/<label>`).
/// # Errors
/// Anything that leaves the new job unloaded; bootstrap's result is authoritative.
pub fn replace(home: &Path, label: &str, text: &str, within: Duration) -> Result<String, String> {
    let plist = plist(home, label);
    if let Some(agents) = plist.parent() {
        fs::create_dir_all(agents).map_err(|e| e.to_string())?;
    }
    let domain = domain()?;
    let target = format!("{domain}/{label}");
    unload(&target, within)?;
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
    Ok(target)
}

/// What [`remove`] found and left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removed {
    /// No property list was there, and no job is loaded.
    Absent,
    /// The property list is removed and the job unloaded.
    Unloaded,
    /// The property list is removed, so nothing starts the agent at the next login, and the
    /// job had not finished unloading when the wait gave up: why.
    Exiting(String),
}

/// Remove the user agent `label`: its property list, then the loaded job, waited out for at
/// most `within`.
/// # Errors
/// A list that cannot be removed.
pub fn remove(home: &Path, label: &str, within: Duration) -> Result<Removed, String> {
    let target = format!("{}/{label}", domain()?);
    remove_list(&plist(home, label), || unload(&target, within))
}

/// [`remove`] over the list at `plist` and the job's `unload`. The list goes first: whatever the
/// unload says, an agent asked to go must not start again at the next login, and a job that
/// outlives the wait is still ended by launchd at its `ExitTimeOut`.
/// # Errors
/// A list that cannot be removed; `unload` is then not asked.
pub fn remove_list(
    plist: &Path,
    unload: impl FnOnce() -> Result<(), String>,
) -> Result<Removed, String> {
    let listed = match fs::remove_file(plist) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(format!("remove {}: {e}", plist.display())),
    };
    Ok(match unload() {
        Ok(()) if listed => Removed::Unloaded,
        Ok(()) => Removed::Absent,
        Err(why) => Removed::Exiting(why),
    })
}

#[cfg(test)]
#[path = "launch_agent_tests.rs"]
mod tests;
