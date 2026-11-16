// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `schedule`: two per-user Scheduled Tasks, [`Task::Update`] running `update` once a day
//! and [`Task::Serve`] running `release-serve --once` every ten minutes.
//!
//! Each task is registered with `schtasks.exe /Create /XML` for the calling
//! account, at the least privilege: no administrator is asked. The XML says
//! what `schtasks`' flags cannot: the task starts on battery, is not stopped
//! when the PC goes on battery, and a start missed while the PC slept or was
//! off runs once it is back ([`CONDITIONS`]); a start while a run of it is
//! still going is passed over, so each runs once at a time. It does not wake
//! the PC.
//!
//! The tasks run a copy of this tool kept beside the build tree, so cleaning a
//! cargo target directory does not break them, name the repository by its
//! absolute path, so they do not depend on where they were started or built,
//! and run under `conhost.exe --headless`, so a build puts no console window on
//! the desktop.

use std::path::Path;

/// The daily update task's name.
pub const TASK: &str = "aterm-winship-update";
/// The release-serve task's name.
pub const SERVE_TASK: &str = "aterm-winship-release-serve";
/// How often the release-serve task starts, as Task Scheduler writes a duration.
pub const SERVE_EVERY: &str = "PT10M";
/// When the update task runs when `--daily` names no time.
pub const DEFAULT_TIME: &str = "04:30";
/// What a registered task does, for the person who registered it.
pub const CONDITIONS: &str = "runs while you are logged on, on battery too, is not stopped when the PC goes on battery, \
     and a start missed while the PC slept or was off runs once it is back (the PC is not woken for it)";

/// One of the two tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// `update --log`, daily.
    Update,
    /// `release-serve --once --log`, every [`SERVE_EVERY`].
    Serve,
}

impl Task {
    /// Both tasks, in the order they are registered.
    pub const ALL: [Self; 2] = [Self::Update, Self::Serve];

    /// Its name in Task Scheduler.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Update => TASK,
            Self::Serve => SERVE_TASK,
        }
    }

    /// The verb and flags it runs the kept tool with, before `--repo`.
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Update => "update --log",
            Self::Serve => "release-serve --once --log",
        }
    }

    /// When it runs, for the person who registers it.
    #[must_use]
    pub const fn when(self) -> &'static str {
        match self {
            Self::Update => "daily",
            Self::Serve => "every 10 minutes",
        }
    }

    /// What it does, as its description in Task Scheduler.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::Update => {
                "Builds, smokes and stages aterm's origin/main once a day (aterm-winship update --log)."
            }
            Self::Serve => {
                "Builds, signs and hands off the Windows zip a release cut asks for through the dev repository, every ten minutes (aterm-winship release-serve --once --log)."
            }
        }
    }
}

/// A `--daily` time: `HH:MM`, 24-hour.
///
/// # Errors
/// Not two digits, a colon and two digits naming a minute of the day.
pub fn time(text: &str) -> Result<String, String> {
    minute_of_day(text).map(|_| text.to_string())
}

fn minute_of_day(text: &str) -> Result<u64, String> {
    let bad = || format!("--daily {text}: a 24-hour time, HH:MM");
    let (h, m) = text.split_once(':').ok_or_else(bad)?;
    let two = |s: &str, below: u64| {
        (s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u64>().ok())
            .flatten()
            .filter(|n| *n < below)
    };
    match (two(h, 24), two(m, 60)) {
        (Some(h), Some(m)) => Ok(h * 60 + m),
        _ => Err(bad()),
    }
}

fn local_text(secs: u64) -> String {
    crate::pointer::rfc3339(secs)
        .trim_end_matches('Z')
        .to_string()
}

/// The update task's first start, `YYYY-MM-DDTHH:MM:00` on the local clock: the next
/// `at` after `local_now` (seconds since the epoch, shifted to the local
/// clock). Never a time already past: a task that starts when a start was
/// missed would take a boundary in the past as one, and build the moment it
/// is registered.
///
/// # Errors
/// `at` is not a [`time`].
pub fn start_boundary(local_now: u64, at: &str) -> Result<String, String> {
    const DAY: u64 = 86_400;
    let at = minute_of_day(at)? * 60;
    let midnight = local_now - local_now % DAY;
    let start = if local_now % DAY < at {
        midnight + at
    } else {
        midnight + DAY + at
    };
    Ok(local_text(start))
}

/// The serve task's first start on the local clock: the start of the second whole minute after
/// `local_now`, never already past when Task Scheduler reads it.
#[must_use]
pub fn soon_boundary(local_now: u64) -> String {
    local_text((local_now / 60 + 2) * 60)
}

