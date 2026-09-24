// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Developer ID gate: `/usr/bin/codesign --verify --strict` under a designated
//! requirement that pins Apple's anchor, the Developer ID chain and one Team ID.
//!
//! ONE requirement text for every caller — the app updater's bundle check and atpkg's
//! staged agent binaries — so the two cannot disagree about what "signed by team X"
//! means. The spawn is bounded by a deadline and fails closed: a timeout, a spawn
//! failure or a non-zero exit is never a pass.

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Absolute, so no `PATH` entry can stand in for Apple's tool.
pub const CODESIGN: &str = "/usr/bin/codesign";

/// The ceiling on one verification when the caller brings no deadline of its own.
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// The first child-exit poll, doubled up to [`POLL_MAX`]: codesign on one binary takes
/// tens of milliseconds, and a fixed tick would round every call up to it.
const POLL_MIN: Duration = Duration::from_millis(1);
const POLL_MAX: Duration = Duration::from_millis(25);

/// How much of codesign's stderr a refusal keeps: the tail, and more than a pipe holds,
/// so no diagnostic a finishing child could write is cut.
const STDERR_KEEP: usize = 64 * 1024;

/// Why a Developer ID verification did not pass.
#[derive(Debug)]
pub enum CodesignError {
    /// The team id is empty or not ASCII alphanumeric; refused before it can reach the
    /// requirement text (a `"` there would rewrite the requirement).
    InvalidTeam(String),
    /// Not macOS: there is no codesign here, so no platform anchor to check.
    Unsupported,
    /// codesign could not be started.
    Spawn(std::io::Error),
    /// Waiting for codesign failed.
    Wait(std::io::Error),
    /// codesign did not finish by the deadline; it was killed and reaped.
    TimedOut,
    /// codesign ran and refused. `code` is its exit status (`None` when a signal ended
    /// it): 1 is a signature that does not verify, 3 a valid signature that fails the
    /// requirement, 2 bad arguments. `stderr` is its diagnostic, untrimmed.
    Refused { code: Option<i32>, stderr: String },
}

impl std::fmt::Display for CodesignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTeam(team) => write!(f, "team id {team:?} is not alphanumeric"),
            Self::Unsupported => f.write_str("codesign exists only on macOS"),
            Self::Spawn(e) => write!(f, "spawn {CODESIGN}: {e}"),
            Self::Wait(e) => write!(f, "wait for {CODESIGN}: {e}"),
            Self::TimedOut => write!(
                f,
                "{CODESIGN} did not finish in time; treating as a rejection"
            ),
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

/// codesign's words (Security.framework's `errSecCS…` strings) for a signature fault in
/// the bytes it read. Exit 1 also reports a file it could not read or an internal
/// failure, and those are facts about this machine, not verdicts on the bytes.
const SIGNATURE_FAULTS: [&str; 6] = [
    "code object is not signed at all",
    "invalid signature (code or signature have been modified)",
    "invalid or unsupported format for signature",
    "unsupported type or version of signature",
    "main executable failed strict validation",
    "object file format unrecognized, invalid, or unsuitable",
];

impl CodesignError {
    /// Whether this is codesign's verdict on the bytes — a valid signature that fails the
    /// requirement (exit 3), or exit 1 naming a signature fault ([`SIGNATURE_FAULTS`]) —
    /// rather than a failure to judge them: an unreadable file, an internal error, a
    /// signal, a timeout. Only a verdict is stable enough to remember.
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
}

/// Apple's Developer ID designated requirement, pinned to `team`. The leading `=` makes
/// `-R` read it as requirement source rather than a file name. The two marker OIDs are
/// the Developer ID intermediate CA (`…6.2.6`) and the Developer ID Application leaf
/// (`…6.1.13`); `anchor apple generic` requires the chain to end at Apple's root, so a
/// self-signed certificate that merely claims the team's OU cannot pass.
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

/// Verify that `path` is signed with a Developer ID of `team`, within [`VERIFY_TIMEOUT`].
/// `deep` also verifies nested code (a bundle); a single Mach-O needs none.
///
/// # Errors
/// See [`verify_developer_id_until`].
pub fn verify_developer_id(path: &Path, team: &str, deep: bool) -> Result<(), CodesignError> {
    verify_developer_id_until(path, team, deep, Instant::now() + VERIFY_TIMEOUT)
}

