// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keeper launchd label, socket path, and transient `launchctl` job control.

use std::io;
use std::path::{Path, PathBuf};

/// The installed app's keeper label.
pub const INSTALLED_LABEL: &str = "com.aterm.aterm.keeper";
/// The prefix of a dev build's label: `<prefix>.<hash of the executable path>`.
pub const DEV_LABEL_PREFIX: &str = "com.aterm.aterm.keeper.dev";
/// The prefix of a test's label: `<prefix>.<pid>.<n>` — never the installed one.
pub const TEST_LABEL_PREFIX: &str = "com.aterm.aterm.keeper.test";
/// The installed app's executable in `/Applications`; a person who cannot write there gets it in
/// `~/Applications` ([`installed_executables`]).
pub const INSTALLED_EXECUTABLE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

/// Where aterm installs itself, as the executable in each: `/Applications`
/// ([`INSTALLED_EXECUTABLE`]), else `~/Applications` under `home` (install.sh and the first
/// launch pick the second when the first is not writable). Each is the person's own copy.
#[must_use]
pub fn installed_executables(home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from(INSTALLED_EXECUTABLE)];
    if let Some(home) = home.filter(|h| h.is_absolute()) {
        out.push(
            home.join("Applications")
                .join("aterm.app")
                .join("Contents")
                .join("MacOS")
                .join("aterm"),
        );
    }
    out
}

/// Which keeper a build talks to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeeperPlace {
    /// The launchd label.
    pub label: String,
    /// The socket's file name inside the keeper directory.
    pub socket_name: String,
    /// Whether this is the installed app's keeper.
    pub installed: bool,
}

/// Returns the keeper label and socket name for executable `exe`, under this login's `HOME`
/// ([`place_in`]).
#[must_use]
pub fn place_for(exe: &Path) -> KeeperPlace {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    place_in(exe, home.as_deref())
}

/// [`place_for`]'s pure core: an [`installed_executables`] copy under `home` is the installed
/// app's keeper; any other executable gets its own dev place.
#[must_use]
pub fn place_in(exe: &Path, home: Option<&Path>) -> KeeperPlace {
    if installed_executables(home).iter().any(|p| p == exe) {
        return KeeperPlace {
            label: INSTALLED_LABEL.to_string(),
            socket_name: "keeper.sock".to_string(),
            installed: true,
        };
    }
    let hash = aterm_hash::fnv1a_64(exe.as_os_str().as_encoded_bytes());
    let short = format!("{:08x}", (hash >> 32) as u32 ^ hash as u32);
    KeeperPlace {
        label: format!("{DEV_LABEL_PREFIX}.{short}"),
        socket_name: format!("keeper-dev-{short}.sock"),
        installed: false,
    }
}

/// The keeper `std::env::current_exe` belongs to.
#[must_use]
pub fn own_place() -> KeeperPlace {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .unwrap_or_default();
    place_for(&exe)
}

/// Per-uid private directory holding the keeper socket.
#[must_use]
pub fn keeper_dir() -> Option<PathBuf> {
    admitted_temp_base()
        .map(|b| b.join("aterm"))
        .or_else(|| aterm_uds::control_socket_dir().filter(|d| d.is_absolute()))
}

#[cfg(unix)]
fn admitted_temp_base() -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let base = std::env::temp_dir();
    if !base.is_absolute() || base.starts_with("/tmp") || base.starts_with("/private/tmp") {
        return None;
    }
    let meta = std::fs::symlink_metadata(&base).ok()?;
    if !meta.file_type().is_dir()
        || meta.uid() != aterm_uds::peer::our_uid()
        || meta.permissions().mode() & 0o022 != 0
    {
        return None;
    }
    Some(base)
}

#[cfg(not(unix))]
fn admitted_temp_base() -> Option<PathBuf> {
    None
}

/// The socket path of the keeper `place` names.
#[must_use]
pub fn socket_path(place: &KeeperPlace) -> Option<PathBuf> {
    keeper_dir().map(|d| d.join(&place.socket_name))
}

