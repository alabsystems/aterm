// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `System` port: everything the agent asks of the guest's OS. The Linux arm
//! (`crate::linux::Guest`) asks systemd and the kernel; tests answer with
//! [`crate::fake::Fake`]; the macOS seam bin with [`crate::seam::Seam`].

use crate::wire::Share;

/// What the boot came to: `systemctl is-system-running --wait`'s word and the failed units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Booted {
    /// `running`, `degraded`, `maintenance`, …
    pub state: String,
    /// The failed units (`systemctl --failed`).
    pub failed_units: Vec<String>,
}

impl Booted {
    /// Whether the guest is up: `running` or `degraded`.
    #[must_use]
    pub fn is_up(&self) -> bool {
        matches!(self.state.as_str(), "running" | "degraded")
    }
}

/// A job as `systemctl list-jobs` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawJob {
    /// systemd's job id.
    pub id: u32,
    /// Its unit.
    pub unit: String,
    /// Its type.
    pub kind: String,
    /// Its state.
    pub state: String,
}

/// What a [`crate::wire::Hello`] says of the guest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// The kernel's id of this boot.
    pub boot_id: String,
    /// The kernel's release.
    pub kernel: String,
}

/// Who a command runs as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    /// The agent's own (the macOS seam, which is not root).
    Keep,
    /// The lane user: `setgroups`, then `setgid`, then `setuid` in the child, then the
    /// `chdir` into the command's `cwd` as that user, before the exec.
    Switch {
        /// Its uid.
        uid: u32,
        /// Its primary gid.
        gid: u32,
        /// Every group it is in, the primary one first.
        groups: Vec<u32>,
    },
}

/// The guest's OS, as the agent sees it.
pub trait System: Send + Sync {
    /// The boot's id and the kernel's release.
    fn facts(&self) -> Facts;

    /// The guest's uptime.
    fn uptime_ms(&self) -> u64;

    /// The jobs systemd still runs (`systemctl list-jobs`).
    fn jobs(&self) -> Vec<RawJob>;

    /// Wait for the boot to be over (`systemctl is-system-running --wait`), and say how it
    /// ended.
    fn booted(&self) -> Booted;

    /// Whether the lane user's manager answers at `/run/user/<uid>/bus` (linger).
    ///
    /// # Errors
    /// It does not, or there is no lane user: why.
    fn user_bus(&self) -> Result<(), String>;

    /// The guest's clock, in nanoseconds since the epoch.
    fn now_unix_ns(&self) -> u64;

    /// Step the guest's clock to `unix_ns`.
    ///
    /// # Errors
    /// The kernel refused: why.
    fn set_clock(&self, unix_ns: u64) -> Result<(), String>;

    /// Mount the `aterm` share and bind `shares` read-only at their paths, dropping the
    /// binds of an earlier `Mount`.
    ///
    /// # Errors
    /// A share is not one, or a mount failed: why.
    fn mount(&self, shares: &[Share]) -> Result<(), String>;

    /// Power the guest off (`systemctl poweroff`).
    ///
    /// # Errors
    /// systemctl could not queue it: why.
    fn poweroff(&self) -> Result<(), String>;

    /// Who an `Exec` runs as, read afresh for each one.
    ///
    /// # Errors
    /// There is no lane user: why.
    fn identity(&self) -> Result<Identity, String>;

    /// A line for the console (the unit's `journal+console`), whose tail the host's
    /// reports read when a boot stalls.
    fn say(&self, line: &str) {
        eprintln!("aterm-vz-agent: {line}");
    }
}

/// `systemctl list-jobs --plain --no-legend --no-pager`'s jobs: `JOB UNIT TYPE STATE` per
/// line; a line that is not one is skipped.
#[must_use]
pub fn parse_jobs(text: &str) -> Vec<RawJob> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let id = f.next()?.parse().ok()?;
            let unit = f.next()?.to_string();
            let kind = f.next()?.to_string();
            let state = f.next()?.to_string();
            Some(RawJob {
                id,
                unit,
                kind,
                state,
            })
        })
        .collect()
}