/// [`verify_developer_id`], finished by `deadline` — for a caller whose own budget binds
/// sooner than [`VERIFY_TIMEOUT`].
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
    let requirement = developer_id_requirement(team)?;
    if !cfg!(target_os = "macos") {
        return Err(CodesignError::Unsupported);
    }
    let mut cmd = Command::new(CODESIGN);
    cmd.arg("--verify");
    if deep {
        cmd.arg("--deep");
    }
    cmd.args(["--strict", "--verbose=2", "-R", &requirement])
        .arg(path);
    let (status, stderr) = run_bounded(&mut cmd, deadline)?;
    if status.success() {
        Ok(())
    } else {
        Err(CodesignError::Refused {
            code: status.code(),
            stderr,
        })
    }
}

/// Run `cmd` to completion by `deadline`, keeping the tail of its stderr. The stream is
/// drained on its own thread so a chatty child cannot wedge on a full pipe; a child still
/// running at the deadline is killed and reaped before [`CodesignError::TimedOut`].
fn run_bounded(
    cmd: &mut Command,
    deadline: Instant,
) -> Result<(ExitStatus, String), CodesignError> {
    use std::io::Read as _;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(CodesignError::Spawn)?;
    let drain = match child.stderr.take() {
        Some(mut stream) => {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let spawned = std::thread::Builder::new()
                .name("codesign-stderr".to_string())
                .spawn(move || {
                    let mut tail: Vec<u8> = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while let Ok(n) = stream.read(&mut chunk) {
                        if n == 0 {
                            break;
                        }
                        tail.extend_from_slice(&chunk[..n]);
                        if tail.len() > STDERR_KEEP {
                            let cut = tail.len() - STDERR_KEEP;
                            tail.drain(..cut);
                        }
                    }
                    let _ = tx.send(String::from_utf8_lossy(&tail).into_owned());
                });
            if let Err(e) = spawned {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CodesignError::Spawn(e));
            }
            Some(rx)
        }
        None => None,
    };
    let mut poll = POLL_MIN;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CodesignError::TimedOut);
                }
                std::thread::sleep(poll.min(remaining));
                poll = (poll * 2).min(POLL_MAX);
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CodesignError::Wait(e));
            }
        }
    };
    // The child has exited, so its end of the pipe is closed and the drain finishes
    // promptly; the short floor only covers a deadline the exit itself used up.
    let grace = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(250));
    let stderr = drain
        .and_then(|rx| rx.recv_timeout(grace).ok())
        .unwrap_or_default();
    Ok((status, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The requirement is Apple's Developer ID shape with the team spliced in verbatim —
    /// the exact text the app updater has always passed to `-R`.
    #[test]
    fn the_requirement_pins_anchor_chain_and_team() {
        assert_eq!(
            developer_id_requirement("Q6L2SF6YDW").unwrap(),
            "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
             and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate \
             leaf[subject.OU] = \"Q6L2SF6YDW\""
        );
    }

    /// A team that could rewrite the requirement — or say nothing — is refused before
    /// anything runs, on every platform.
    #[test]
    fn a_team_that_is_not_alphanumeric_is_refused_before_any_spawn() {
        for bad in ["", "Q6L2\"SF6YD", "ABC DEF", "ÄBCDEFGHIJ", "A\"or\"B"] {
            assert!(
                matches!(
                    verify_developer_id(Path::new("/nonexistent"), bad, false),
                    Err(CodesignError::InvalidTeam(_))
                ),
                "{bad:?}"
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_there_is_no_anchor_to_check() {
        assert!(matches!(
            verify_developer_id(Path::new("/bin/sh"), "Q6L2SF6YDW", false),
            Err(CodesignError::Unsupported)
        ));
    }

    /// An unsigned file is a refusal (exit 1), never a pass, and the refusal carries
    /// codesign's own words.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_unsigned_file_is_refused_with_codesigns_reason() {
        let dir = std::env::temp_dir().join(format!("codesign-unsigned-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("not-code");
        std::fs::write(&file, b"#!/bin/sh\necho hi\n").unwrap();
        match verify_developer_id(&file, "Q6L2SF6YDW", false) {
            Err(CodesignError::Refused { code, stderr }) => {
                assert_eq!(code, Some(1), "{stderr}");
                assert!(stderr.contains("not signed"), "{stderr}");
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Exit 3, and exit 1 naming a signature fault, are verdicts on the bytes; exit 1 for
    /// a file codesign could not read or an internal failure, any other exit, a signal and
    /// a failure to run at all are not.
    #[test]
    fn only_a_signature_fault_is_a_verdict() {
        let refused = |code: Option<i32>, stderr: &str| CodesignError::Refused {
            code,
            stderr: stderr.to_string(),
        };
        for stderr in [
            "bin/claude: code object is not signed at all\n",
            "bin/claude: invalid signature (code or signature have been modified)\n\
             In architecture: arm64\n",
            "x: main executable failed strict validation",
            "x: object file format unrecognized, invalid, or unsuitable\n",
        ] {
            assert!(refused(Some(1), stderr).is_verdict(), "{stderr:?}");
        }
        assert!(
            refused(
                Some(3),
                "test-requirement: code failed to satisfy specified code requirement(s)\n"
            )
            .is_verdict()
        );
        for stderr in [
            "bin/claude: Permission denied\n",
            "bin/claude: No such file or directory\n",
            "bin/claude: internal error in Code Signing subsystem\n",
            "bin/claude: the code cannot be read by the verifier (file system permissions etc.)",
            "",
        ] {
            assert!(!refused(Some(1), stderr).is_verdict(), "{stderr:?}");
        }
        assert!(!refused(Some(2), "code object is not signed at all").is_verdict());
        assert!(!refused(None, "code object is not signed at all").is_verdict());
        assert!(!CodesignError::TimedOut.is_verdict());
        assert!(!CodesignError::Unsupported.is_verdict());
        assert!(!CodesignError::InvalidTeam(String::new()).is_verdict());
    }

    /// Measured on the real tool: exit 1 for a file codesign cannot read is NOT a verdict
    /// (the same bytes verify once readable), while exit 1 for an unsigned file is.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_unreadable_file_is_not_a_verdict_and_an_unsigned_one_is() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("codesign-verdict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("thin");
        let mut thin = 0xCFFA_EDFEu32.to_be_bytes().to_vec();
        thin.extend_from_slice(&[0u8; 60]);
        std::fs::write(&file, &thin).unwrap();
        let unsigned = verify_developer_id(&file, "Q6L2SF6YDW", false).unwrap_err();
        assert!(unsigned.is_verdict(), "{unsigned}");

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads a mode-000 file, and then the premise does not hold.
        let premise = std::fs::read(&file).is_err();
        let unreadable = verify_developer_id(&file, "Q6L2SF6YDW", false).unwrap_err();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        if premise {
            assert!(
                matches!(unreadable, CodesignError::Refused { code: Some(1), .. }),
                "{unreadable:?}"
            );
            assert!(!unreadable.is_verdict(), "{unreadable}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A deadline already spent kills and reaps the child rather than waiting on it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_spent_deadline_is_a_timeout_not_a_pass() {
        assert!(matches!(
            verify_developer_id_until(Path::new("/bin/ls"), "Q6L2SF6YDW", false, Instant::now()),
            Err(CodesignError::TimedOut)
        ));
    }

    /// Apple's own binaries are signed by Apple, not by a Developer ID: the requirement
    /// refuses them (exit 3, or 1 where the platform binary has no leaf OU), which is
    /// the proof the team pin is doing work rather than accepting any valid signature.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_validly_signed_binary_of_another_signer_is_refused() {
        match verify_developer_id(Path::new("/bin/ls"), "Q6L2SF6YDW", false) {
            Err(CodesignError::Refused { code, .. }) => {
                assert!(matches!(code, Some(1 | 3)), "{code:?}");
            }
            other => panic!("{other:?}"),
        }
    }
}
