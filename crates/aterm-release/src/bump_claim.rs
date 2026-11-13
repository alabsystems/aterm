// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Custody of the cutter's one unpublished bump. No other local history is disposable.

use std::path::Path;

use crate::ledger::{GitRunner, git_ok};
use crate::release::{RELEASE_COMMAND, Refusal};

pub(crate) const MAX_ATTEMPTS: usize = 3;

/// The exact retry path used by the cutter and the local-bare fixture. Only a
/// definite lost compare-and-swap retries; uncertain/native refusals retain custody.
pub(crate) fn push(
    git: &dyn GitRunner,
    journal: &Path,
    mut tip: String,
    mut edit: impl FnMut(&str) -> Result<String, Refusal>,
    mut write: impl FnMut(&str, &str) -> Result<crate::ledger::RefWrite, String>,
    mut refresh: impl FnMut() -> Result<String, String>,
) -> Result<String, Refusal> {
    let mut composed = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let bumped = match composed.take() {
            Some(bumped) => bumped,
            None => edit(&tip)?,
        };
        match write(&tip, &bumped) {
            Ok(crate::ledger::RefWrite::Written) => return Ok(bumped),
            Ok(crate::ledger::RefWrite::Conflict) => {}
            Err(why) | Ok(crate::ledger::RefWrite::Transient(why)) => return Err(Refusal::new(
                "the native bump write did not establish a successful claim",
                "inspect the native refusal before rerunning; the journaled local bump is retained",
            )
            .detail([why])),
        }
        tip = refresh().map_err(|why| {
            Refusal::new(
                "cannot refresh main after the bump lost its compare-and-swap",
                "retry once the native origin answers; the journaled local bump is retained",
            )
            .detail([why])
        })?;
        composed = recover(git, journal, &tip, attempt < MAX_ATTEMPTS).map_err(|why| {
            Refusal::new(
                "cannot recover the owned bump after a race",
                "inspect the custody journal and clone",
            )
            .detail([why])
        })?;
        if composed.is_none() && text(git, &["rev-parse", "HEAD"]).as_deref() != Ok(tip.as_str()) {
            return Err(Refusal::new(
                "the local bump no longer matches its custody journal",
                "preserve the local history and inspect the divergence",
            ));
        }
        if attempt == MAX_ATTEMPTS {
            return Err(Refusal::new(
                "main moved under the bump three times (each push lost the race); the owned bump is undone",
                format!("{RELEASE_COMMAND} --bump again"),
            ));
        }
    }
    unreachable!("the nonzero attempt bound always returns a verdict")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Owned {
    commit: String,
    parent: String,
    subject: String,
    paths: Vec<String>,
}

fn text(git: &dyn GitRunner, args: &[&str]) -> Result<String, String> {
    git_ok(git, args)
        .map(|out| out.stdout_utf8().trim().to_owned())
        .map_err(|e| e.to_string())
}

fn paths(git: &dyn GitRunner, before: &str, after: &str) -> Result<Vec<String>, String> {
    let out =
        git_ok(git, &["diff", "--name-only", "-z", before, after]).map_err(|e| e.to_string())?;
    Ok(out
        .stdout_utf8()
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect())
}

fn touched(git: &dyn GitRunner, before: &str, after: &str) -> Result<Vec<String>, String> {
    aterm_verify::git_paths::range(
        &mut |args| {
            crate::ledger::git_read_ok(git, args)
                .map(|out| out.stdout)
                .map_err(|e| e.to_string())
        },
        before,
        after,
        aterm_verify::git_paths::MergeDiff::AllParents,
        &[],
    )?
    .into_iter()
    .map(|path| String::from_utf8(path).map_err(|e| e.to_string()))
    .collect()
}

pub(crate) fn record(
    git: &dyn GitRunner,
    journal: &Path,
    subject: &str,
    mut declared: Vec<String>,
) -> Result<(), String> {
    let commit = text(git, &["rev-parse", "HEAD"])?;
    let parent = text(git, &["rev-parse", "HEAD^"])?;
    let actual = paths(git, &parent, &commit)?;
    declared.sort();
    declared.dedup();
    // The declared set is every path the bump may write; one of them (the changelog, whose
    // announcement is reconciled on every bump) can come out as it was. Custody is that the
    // commit changed something and nothing undeclared.
    if actual.is_empty()
        || actual
            .iter()
            .any(|path| declared.binary_search(path).is_err())
        || text(git, &["show", "-s", "--format=%s", "HEAD"])? != subject
    {
        return Err("the bump does not match its declared subject and changed set".into());
    }
    let record = Owned {
        commit,
        parent,
        subject: subject.into(),
        paths: actual,
    };
    let bytes = aterm_json::to_string(&record).map_err(|e| e.to_string())?;
    crate::durable::write_atomic_durable(journal, bytes.as_bytes(), 0o600)
        .map_err(|e| e.to_string())
}

/// Recover only an exact journaled bump whose parent is still in main. Returns
/// a composed bump for a disjoint advance; overlapping files require fresh edits.
pub(crate) fn recover(
    git: &dyn GitRunner,
    journal: &Path,
    tip: &str,
    compose: bool,
) -> Result<Option<String>, String> {
    let bytes = match crate::durable::read_text(journal) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let owned: Owned = aterm_json::from_str(&bytes).map_err(|e| e.to_string())?;
    let head = text(git, &["rev-parse", "HEAD"])?;
    if head != owned.commit || head == tip {
        return Ok(None);
    }
    let parents = text(git, &["show", "-s", "--format=%P", &head])?;
    if parents != owned.parent
        || text(git, &["show", "-s", "--format=%s", &head])? != owned.subject
        || paths(git, &owned.parent, &head)? != owned.paths
        || !git_ok(git, &["merge-base", "--is-ancestor", &owned.parent, tip]).is_ok()
        || git_ok(git, &["merge-base", "--is-ancestor", &head, tip]).is_ok()
    {
        return Ok(None);
    }
    if !text(git, &["status", "--porcelain"])?.is_empty() {
        return Err("cannot recover an owned bump in a dirty clone".into());
    }
    let disjoint = touched(git, &owned.parent, tip)?
        .iter()
        .all(|p| !owned.paths.contains(p));
    text(git, &["reset", "--keep", tip])?;
    if compose && disjoint {
        if let Err(why) = text(git, &["cherry-pick", &owned.commit]) {
            text(git, &["cherry-pick", "--abort"])?;
            return Err(format!("the disjoint bump did not compose cleanly: {why}"));
        }
        record(git, journal, &owned.subject, owned.paths)?;
        return text(git, &["rev-parse", "HEAD"]).map(Some);
    }
    Ok(None)
}

#[cfg(test)]
#[path = "bump_claim_tests.rs"]
mod tests;
