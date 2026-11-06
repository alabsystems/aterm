// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit owner acceptance of a measured release defect, separate from a passing check.

use std::path::{Path, PathBuf};

use crate::ledger::{GitCli, GitRunner};

#[derive(serde::Serialize, serde::Deserialize)]
struct Acceptance {
    commit: String,
    executable: String,
    report: PathBuf,
    report_sha256: String,
    reason: String,
    #[serde(default)]
    inputs: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Attempt {
    inputs: String,
    report_sha256: String,
    exit_code: i32,
    commit: String,
    executable: String,
}

pub(crate) fn record_attempt(
    report: &Path,
    inputs: &str,
    exit_code: i32,
    commit: &str,
    executable: &str,
) -> Result<(), String> {
    let attempt = Attempt {
        inputs: inputs.into(),
        report_sha256: crate::artifact::file_sha256(report)?,
        exit_code,
        commit: commit.into(),
        executable: executable.into(),
    };
    let text = aterm_toml::to_string(&attempt).map_err(|error| error.to_string())?;
    crate::durable::write_atomic_durable(
        &report.with_extension("inputs.toml"),
        text.as_bytes(),
        0o600,
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
fn report_inputs(report: &Path, commit: &str, executable: &str) -> Result<String, String> {
    let report_sha256 = crate::artifact::file_sha256(report)?;
    report_inputs_with_sha256(report, commit, executable, &report_sha256)
}

fn report_inputs_with_sha256(
    report: &Path,
    commit: &str,
    executable: &str,
    report_sha256: &str,
) -> Result<String, String> {
    let text = crate::durable::read_regular_text(&report.with_extension("inputs.toml"), 64 * 1024)
        .map_err(|error| error.to_string())?;
    let attempt: Attempt = aterm_toml::from_str(&text).map_err(|error| error.to_string())?;
    if attempt.exit_code != 1
        || attempt.commit != commit
        || attempt.executable != executable
        || attempt.report_sha256 != report_sha256
    {
        return Err("the failed verification attempt does not match its report".into());
    }
    Ok(attempt.inputs)
}

fn path(git: &dyn GitRunner, commit: &str) -> Result<PathBuf, String> {
    Ok(crate::release::release_dir(git)?
        .join("accepted-measure")
        .join(commit))
}

fn report_is_measure_only(text: &str) -> bool {
    let required: std::collections::BTreeSet<_> = aterm_verify::checks::RELEASE
        .iter()
        .filter(|check| **check != aterm_verify::checks::Check::Measure)
        .map(|check| check.name())
        .collect();
    let summary_prefix = format!("verify: FAIL — measure failed; {} ok in ", required.len());
    let mut passed = std::collections::BTreeSet::new();
    let mut failed = Vec::new();
    let mut has_summary = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("ok ") {
            if let Some(name) = rest.split_whitespace().next() {
                passed.insert(name);
            }
        } else if let Some(rest) = line.strip_prefix("FAIL ") {
            if let Some(name) = rest.split_whitespace().next() {
                failed.push(name);
            }
        } else if line.starts_with(&summary_prefix) {
            has_summary = true;
        }
    }
    passed == required && failed == ["measure"] && has_summary
}

/// Record an explicit instruction to ship despite the measurement failure in `report`.
/// The report must show that all other release checks passed for the journaled build.
///
/// # Errors
/// Refuses a missing reason, journal or artifact, a changed executable, a mismatched or
/// incomplete report, and any failure reading or writing the acceptance record.
pub fn record(repo: &Path, report: &Path, reason: &str) -> Result<(), String> {
    if reason.trim().is_empty() {
        return Err("owner acceptance requires its reason".into());
    }
    let git = GitCli::new(repo);
    let journal = crate::release::Journal::load(
        &crate::release::release_dir(&git)?.join(crate::release::JOURNAL_FILE),
    )?
    .ok_or("no release journal")?;
    let artifact = journal.artifact.ok_or("no built release artifact")?;
    let report = std::fs::canonicalize(report).map_err(|e| e.to_string())?;
    let text = crate::durable::read_text(&report).map_err(|e| e.to_string())?;
    if !report_is_measure_only(&text) {
        return Err(
            "acceptance requires a complete report: only measure failed, all other release checks passed"
                .into(),
        );
    }
    if crate::artifact::macho_sha256(&artifact.app)? != artifact.macho_sha256 {
        return Err("the built executable changed since measurement".into());
    }
    let report_sha256 =
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(text.as_bytes()));
    let inputs = crate::build_inputs::current()?;
    if report_inputs_with_sha256(
        &report,
        &journal.commit,
        &artifact.macho_sha256,
        &report_sha256,
    )? != inputs
    {
        return Err("the measurement report has no matching compiler/SDK content identity; rerun verification".into());
    }
    let acceptance = Acceptance {
        commit: journal.commit,
        executable: artifact.macho_sha256,
        report_sha256,
        report,
        reason: reason.trim().into(),
        inputs,
    };
    let dest = path(&git, &acceptance.commit)?;
    std::fs::create_dir_all(dest.parent().ok_or("acceptance has no parent")?)
        .map_err(|e| e.to_string())?;
    let text = aterm_toml::to_string(&acceptance).map_err(|e| e.to_string())?;
    crate::durable::write_atomic_durable(&dest, text.as_bytes(), 0o600)
        .map_err(|e| e.to_string())?;
    println!(
        "OWNER ACCEPTED measure failure for {}: {}; report {} (not a passing verification)",
        acceptance.commit,
        acceptance.reason,
        acceptance.report.display()
    );
    Ok(())
}

fn read(git: &dyn GitRunner, commit: &str) -> Result<Option<Acceptance>, String> {
    let text = match crate::durable::read_text(&path(git, commit)?) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let held: Acceptance = aterm_toml::from_str(&text).map_err(|e| e.to_string())?;
    if held.inputs.is_empty() {
        return Ok(None);
    }
    let report_text = crate::durable::read_text(&held.report).map_err(|e| e.to_string())?;
    let report_sha256 =
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(report_text.as_bytes()));
    if report_inputs_with_sha256(&held.report, &held.commit, &held.executable, &report_sha256)?
        != held.inputs
    {
        return Err("owner acceptance no longer matches its build inputs".into());
    }
    if held.commit != commit
        || held.reason.is_empty()
        || report_sha256 != held.report_sha256
        || !report_is_measure_only(&report_text)
    {
        return Err("owner acceptance no longer matches its complete measurement report".into());
    }
    Ok(Some(held))
}

