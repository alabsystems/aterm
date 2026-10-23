// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Developer ID signature verification via `/usr/bin/codesign --verify --strict`.

use std::path::Path;
use std::time::{Duration, Instant};

use aterm_uds::deadline::{RunError, output_until};

/// Absolute path to Apple's `codesign` tool.
pub const CODESIGN: &str = aterm_uds::os_path!("codesign");

/// Base verification timeout and floor for [`verify_timeout_for`].
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Waiting for `codesign` failed.
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

/// [`verify_developer_id_until`] within [`VERIFY_TIMEOUT`].
#[cfg(test)]
fn verify_developer_id(path: &Path, team: &str, deep: bool) -> Result<(), CodesignError> {
    verify_developer_id_until(path, team, deep, Instant::now() + VERIFY_TIMEOUT)
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
mod tests {
    use super::*;

    /// The requirement is Apple's Developer ID shape with the team spliced in verbatim.
    #[test]
    fn the_requirement_pins_anchor_chain_and_team() {
        assert_eq!(
            developer_id_requirement("Q6L2SF6YDW").unwrap(),
            "=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
             and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate \
             leaf[subject.OU] = \"Q6L2SF6YDW\""
        );
    }

    /// A team that could rewrite the requirement, or is empty, is refused before any spawn.
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

    /// Exit 3, and exit 1 naming a signature fault, are verdicts; exit 1 for an unreadable
    /// file or internal failure, any other exit, a signal and a failure to run are not.
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
            "aterm.app: a sealed resource is missing or invalid\n",
            "aterm.app: unsealed contents present in the bundle root\n",
            "A.app: invalid Info.plist (plist or signature have been modified)\n",
            "A.app: invalid resource directory (directory or signature have been modified)\n\
             In subcomponent: /A.app/Contents/Helpers/h\n",
            "aterm.app: bundle format unrecognized, invalid, or unsuitable\n",
            "A.app: invalid destination for symbolic link in bundle\n\
             file modified: /A.app/Contents/Resources/h: Permission denied\n",
        ] {
            assert!(refused(Some(1), stderr).is_verdict(), "{stderr:?}");
            let verified = Path::new(stderr.split(": ").next().unwrap_or_default());
            assert!(
                !refused(Some(1), stderr).judged_nothing_at(verified),
                "{stderr:?}"
            );
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

    /// A signal, and exit 1 whose first line names only a failure to read or run on the
    /// verified path, judged nothing.
    #[test]
    fn only_a_known_machine_fault_or_a_signal_judged_nothing() {
        let refused = |code: Option<i32>, stderr: &str| CodesignError::Refused {
            code,
            stderr: stderr.to_string(),
        };
        assert!(refused(None, "").judged_nothing_at(Path::new("bin/claude")));
        for stderr in [
            "bin/claude: Permission denied\n",
            "\n  bin/claude: Permission denied  \n",
            "bin/claude: internal error in Code Signing subsystem\n",
            "bin/claude: the code cannot be read by the verifier (file system permissions etc.)",
        ] {
            assert!(
                refused(Some(1), stderr).judged_nothing_at(Path::new("bin/claude")),
                "{stderr:?}"
            );
        }
        let progress = [
            (
                "/x/N1.app",
                "--prepared:/x/N1.app/Contents/Helpers/h\nX: Permission denied\n\
                 --validated:/x/N1.app/Contents/Helpers/h\nX: Permission denied\n\
                 /x/N1.app: nested code is modified or invalid\n"
                    .to_string(),
            ),
            (
                "/x/C.app",
                "--prepared:/x/C.app/Contents/Helpers/h: Permission denied\n\
                 --validated:/x/C.app/Contents/Helpers/h: Permission denied\n\
                 /x/C.app: nested code is modified or invalid\n"
                    .to_string(),
            ),
            (
                "/x/A.app",
                format!(
                    "/x/A.app: Permission denied\n{}/x/A.app: nested code is modified or \
                     invalid\n",
                    "--prepared:/x/A.app/Contents/Helpers/h\n".repeat(STDERR_KEEP / 32)
                ),
            ),
        ];
        for (verified, stderr) in &progress {
            assert!(
                !refused(Some(1), stderr).judged_nothing_at(Path::new(verified)),
                "{stderr:.300}"
            );
        }
        for (code, verified, stderr) in [
            (
                Some(1),
                "A.app",
                "A.app: nested code is modified or invalid\n",
            ),
            (Some(1), "A.app", ""),
            (Some(1), "/x/A.app", "/x/Other.app: Permission denied\n"),
            (Some(1), "A.app", "A.app: No such file or directory\n"),
            (
                Some(1),
                "A.app",
                "A.app: invalid destination for symbolic link in bundle\n\
                 file modified: /A.app/Contents/Resources/h: Permission denied\n",
            ),
            (
                Some(1),
                "A.app",
                "A.app: a verdict nobody has measured yet\n\
                 In subcomponent: /A.app/Contents/Helpers/h: Permission denied\n",
            ),
            (Some(1), "A.app", "A.app: unPermission denied\n"),
            (
                Some(1),
                "A.app",
                "A.app: Permission denied\nA.app: code object is not signed at all\n",
            ),
            (Some(2), "bin/claude", "bin/claude: Permission denied\n"),
            (
                Some(3),
                "A.app",
                "code failed to satisfy specified code requirement(s)\n",
            ),
        ] {
            assert!(
                !refused(code, stderr).judged_nothing_at(Path::new(verified)),
                "{code:?} {stderr:?}"
            );
        }
        assert!(!CodesignError::TimedOut.judged_nothing_at(Path::new("A.app")));
    }

    /// Exit 1 for a file codesign cannot read is not a verdict (the bytes verify once
    /// readable); exit 1 for an unsigned file is.
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
        let premise = std::fs::read(&file).is_err();
        let unreadable = verify_developer_id(&file, "Q6L2SF6YDW", false).unwrap_err();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        if premise {
            assert!(
                matches!(unreadable, CodesignError::Refused { code: Some(1), .. }),
                "{unreadable:?}"
            );
            assert!(!unreadable.is_verdict(), "{unreadable}");
            assert!(unreadable.judged_nothing_at(&file), "{unreadable}");
        }
        assert!(!unsigned.judged_nothing_at(&file), "{unsigned}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A small file keeps 30 s, a 238 MB binary gets 143 s, nothing more than the cap.
    #[test]
    fn the_verify_deadline_scales_with_the_file_and_is_capped() {
        assert_eq!(verify_timeout_for(0), VERIFY_TIMEOUT);
        assert_eq!(verify_timeout_for(64 * 1024), VERIFY_TIMEOUT);
        assert_eq!(
            verify_timeout_for(238_223_808),
            Duration::from_secs(30 + 113)
        );
        assert!(verify_timeout_for(238_223_808) > VERIFY_TIMEOUT);
        assert_eq!(verify_timeout_for(u64::MAX), VERIFY_TIMEOUT_MAX);
        assert_eq!(
            verify_timeout_for(10 * 1024 * 1024 * 1024),
            VERIFY_TIMEOUT_MAX
        );
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

    /// Apple's own binaries carry no Developer ID: the requirement refuses them (exit 3, or 1
    /// where a platform binary has no leaf OU), so the team pin does work.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_validly_signed_binary_of_another_signer_is_refused() {
        aterm_uds::deadline::require_trust_service();
        match verify_developer_id(Path::new("/bin/ls"), "Q6L2SF6YDW", false) {
            Err(CodesignError::Refused { code, .. }) => {
                assert!(matches!(code, Some(1 | 3)), "{code:?}");
            }
            other => panic!("{other:?}"),
        }
    }
}
