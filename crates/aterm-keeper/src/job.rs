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
/// The installed app's executable.
pub const INSTALLED_EXECUTABLE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

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

/// Returns the keeper label and socket name for executable `exe`.
#[must_use]
pub fn place_for(exe: &Path) -> KeeperPlace {
    if exe == Path::new(INSTALLED_EXECUTABLE) {
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

/// Read a job's state with `launchctl print gui/<uid>/<label>` (read-only).
///
/// # Errors
/// `launchctl` failing to run at all.
#[cfg(unix)]
pub fn state(label: &str) -> io::Result<JobState> {
    let out = aterm_uds::spawnfd::command("/bin/launchctl")
        .arg("print")
        .arg(format!("{}/{label}", gui_domain()))
        .output()?;
    if !out.status.success() {
        return Ok(JobState::Absent);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let pid = text.lines().find_map(|l| {
        let l = l.trim();
        l.strip_prefix("pid = ").and_then(|p| p.trim().parse().ok())
    });
    Ok(JobState::Loaded { pid })
}

fn run_launchctl(argv: &[String]) -> io::Result<()> {
    let out = aterm_uds::spawnfd::command("/bin/launchctl")
        .args(argv)
        .output()?;
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
mod tests {
    use super::*;

    #[test]
    fn the_installed_app_and_dev_builds_never_share_a_keeper() {
        let installed = place_for(Path::new(INSTALLED_EXECUTABLE));
        assert_eq!(installed.label, INSTALLED_LABEL);
        assert_eq!(installed.socket_name, "keeper.sock");
        assert!(installed.installed);
        let dev = place_for(Path::new("/Users//x/aterm/target/debug/aterm"));
        assert!(dev.label.starts_with(DEV_LABEL_PREFIX), "{dev:?}");
        assert!(!dev.installed);
        assert_ne!(dev.label, installed.label);
        assert_ne!(dev.socket_name, installed.socket_name);
        assert_eq!(
            dev,
            place_for(Path::new("/Users//x/aterm/target/debug/aterm"))
        );
    }

    #[test]
    fn the_bundle_is_read_off_the_executable_path() {
        assert_eq!(
            own_bundle(Path::new(INSTALLED_EXECUTABLE)),
            Some(PathBuf::from("/Applications/aterm.app"))
        );
        assert_eq!(own_bundle(Path::new("/Users//x/target/debug/aterm")), None);
        assert_eq!(own_bundle(Path::new("/x/Contents/MacOS/aterm")), None);
    }

    #[test]
    fn the_submit_argv_is_plain() {
        assert_eq!(
            submit_argv("l", "/bin/sleep", &["600"], Path::new("/p/k.log")),
            [
                "submit",
                "-l",
                "l",
                "-e",
                "/p/k.log",
                "--",
                "/bin/sleep",
                "600"
            ]
        );
        assert_eq!(
            log_path(Path::new("/p/keeper-dev-0123abcd.sock")),
            Path::new("/p/keeper-dev-0123abcd.sock.log")
        );
        assert_eq!(
            log_path(Path::new("/p/k.log")),
            Path::new("/p/k.log.log"),
            "never the socket itself"
        );
    }

    /// A test-labelled job is submitted into the GUI domain, runs with its stderr in the log,
    /// and after `remove` launchd knows no such job; a guard removes it if an assertion fails.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_test_job_is_submitted_and_removed_without_a_trace() {
        use std::os::unix::fs::DirBuilderExt as _;
        struct Guard(String);
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = remove(&self.0);
            }
        }
        let label = format!("{TEST_LABEL_PREFIX}.{}.jobtest", std::process::id());
        assert_ne!(label, INSTALLED_LABEL);
        assert_eq!(state(&label).expect("launchctl"), JobState::Absent);
        let dir = std::env::temp_dir().join(format!("akj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .expect("dir");
        let log = log_path(&dir.join("k.sock"));
        let guard = Guard(label.clone());
        submit(
            &label,
            "/bin/sh",
            &["-c", "echo keeper-job-stderr >&2; exec /bin/sleep 600"],
            &log,
        )
        .expect("submit");
        let started = std::time::Instant::now();
        let pid = loop {
            if let JobState::Loaded { pid: Some(pid) } = state(&label).expect("launchctl") {
                break pid;
            }
            assert!(started.elapsed().as_secs() < 60, "the job never ran");
            std::thread::yield_now();
        };
        assert!(aterm_uds::process::pid_alive(pid));
        let started = std::time::Instant::now();
        while !std::fs::read_to_string(&log).is_ok_and(|t| t.contains("keeper-job-stderr")) {
            assert!(
                started.elapsed().as_secs() < 60,
                "the job's stderr never reached {}",
                log.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        remove(&label).expect("remove");
        let started = std::time::Instant::now();
        loop {
            let now = state(&label).expect("launchctl");
            if now == JobState::Absent {
                break;
            }
            assert!(started.elapsed().as_secs() < 60, "never gone: {now:?}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::mem::forget(guard);
        let started = std::time::Instant::now();
        while aterm_uds::process::pid_alive(pid) {
            assert!(
                started.elapsed().as_secs() < 60,
                "the job's process outlived it"
            );
            std::thread::yield_now();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
