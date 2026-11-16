// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! launchd's job switches as `launchctl print-disabled <domain>` lists them, read without
//! changing them: a person's `launchctl disable`, or their switch of a job's background item in
//! System Settings' Login Items ([`job_switch_in`]). Every reader of that list in aterm reads it
//! here, so a row of another block is never taken for a job's switch.

/// `launchctl`'s fixed path.
pub const LAUNCHCTL: &str = crate::os_path!("launchctl");

/// The argv (after [`LAUNCHCTL`]) that prints the `system` domain's job switches.
pub const PRINT_DISABLED_SYSTEM: [&str; 2] = ["print-disabled", "system"];

/// A job's switch as launchd lists it in `launchctl print-disabled <domain>` ([`job_switch_in`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobSwitch {
    /// Its row reads `enabled` (`false` before macOS 13).
    On,
    /// Its row reads `disabled` (`true` before macOS 13): written by a person's `launchctl
    /// disable` or their switch of the job's background item in Login Items.
    Off,
    /// The list has no row for it: launchd holds no override, and the job's plist decides.
    Unlisted,
}

/// The switch `launchctl print-disabled <domain>`'s `text` lists for the job `label`: its row of
/// the `disabled services = {` block. A row of any other block (`login item associations` names
/// jobs too) is not the job's switch.
///
/// # Errors
/// `text` holds no such block, the block ends before its close, or the job's row says anything
/// but `enabled`, `disabled`, `true` or `false`, in one line.
pub fn job_switch_in(text: &str, label: &str) -> Result<JobSwitch, String> {
    let quoted = format!("\"{label}\"");
    let mut lines = text
        .lines()
        .skip_while(|line| line.trim() != "disabled services = {");
    if lines.next().is_none() {
        return Err(format!(
            "launchctl print-disabled printed {:?}, not its service list",
            text.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
        ));
    }
    for line in lines {
        let line = line.trim();
        if line == "}" {
            return Ok(JobSwitch::Unlisted);
        }
        let Some((name, state)) = line.split_once("=>") else {
            continue;
        };
        if name.trim() != quoted {
            continue;
        }
        return match state.trim() {
            "disabled" | "true" => Ok(JobSwitch::Off),
            "enabled" | "false" => Ok(JobSwitch::On),
            other => Err(format!("launchctl says {label} is {other:?}")),
        };
    }
    Err("launchctl print-disabled's service list ends before its close".to_owned())
}

/// Whether launchd lists the system LaunchDaemon `label` switched off (`launchctl print-disabled
/// system`, [`job_switch_in`]), read without changing it: `None` where the read failed, timed out
/// or printed no list, and off macOS. One `launchctl` run, waited on for at most five seconds:
/// never on the window thread.
#[must_use]
pub fn system_job_switched_off(label: &str) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        let mut command = crate::spawnfd::command(LAUNCHCTL);
        command.args(PRINT_DISABLED_SYSTEM);
        let output =
            crate::deadline::output_within(&mut command, std::time::Duration::from_secs(5)).ok()?;
        switched_off_of(
            output.status.code(),
            &String::from_utf8(output.stdout).ok()?,
            label,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = label;
        None
    }
}

/// [`system_job_switched_off`]'s answer from a `launchctl` run that exited `code` printing
/// `text`: only a successful run's readable list answers.
#[cfg(any(target_os = "macos", test))]
fn switched_off_of(code: Option<i32>, text: &str, label: &str) -> Option<bool> {
    if code != Some(0) {
        return None;
    }
    job_switch_in(text, label)
        .ok()
        .map(|switch| switch == JobSwitch::Off)
}

#[cfg(test)]
#[path = "launchd_tests.rs"]
mod tests;
