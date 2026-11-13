// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Developer ID signature verification via `/usr/bin/codesign --verify --strict`.

use std::path::Path;
use std::time::{Duration, Instant};

use aterm_uds::deadline::{RunError, output_until};

/// Absolute path to Apple's `codesign` tool.
pub const CODESIGN: &str = aterm_uds::os_path!("codesign");

/// Base verification timeout and floor for [`verify_timeout_for`].
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(60);

/// Minimum assumed read rate in bytes per second for [`verify_timeout_for`].
pub const VERIFY_MIN_BYTES_PER_SEC: u64 = 2 * 1024 * 1024;

/// Maximum timeout [`verify_timeout_for`] grants a single file.
pub const VERIFY_TIMEOUT_MAX: Duration = Duration::from_secs(600);

/// Returns the verification timeout for a `len`-byte file, capped at [`VERIFY_TIMEOUT_MAX`].
#[must_use]
pub fn verify_timeout_for(len: u64) -> Duration {
    VERIFY_TIMEOUT
        .saturating_add(Duration::from_secs(len / VERIFY_MIN_BYTES_PER_SEC))
        .min(VERIFY_TIMEOUT_MAX)
}

/// How much of codesign's stderr a refusal keeps: the tail, and more than a pipe holds,
/// so no diagnostic a finishing child could write is cut.
const STDERR_KEEP: usize = 64 * 1024;

/// Why a Developer ID verification did not pass.
#[derive(Debug)]
pub enum CodesignError {
    /// Team ID is empty or not ASCII alphanumeric.
    InvalidTeam(String),
    /// Not macOS.
    Unsupported,
    /// `codesign` could not be started.
    Spawn(std::io::Error),
    /// Waiting for or capturing output from a started `codesign` failed.
    Wait(std::io::Error),
    /// `codesign` did not finish before the deadline and was killed.
    TimedOut,
    /// `codesign` ran and exited non-zero (`code` is `None` when killed by a signal).
    Refused { code: Option<i32>, stderr: String },
}

impl std::fmt::Display for CodesignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTeam(team) => write!(f, "team id {team:?} is not alphanumeric"),
            Self::Unsupported => f.write_str("codesign exists only on macOS"),
            Self::Spawn(e) => write!(f, "spawn {CODESIGN}: {e}"),
            Self::Wait(e) => write!(f, "wait for {CODESIGN}: {e}"),
            Self::TimedOut => write!(f, "{CODESIGN} did not finish in time"),
            Self::Refused { code, stderr } => {
                match code {
                    Some(code) => write!(f, "{CODESIGN} refused (exit {code})")?,
                    None => write!(f, "{CODESIGN} was ended by a signal")?,
                }
                match stderr.lines().rev().map(str::trim).find(|l| !l.is_empty()) {
                    Some(line) => write!(f, ": {line}"),
                    None => Ok(()),
                }
            }
        }
    }
}

impl std::error::Error for CodesignError {}

/// codesign's words (Security.framework's `errSecCS…` strings) for a signature fault in the
/// bytes it read; exit 1 also reports an unreadable file or an internal failure, which are not
/// verdicts.
const SIGNATURE_FAULTS: [&str; 12] = [
    "code object is not signed at all",
    "invalid signature (code or signature have been modified)",
    "invalid or unsupported format for signature",
    "unsupported type or version of signature",
    "main executable failed strict validation",
    "object file format unrecognized, invalid, or unsuitable",
    "a sealed resource is missing or invalid",
    "unsealed contents present in the bundle root",
    "invalid Info.plist (plist or signature have been modified)",
    "invalid resource directory (directory or signature have been modified)",
    "bundle format unrecognized, invalid, or unsuitable",
    "invalid destination for symbolic link in bundle",
];

/// codesign's words for a failure to read or run, as the `<path>: <fault>` first line of an
/// exit-1 refusal: facts about this machine, never about the bytes
/// ([`CodesignError::judged_nothing_at`]).
const MACHINE_FAULTS: [&str; 7] = [
    "Permission denied",
    "internal error in Code Signing subsystem",
    "the code cannot be read by the verifier (file system permissions etc.)",
    "Resource temporarily unavailable",
    "Operation not permitted",
    "Too many open files",
    "Too many open files in system",
];

impl CodesignError {
    /// Returns whether this error is a definitive signature verdict on the bytes (exit 3 or a signature fault on exit 1).
    #[must_use]
    pub fn is_verdict(&self) -> bool {
        match self {
            Self::Refused { code: Some(3), .. } => true,
            Self::Refused {
                code: Some(1),
                stderr,
            } => stderr.lines().any(|line| {
                let line = line.trim_end();
                SIGNATURE_FAULTS.iter().any(|fault| line.ends_with(fault))
            }),
            _ => false,
        }
    }

