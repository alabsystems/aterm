// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Developer ID gate: `/usr/bin/codesign --verify --strict` under a designated
//! requirement that pins Apple's anchor, the Developer ID chain and one Team ID.
//!
//! ONE requirement text for every caller — the app updater's bundle check and atpkg's
//! staged agent binaries — so the two cannot disagree about what "signed by team X"
//! means. The spawn is bounded by a deadline (`aterm_uds::deadline::output_until`, the
//! workspace's one bounded child runner) and fails closed: a timeout, a spawn failure or
//! a non-zero exit is never a pass.

use std::path::Path;
use std::time::{Duration, Instant};

use aterm_uds::deadline::{RunError, output_until};

/// Absolute, so no `PATH` entry can stand in for Apple's tool.
pub const CODESIGN: &str = "/usr/bin/codesign";

/// The ceiling on one verification when the caller brings no deadline of its own — and
/// the floor [`verify_timeout_for`] scales up from.
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// The slowest read rate a size-scaled verification ([`verify_timeout_for`]) is given time
/// for: 2 MiB/s. codesign `--verify --strict` hashes every page of the file, so its time
/// is its READ time, and under atpkg's `taskpolicy -c utility` clamp (the window spawns
/// every pass as `qos::Role::Background`, and codesign inherits the clamp) those reads are
/// IO-throttled behind whatever else the disk is doing. Measured at a load average of
/// ~90: a 238 MB binary verified in 1.98 s unclamped and in 453 s clamped (throttled
/// reads, 2.3 s of CPU in all), and a fixed 30 s ceiling failed it, then passed it 21 s
/// later — a race against the disk, not a verdict.
/// So no deadline is long enough on a saturated machine: this rate covers a moderately
/// busy one, and what a deadline still cannot cover is DEFERRED by the caller, never
/// failed (atpkg's `StageError::VerifyDeferred`).
pub const VERIFY_MIN_BYTES_PER_SEC: u64 = 2 * 1024 * 1024;

/// The most time [`verify_timeout_for`] gives one file, however large: a verification
/// holds the store lock, and one that has not finished in ten minutes is deferred to the
/// next pass rather than waited on.
pub const VERIFY_TIMEOUT_MAX: Duration = Duration::from_secs(600);

/// The deadline one verification of a `len`-byte file is given: [`VERIFY_TIMEOUT`] plus
/// one second per [`VERIFY_MIN_BYTES_PER_SEC`], capped at [`VERIFY_TIMEOUT_MAX`]. A small
/// file keeps the historical 30 s; codex's 238 MB binary gets 143 s.
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
    /// The team id is empty or not ASCII alphanumeric; refused before it can reach the
    /// requirement text (a `"` there would rewrite the requirement).
    InvalidTeam(String),
    /// Not macOS: there is no codesign here, so no platform anchor to check.
    Unsupported,
    /// codesign could not be started.
    Spawn(std::io::Error),
    /// Waiting for codesign failed.
    Wait(std::io::Error),
    /// codesign did not finish by the deadline; it was killed and reaped. Never a pass,
    /// and never a verdict on the bytes ([`Self::is_verdict`]): what a caller does with it
    /// is its own policy — the app updater refuses the update, atpkg's Developer ID gate
    /// defers the program to its next pass (`StageError::VerifyDeferred`).
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

/// codesign's words (Security.framework's `errSecCS…` strings) for a signature fault in
/// the bytes it read. Exit 1 also reports a file it could not read or an internal
/// failure, and those are facts about this machine, not verdicts on the bytes. The
/// bundle-level two (a changed, added or removed sealed resource; a stray file in the
/// bundle root) were measured on an ad hoc signed `.app` under `--deep --strict`: the
/// app updater's stage is a bundle, and its tampering must stay a verdict where a
/// non-verdict is a passing refusal. `bundle format
/// unrecognized, invalid, or unsuitable` is the bundle's form of the object-file line
/// (`errSecCSBadBundleFormat`, measured: an `.app` with no `Info.plist`, or
/// an empty one): a bundle that is not a bundle stays that way until something
/// replaces it.
const SIGNATURE_FAULTS: [&str; 11] = [
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
];