/// `text` as one argument of a Windows command line, by the rule the C runtime
/// and Rust's `std::env::args` read it back with: quoted, and a run of
/// backslashes before the closing quote doubled, so `C:\src\aterm\` is not read
/// as a path that ends in a quote.
///
/// # Errors
/// `text` holds a double quote, which no path can.
pub fn quoted(text: &str) -> Result<String, String> {
    if text.contains('"') {
        return Err(format!(
            "{text}: a path with a double quote cannot be scheduled"
        ));
    }
    let tail = text.len() - text.trim_end_matches('\\').len();
    Ok(format!("\"{text}{}\"", "\\".repeat(tail)))
}

/// The arguments `conhost.exe` is started with for `task`: `--headless`, then the kept
/// tool's [`Task::verb`] and `--repo <repo>`.
///
/// # Errors
/// A path holds a double quote, or `repo` is not absolute (the task starts in
/// the system directory, where a relative path names nothing).
pub fn arguments(task: Task, tool: &Path, repo: &Path) -> Result<String, String> {
    let repo = repo.to_string_lossy();
    if !windows_absolute(&repo) {
        return Err(format!("the repository {repo} is not an absolute path"));
    }
    Ok(format!(
        "--headless {} {} --repo {}",
        quoted(&tool.to_string_lossy())?,
        task.verb(),
        quoted(&repo)?
    ))
}

/// Whether `text` is an absolute Windows path, `X:\…` or `\\server\…`. Read
/// from the text, so the answer is the same on the hosts that only test this.
fn windows_absolute(text: &str) -> bool {
    let bytes = text.as_bytes();
    text.starts_with(r"\\")
        || matches!(bytes, [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
}

fn xml_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The task's trigger from `start`: daily for [`Task::Update`]; every [`SERVE_EVERY`], with
/// no end, for [`Task::Serve`].
fn trigger(task: Task, start: &str) -> String {
    match task {
        Task::Update => format!(
            "    <CalendarTrigger>
      <StartBoundary>{start}</StartBoundary>
      <Enabled>true</Enabled>
      <ScheduleByDay>
        <DaysInterval>1</DaysInterval>
      </ScheduleByDay>
    </CalendarTrigger>"
        ),
        Task::Serve => format!(
            "    <TimeTrigger>
      <Repetition>
        <Interval>{SERVE_EVERY}</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>{start}</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>"
        ),
    }
}

/// `task` as Task Scheduler's XML: started from `start` ([`start_boundary`] for the update,
/// [`soon_boundary`] for the serve task) on its trigger, for `user` (`DOMAIN\name`; the
/// caller when `None`) at the least privilege with the token of their logon, running `host`
/// (`conhost.exe`) with [`arguments`].
///
/// # Errors
/// What [`arguments`] refuses.
pub fn task_xml(
    task: Task,
    host: &Path,
    tool: &Path,
    repo: &Path,
    start: &str,
    user: Option<&str>,
) -> Result<String, String> {
    let arguments = arguments(task, tool, repo)?;
    let user = user.map_or_else(String::new, |user| {
        format!("\n      <UserId>{}</UserId>", xml_text(user))
    });
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{}</Description>
  </RegistrationInfo>
  <Triggers>
{}
  </Triggers>
  <Principals>
    <Principal id="Author">{user}
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT4H</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{}</Command>
      <Arguments>{}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        xml_text(task.what()),
        trigger(task, start),
        xml_text(&host.to_string_lossy()),
        xml_text(&arguments),
    ))
}

/// `text` as the bytes `schtasks.exe /XML` reads: UTF-16, little-endian, led
/// by a byte-order mark.
#[must_use]
pub fn utf16(text: &str) -> Vec<u8> {
    std::iter::once(0xFEFF)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// `schtasks.exe`'s arguments that register `task` from the XML file `xml`
/// (replacing one of its name).
#[must_use]
pub fn create_args(task: Task, xml: &Path) -> Vec<String> {
    let mut args = ["/Create", "/F", "/TN", task.name(), "/XML"]
        .map(String::from)
        .to_vec();
    args.push(xml.to_string_lossy().into_owned());
    args
}

/// `schtasks.exe`'s arguments that ask whether `task` is registered.
#[must_use]
pub fn query_args(task: Task) -> Vec<String> {
    ["/Query", "/TN", task.name()].map(String::from).to_vec()
}

/// `schtasks.exe`'s arguments that remove `task`.
#[must_use]
pub fn delete_args(task: Task) -> Vec<String> {
    ["/Delete", "/F", "/TN", task.name()]
        .map(String::from)
        .to_vec()
}

/// The command line as a person would type it at `cmd.exe`, for `--dry-run`.
#[must_use]
pub fn shown(args: &[String]) -> String {
    let mut line = "schtasks.exe".to_string();
    for arg in args {
        line.push(' ');
        if arg.contains([' ', '"']) {
            line.push('"');
            line.push_str(&arg.replace('"', "\\\""));
            line.push('"');
        } else {
            line.push_str(arg);
        }
    }
    line
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