    /// Returns whether `codesign` on `verified` terminated by signal or failed on a known machine read/run fault without judging the bytes.
    #[must_use]
    pub fn judged_nothing_at(&self, verified: &Path) -> bool {
        match self {
            Self::Refused { code: None, .. } => true,
            Self::Refused {
                code: Some(1),
                stderr,
            } => {
                let prefix = format!("{}: ", verified.display());
                let first = stderr.lines().map(str::trim).find(|l| !l.is_empty());
                stderr.len() < STDERR_KEEP
                    && !self.is_verdict()
                    && first
                        .and_then(|first| first.strip_prefix(prefix.as_str()))
                        .is_some_and(|fault| MACHINE_FAULTS.contains(&fault))
            }
            _ => false,
        }
    }
}

/// Builds Apple's Developer ID designated requirement string pinned to `team`.
///
/// # Errors
/// [`CodesignError::InvalidTeam`] for an empty or non-alphanumeric `team`.
pub fn developer_id_requirement(team: &str) -> Result<String, CodesignError> {
    if team.is_empty() || !team.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(CodesignError::InvalidTeam(team.to_string()));
    }
    Ok(format!(
        "=anchor apple generic \
         and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{team}\""
    ))
}

/// [`verify_developer_id_until`] within [`aterm_uds::deadline::CODESIGN_HANG`]: a test's
/// codesign run must finish, so its bound is the hang detector, not the product's budget.
#[cfg(test)]
fn verify_developer_id(path: &Path, team: &str, deep: bool) -> Result<(), CodesignError> {
    verify_developer_id_until(
        path,
        team,
        deep,
        Instant::now() + aterm_uds::deadline::CODESIGN_HANG,
    )
}

/// Verifies that `path` is signed with a Developer ID of `team` before `deadline`.
///
/// # Errors
/// [`CodesignError::InvalidTeam`] before anything runs; [`CodesignError::Unsupported`]
/// off macOS; otherwise the spawn, wait, timeout or refusal, each a rejection.
pub fn verify_developer_id_until(
    path: &Path,
    team: &str,
    deep: bool,
    deadline: Instant,
) -> Result<(), CodesignError> {
    let mut cmd = developer_id_command(path, team, deep)?;
    developer_id_verdict(output_until(&mut cmd, deadline))
}

/// Builds the `codesign` command used by [`verify_developer_id_until`].
///
/// # Errors
/// [`CodesignError::InvalidTeam`] before anything is built; [`CodesignError::Unsupported`] off macOS.
pub fn developer_id_command(
    path: &Path,
    team: &str,
    deep: bool,
) -> Result<aterm_uds::spawnfd::Command, CodesignError> {
    let requirement = developer_id_requirement(team)?;
    if !cfg!(target_os = "macos") {
        return Err(CodesignError::Unsupported);
    }
    let mut cmd = aterm_uds::spawnfd::command(CODESIGN);
    cmd.arg("--verify");
    if deep {
        cmd.arg("--deep");
    }
    cmd.args(["--strict", "--verbose=2", "-R", &requirement])
        .arg(path);
    Ok(cmd)
}

/// Evaluates the output of [`developer_id_command`].
///
/// # Errors
/// Returns [`CodesignError`] on spawn/wait error, timeout, or non-zero exit.
pub fn developer_id_verdict(
    run: Result<std::process::Output, RunError>,
) -> Result<(), CodesignError> {
    let out = run.map_err(|e| match e {
        RunError::Spawn(e) => CodesignError::Spawn(e),
        RunError::Capture(e) => {
            CodesignError::Wait(std::io::Error::other(format!("capture after start: {e}")))
        }
        RunError::Wait(e) => CodesignError::Wait(e),
        RunError::TimedOut => CodesignError::TimedOut,
    })?;
    if out.status.success() {
        Ok(())
    } else {
        let tail = out.stderr.len().saturating_sub(STDERR_KEEP);
        Err(CodesignError::Refused {
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr[tail..]).into_owned(),
        })
    }
}

#[cfg(test)]
#[path = "codesign_tests.rs"]
mod tests;
#[test]
fn capture_failure_after_start_is_not_spawn_or_a_signature_verdict() {
    let error = developer_id_verdict(Err(RunError::Capture(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "capture setup failed after dispatch",
    ))))
    .unwrap_err();
    assert!(matches!(&error, CodesignError::Wait(_)));
    assert!(!error.is_verdict());
    assert!(!error.judged_nothing_at(Path::new("A.app")));
    assert!(error.to_string().contains("capture after start"));
}