/// codesign's words for a failure to READ or to run, on a line of an exit-1 refusal:
/// facts about this machine at this moment, never about the bytes
/// ([`CodesignError::judged_nothing`]).
const MACHINE_FAULTS: [&str; 8] = [
    "Permission denied",
    "No such file or directory",
    "internal error in Code Signing subsystem",
    "the code cannot be read by the verifier (file system permissions etc.)",
    "Resource temporarily unavailable",
    "Operation not permitted",
    "Too many open files",
    "Too many open files in system",
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

    /// Whether codesign RAN and KNOWABLY judged nothing: a signal ended it, or exit 1
    /// names only a failure to read or to run ([`MACHINE_FAULTS`]) and no signature
    /// fault. Narrower than `!is_verdict()` on purpose: a refusal in words not listed
    /// either way (codesign has many) stays a refusal of the bytes, so a caller that
    /// discards or asks a person on a refusal keeps doing so for any tampering it has
    /// not been taught (a modified Info.plist and a modified resource directory were
    /// measured in words neither list then held).
    #[must_use]
    pub fn judged_nothing(&self) -> bool {
        match self {
            Self::Refused { code: None, .. } => true,
            Self::Refused {
                code: Some(1),
                stderr,
            } => {
                let named = |faults: &[&str]| {
                    stderr.lines().any(|line| {
                        let line = line.trim_end();
                        faults.iter().any(|fault| line.ends_with(fault))
                    })
                };
                !self.is_verdict() && named(&MACHINE_FAULTS)
            }
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

/// [`verify_developer_id_until`] within [`VERIFY_TIMEOUT`] — the tests' spelling.
#[cfg(test)]
fn verify_developer_id(path: &Path, team: &str, deep: bool) -> Result<(), CodesignError> {
    verify_developer_id_until(path, team, deep, Instant::now() + VERIFY_TIMEOUT)
}

/// Verify that `path` is signed with a Developer ID of `team`, finished by `deadline`.
/// `deep` also verifies nested code (a bundle); a single Mach-O needs none.
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
    let mut cmd = aterm_uds::spawnfd::command(CODESIGN);
    cmd.arg("--verify");
    if deep {
        cmd.arg("--deep");
    }
    cmd.args(["--strict", "--verbose=2", "-R", &requirement])
        .arg(path);
    let out = output_until(&mut cmd, deadline).map_err(|e| match e {
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
            "aterm.app: a sealed resource is missing or invalid\n",
            "aterm.app: unsealed contents present in the bundle root\n",
            "A.app: invalid Info.plist (plist or signature have been modified)\n",
            "A.app: invalid resource directory (directory or signature have been modified)\n\
             In subcomponent: /A.app/Contents/Helpers/h\n",
            "aterm.app: bundle format unrecognized, invalid, or unsuitable\n",
        ] {
            assert!(refused(Some(1), stderr).is_verdict(), "{stderr:?}");
            assert!(!refused(Some(1), stderr).judged_nothing(), "{stderr:?}");
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

    /// What a caller that DISCARDS or ASKS A PERSON on a refusal may set aside
    /// ([`CodesignError::judged_nothing`]): a signal, and exit 1 naming only a failure to
    /// read or run. NEGATIVE CONTROLS: a refusal in words neither list holds — the next
    /// tampering codesign words differently — judged something (fail closed), as does a
    /// line naming a machine fault beside a signature fault, and exit 2 or 3.
    #[test]
    fn only_a_known_machine_fault_or_a_signal_judged_nothing() {
        let refused = |code: Option<i32>, stderr: &str| CodesignError::Refused {
            code,
            stderr: stderr.to_string(),
        };
        assert!(refused(None, "").judged_nothing());
        for stderr in [
            "bin/claude: Permission denied\n",
            "bin/claude: No such file or directory\n",
            "bin/claude: internal error in Code Signing subsystem\n",
            "bin/claude: the code cannot be read by the verifier (file system permissions etc.)",
        ] {
            assert!(refused(Some(1), stderr).judged_nothing(), "{stderr:?}");
        }
        for (code, stderr) in [
            (Some(1), "A.app: nested code is modified or invalid\n"),
            (Some(1), ""),
            (
                Some(1),
                "A.app: Permission denied\nA.app: code object is not signed at all\n",
            ),
            (Some(2), "bin/claude: Permission denied\n"),
            (
                Some(3),
                "code failed to satisfy specified code requirement(s)\n",
            ),
        ] {
            assert!(
                !refused(code, stderr).judged_nothing(),
                "{code:?} {stderr:?}"
            );
        }
        assert!(!CodesignError::TimedOut.judged_nothing());
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
            assert!(unreadable.judged_nothing(), "{unreadable}");
        }
        assert!(!unsigned.judged_nothing(), "{unsigned}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE DEADLINE SCALES WITH THE FILE: a small file keeps 30 s, a 238 MB binary gets
    /// 143 s, and nothing is given more than the ten-minute cap.
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

    /// Apple's own binaries are signed by Apple, not by a Developer ID: the requirement
    /// refuses them (exit 3, or 1 where the platform binary has no leaf OU), which is
    /// the proof the team pin is doing work rather than accepting any valid signature.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_validly_signed_binary_of_another_signer_is_refused() {
        if aterm_uds::deadline::trust_service_silent() {
            return;
        }
        match verify_developer_id(Path::new("/bin/ls"), "Q6L2SF6YDW", false) {
            Err(CodesignError::Refused { code, .. }) => {
                assert!(matches!(code, Some(1 | 3)), "{code:?}");
            }
            other => panic!("{other:?}"),
        }
    }
}