/// `systemctl --failed --plain --no-legend --no-pager`'s units: each line's first word, when
/// it holds a `.` (a unit name).
#[must_use]
pub fn parse_failed(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| unit.contains('.'))
        .map(str::to_string)
        .collect()
}

/// `/proc/uptime`'s first field, in milliseconds.
#[must_use]
pub fn parse_uptime_ms(text: &str) -> Option<u64> {
    let secs = text.split_whitespace().next()?;
    let (whole, frac) = secs.split_once('.').unwrap_or((secs, ""));
    let whole: u64 = whole.parse().ok()?;
    let mut ms = 0;
    for (i, digit) in frac.bytes().take(3).enumerate() {
        if !digit.is_ascii_digit() {
            return None;
        }
        ms += u64::from(digit - b'0') * [100, 10, 1][i];
    }
    whole.checked_mul(1000)?.checked_add(ms)
}

/// How far the guest's clock may stray from a request's before the agent steps it: 100 ms.
pub const CLOCK_SLACK_NS: u64 = 100_000_000;

/// Whether a request sent at `host_ns` steps a guest clock reading `guest_ns`.
#[must_use]
pub const fn needs_step(guest_ns: u64, host_ns: u64) -> bool {
    guest_ns.abs_diff(host_ns) > CLOCK_SLACK_NS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_jobs_reads_each_job_and_skips_what_is_not_one() {
        let text = "\
117 systemd-networkd-wait-online.service start running
 98 cloud-final.service                  start waiting
No jobs running.
x not.a job line
";
        assert_eq!(
            parse_jobs(text),
            [
                RawJob {
                    id: 117,
                    unit: "systemd-networkd-wait-online.service".into(),
                    kind: "start".into(),
                    state: "running".into(),
                },
                RawJob {
                    id: 98,
                    unit: "cloud-final.service".into(),
                    kind: "start".into(),
                    state: "waiting".into(),
                },
            ]
        );
        assert_eq!(parse_jobs(""), []);
    }

    #[test]
    fn failed_units_are_each_lines_unit() {
        let text = "fwupd.service loaded failed failed Firmware update daemon\n\
                    snapd.seeded.service loaded failed failed Wait until snapd is fully seeded\n";
        assert_eq!(
            parse_failed(text),
            ["fwupd.service", "snapd.seeded.service"]
        );
        assert_eq!(parse_failed("\n"), Vec::<String>::new());
    }

    #[test]
    fn uptime_reads_seconds_to_milliseconds() {
        assert_eq!(parse_uptime_ms("1.83 3.20\n"), Some(1830));
        assert_eq!(parse_uptime_ms("12345.6 1.0"), Some(12_345_600));
        assert_eq!(parse_uptime_ms("7 0"), Some(7000));
        assert_eq!(parse_uptime_ms("0.0049 0"), Some(4));
        assert_eq!(parse_uptime_ms("x"), None);
        assert_eq!(parse_uptime_ms(""), None);
    }

    #[test]
    fn only_a_clock_past_the_slack_is_stepped() {
        let host = 1_792_000_000_000_000_000;
        assert!(!needs_step(host, host));
        assert!(!needs_step(host - CLOCK_SLACK_NS, host));
        assert!(!needs_step(host + CLOCK_SLACK_NS, host));
        assert!(needs_step(host - CLOCK_SLACK_NS - 1, host));
        assert!(needs_step(host + CLOCK_SLACK_NS + 1, host));
        // The guest's measured lag: about two minutes behind (no RTC).
        assert!(needs_step(host - 120_000_000_000, host));
    }

    #[test]
    fn only_running_or_degraded_is_up() {
        let at = |state: &str| Booted {
            state: state.into(),
            failed_units: Vec::new(),
        };
        assert!(at("running").is_up());
        assert!(at("degraded").is_up());
        for other in [
            "starting",
            "maintenance",
            "stopping",
            "offline",
            "unknown",
            "",
        ] {
            assert!(!at(other).is_up(), "{other}");
        }
    }
}