/// The user's GUI domain target, `gui/<uid>`.
#[cfg(unix)]
#[must_use]
pub fn gui_domain() -> String {
    format!("gui/{}", aterm_uds::peer::our_uid())
}

/// Log file path for the keeper job beside `socket` (`<socket>.log`).
#[must_use]
pub fn log_path(socket: &Path) -> PathBuf {
    let mut name = socket.as_os_str().to_owned();
    name.push(".log");
    PathBuf::from(name)
}

/// Builds the `launchctl submit` argument vector for `label` with stderr redirected to `stderr`.
#[must_use]
pub fn submit_argv(label: &str, program: &str, args: &[&str], stderr: &Path) -> Vec<String> {
    let mut argv = vec![
        "submit".to_string(),
        "-l".to_string(),
        label.to_string(),
        "-e".to_string(),
        stderr.to_string_lossy().into_owned(),
        "--".to_string(),
        program.to_string(),
    ];
    argv.extend(args.iter().map(|a| (*a).to_string()));
    argv
}

/// Submits a transient KeepAlive job with stderr redirected to `stderr`.
///
/// # Errors
/// `launchctl` failing to run or refusing.
pub fn submit(label: &str, program: &str, args: &[&str], stderr: &Path) -> io::Result<()> {
    run_launchctl(&submit_argv(label, program, args, stderr))
}

/// End a submitted job (launchd sends it SIGTERM) and forget it.
///
/// # Errors
/// `launchctl` failing to run or refusing (a label it does not know).
pub fn remove(label: &str) -> io::Result<()> {
    run_launchctl(&["remove".to_string(), label.to_string()])
}

/// What launchd says of a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    /// launchd knows no such job in the GUI domain.
    Absent,
    /// It is loaded; `pid` when it is running now.
    Loaded { pid: Option<u32> },
}

/// How long one `launchctl` may take before it is killed and its call fails.
const LAUNCHCTL_HANG: std::time::Duration = std::time::Duration::from_secs(60);

fn launchctl_output(
    command: &mut aterm_uds::spawnfd::Command,
    started: std::time::Instant,
) -> Result<std::process::Output, aterm_uds::deadline::RunError> {
    let deadline = started + LAUNCHCTL_HANG;
    aterm_uds::deadline::output_until(command, deadline)
}

fn launchctl_io_error(err: aterm_uds::deadline::RunError) -> io::Error {
    match err {
        aterm_uds::deadline::RunError::Spawn(e) => e,
        other => io::Error::other(other),
    }
}

/// Read a job's state with `launchctl print gui/<uid>/<label>` (read-only).
///
/// # Errors
/// `launchctl` failing to run at all.
#[cfg(unix)]
pub fn state(label: &str) -> io::Result<JobState> {
    let mut cmd = aterm_uds::spawnfd::command("/bin/launchctl");
    cmd.arg("print").arg(format!("{}/{label}", gui_domain()));
    let out = launchctl_output(&mut cmd, std::time::Instant::now()).map_err(launchctl_io_error)?;
    if !out.status.success() {
        return Ok(JobState::Absent);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let pid = text.lines().find_map(|l| {
        let top = l.strip_prefix('\t')?;
        let s = top.strip_prefix("pid = ")?.trim();
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
            .filter(|p| *p > 0)
    });
    Ok(JobState::Loaded { pid })
}

fn run_launchctl(argv: &[String]) -> io::Result<()> {
    let mut cmd = aterm_uds::spawnfd::command("/bin/launchctl");
    cmd.args(argv);
    let out = launchctl_output(&mut cmd, std::time::Instant::now()).map_err(launchctl_io_error)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "launchctl {}: {}{}",
            argv.first().map_or("", String::as_str),
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        )))
    }
}

/// Returns the `.app` bundle directory containing `exe`, or `None` outside a bundle.
#[must_use]
pub fn own_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension().is_some_and(|e| e == "app");
    is_bundle.then(|| bundle.to_path_buf())
}

#[cfg(test)]
#[path = "job_tests.rs"]
mod tests;