/// A release may proceed under this explicit exception, but it never claims a verification pass.
///
/// # Errors
/// Refuses an unreadable, malformed or changed acceptance record or report.
pub fn accepted(
    git: &dyn GitRunner,
    commit: &str,
    executable: &str,
) -> Result<Option<String>, String> {
    let Some(acceptance) = read(git, commit)? else {
        return Ok(None);
    };
    if acceptance.inputs != crate::build_inputs::current()? {
        return Ok(None);
    }
    Ok(Some(acceptance).filter(|a| a.executable == executable).map(|a| {
        format!("OWNER ACCEPTED measure failure: {}; all other release checks passed; report {} (not a full verification pass)", a.reason, a.report.display())
    }))
}

#[cfg(test)]
mod tests {
    use super::report_is_measure_only;

    #[test]
    fn report_inputs_require_a_separate_matching_failed_attempt_receipt() {
        let root = aterm_tempfile::tempdir().unwrap();
        let report = root.path().join("verify.log");
        let digest = "a".repeat(64);
        let marker = format!("release-inputs-sha256: {digest}\n");
        std::fs::write(&report, &marker).unwrap();
        assert!(
            super::report_inputs(&report, "source", "image").is_err(),
            "test output cannot forge a receipt"
        );
        super::record_attempt(&report, &digest, 0, "source", "image").unwrap();
        assert!(
            super::report_inputs(&report, "source", "image").is_err(),
            "a pass is not a measured failure"
        );
        super::record_attempt(&report, &digest, 1, "source", "image").unwrap();
        assert_eq!(
            super::report_inputs(&report, "source", "image").unwrap(),
            digest
        );
        assert!(super::report_inputs(&report, "other source", "image").is_err());
        assert!(super::report_inputs(&report, "source", "other image").is_err());
        std::fs::write(&report, "changed report").unwrap();
        assert!(super::report_inputs(&report, "source", "image").is_err());
    }

    #[test]
    fn only_a_complete_measure_only_failure_can_be_accepted() {
        let report = "ok fmt\nok forge\nok libc\nok lint\nok census\nok doc\nok test\nok spec\nok appkit\nFAIL measure\nverify: FAIL — measure failed; 9 ok in 40m\n";
        assert!(report_is_measure_only(report));
        assert!(!report_is_measure_only(
            &report.replace("ok test", "FAIL test")
        ));
        assert!(!report_is_measure_only(&report.replace("ok appkit\n", "")));
        assert!(!report_is_measure_only(
            &report.replace("FAIL measure", "NOT RUN measure")
        ));
    }
}
