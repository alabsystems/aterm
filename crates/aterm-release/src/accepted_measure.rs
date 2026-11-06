// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit owner acceptance of a measured release defect, separate from a passing check.

use std::path::Path;

use crate::ledger::GitCli;

/// Record an explicit instruction to ship despite the measure FAIL in `report`: the journal's
/// bundle's measure record, a FAIL under the current build-inputs seal read from `report`
/// (its SHA-256 unchanged), is rewritten as accepted ([`crate::check_record::accept`]). The
/// other checks are judged when the records are read: the release proceeds only once each of
/// them has its own ok record ([`crate::check_record::checks`]).
///
/// # Errors
/// Refuses a missing reason, journal or artifact, a changed executable, a report that is not the
/// measure FAIL's log as it was, and any failure reading or writing the record.
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
    if crate::artifact::macho_sha256(&artifact.app)? != artifact.macho_sha256 {
        return Err("the built executable changed since measurement".into());
    }
    let report_sha256 = crate::artifact::file_sha256(&report)?;
    let at = crate::check_record::accept(
        &git,
        &journal.commit,
        &artifact.macho_sha256,
        &crate::build_inputs::current()?,
        (&report, &report_sha256),
        reason.trim(),
    )?;
    println!(
        "OWNER ACCEPTED measure failure for {}: {}; report {} (not a passing verification; \
         record {})",
        journal.commit,
        reason.trim(),
        report.display(),
        at.display()
    );
    Ok(())
}
