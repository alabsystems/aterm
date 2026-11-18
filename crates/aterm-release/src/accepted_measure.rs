// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit owner acceptance of a recorded release-check FAIL, separate from a passing check.

use std::path::Path;

use aterm_verify::checks::Check;

use crate::ledger::GitCli;

/// Record an explicit instruction to ship despite the FAIL of `check` in `report`: the journaled
/// release's record of `check` (the bundle's for measure, the tree's for a check that reads no
/// bundle), a FAIL (for measure also a NOT RUN) under the current build-inputs seal read from
/// `report` (its SHA-256 unchanged), is rewritten as accepted ([`crate::check_record::accept`]). Only a check that
/// runs the product's tests or measurements is accepted
/// ([`crate::check_record::acceptable`]). The other checks are judged when the records are
/// read: the release proceeds only once each of them has its own ok record or its own
/// acceptance ([`crate::check_record::checks`]).
///
/// # Errors
/// Refuses a missing reason or journal, a check whose FAIL is never accepted, for measure a
/// missing artifact or a changed executable, a report that is not the FAIL's log as it was, and
/// any failure reading or writing the record.
pub fn record(repo: &Path, check: Check, report: &Path, reason: &str) -> Result<(), String> {
    if reason.trim().is_empty() {
        return Err("owner acceptance requires its reason".into());
    }
    let git = GitCli::new(repo);
    let journal = crate::release::Journal::load(
        &crate::release::release_dir(&git)?.join(crate::release::JOURNAL_FILE),
    )?
    .ok_or("no release journal")?;
    let executable = if check == Check::Measure {
        let artifact = journal
            .artifact
            .as_ref()
            .ok_or("no built release artifact")?;
        if crate::artifact::macho_sha256(&artifact.app)? != artifact.macho_sha256 {
            return Err("the built executable changed since measurement".into());
        }
        Some(artifact.macho_sha256.clone())
    } else {
        None
    };
    let report = std::fs::canonicalize(report).map_err(|e| e.to_string())?;
    let report_sha256 = crate::artifact::file_sha256(&report)?;
    let at = crate::check_record::accept(
        &git,
        &journal.commit,
        (check, executable.as_deref()),
        &crate::build_inputs::for_commit(&git, &journal.commit)?,
        (&report, &report_sha256),
        reason.trim(),
    )?;
    println!(
        "OWNER ACCEPTED {} failure for {}: {}; report {} (not a passing verification; \
         record {})",
        check.name(),
        journal.commit,
        reason.trim(),
        report.display(),
        at.display()
    );
    Ok(())
}
