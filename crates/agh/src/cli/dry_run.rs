// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `--dry-run` (plan §10, body 9): nothing is written, journaled or posted. The routing, the
//! permit and the reads run as the real verb's do, and each report says what the real run
//! would print: `would_send` where it would send, or the outcome its guard read already
//! gives. Reading a journal may cut a torn tail or raise a stale intent's need, as every
//! reader's scan does; a journal that is not there is not made.

use std::collections::BTreeSet;

use super::Answer;
use super::cmd_release_write::{
    Ended, Write, answered, as_outcome, failed_all, items, push_step, refused_as, uploaded_before,
};
use super::write_out::{ReportOutcome, WriteReport, done, failed};
use crate::error::{Error, Kind, Result};
use crate::github::releases::{
    ASSETS_MAX, AssetSet, CreateStep, DraftRequest, EnsureSpec, PublishGuard, PublishPatch,
    ReleaseEdit, Replace, UploadFile, asset_set, clobber_temporary,
};
use crate::journal::{Intent, IntentDraft, IntentId, IntentStore, IntentView, Settled};
use crate::model::ReleaseObject;
use crate::write::WriteOp;

/// What one step of a real run would do.
enum Verdict {
    /// It would send.
    Send,
    /// Its guard read finds the state already right: the slug the real run prints, with the
    /// object's id and URL.
    Same(ReportOutcome, Option<u64>, Option<String>),
    /// It would be refused, with this error.
    Refuse(Error),
}

impl Verdict {
    /// `report` as this verdict prints it, or the error that ends the run.
    fn report(self, report: &WriteReport) -> std::result::Result<WriteReport, Ended> {
        match self {
            Verdict::Send => Ok(as_outcome(report.clone(), ReportOutcome::WouldSend)),
            Verdict::Same(outcome, id, url) => {
                Ok(as_outcome(report.clone(), outcome).with_object(id, url.as_deref()))
            }
            Verdict::Refuse(error) => Err(Box::new((refused_as(report.clone(), &error), error))),
        }
    }
}

/// The journal a dry run reads: this repository's when its file is there (or the tape's,
/// under a tape), and an empty store otherwise, so a dry run makes no journal. Only this
/// repository's own path is looked at (`<journal>/<owner>/<repo>.steps`, lowercased, as
/// `Journal::open` names it), so another repository's journal never decides this dry run.
///
/// # Errors
///
/// could_not_run, `Sub::Local`, for a path whose entry cannot be looked at (the real run's
/// journal would meet it too); `Github::journal`'s errors.
fn readable_journal(w: &Write<'_>) -> Result<Box<dyn IntentStore>> {
    let taped = w.gh.fx().mode().is_taped();
    let there = match w.gh.dirs() {
        Some(dirs) if !taped => {
            let path = crate::journal::path_of(dirs, &w.slug);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => {
                    return Err(Error::local(format!(
                        "the write journal {} cannot be looked at: {e}",
                        path.display()
                    )));
                }
            }
        }
        Some(_) => true,
        None => taped,
    };
    if there {
        return Ok(Box::new(w.gh.journal(&w.slug)?));
    }
    Ok(Box::new(NoJournal))
}

/// The journal of a repository that has none: no intent, and nothing a dry run would write.
struct NoJournal;

impl NoJournal {
    /// What a write to it answers: a dry run writes nothing, so no caller asks.
    fn writes_nothing() -> Error {
        Error::local("a dry run writes nothing to the journal")
    }
}

impl IntentStore for NoJournal {
    fn begin(&self, _draft: &IntentDraft) -> Result<IntentId> {
        Err(NoJournal::writes_nothing())
    }

    fn settle(&self, _id: &IntentId, _outcome: &Settled) -> Result<()> {
        Err(NoJournal::writes_nothing())
    }

    fn observe(&self, _id: &IntentId, _found: Option<u64>) -> Result<()> {
        Err(NoJournal::writes_nothing())
    }

    fn open(&self) -> Result<Vec<Intent>> {
        Ok(Vec::new())
    }

    fn latest(&self, _op: WriteOp, _key: &str) -> Result<Option<IntentView>> {
        Ok(None)
    }
}

/// The create's and the edit's own local refusal of notes over 125,000 characters (Invalid,
/// exit 10), `releases::notes_bound`, naming this repository as theirs does.
fn notes_bound(w: &Write<'_>, notes: &str) -> Result<()> {
    crate::github::releases::notes_bound(notes).map_err(|e| e.with_repo(w.slug.to_string()))
}

/// The create's verdict: `Begin` → `Send`; `Converge` → `converged` with the release;
/// `StillOpen` → the open-intent rule's error; `Refuse` → its error.
fn create_verdict(w: &Write<'_>, req: &DraftRequest) -> Result<(Verdict, Option<ReleaseObject>)> {
    notes_bound(w, &req.body)?;
    let journal = readable_journal(w)?;
    Ok(match w.releases().create_step(req, &*journal)? {
        CreateStep::Begin { .. } => (Verdict::Send, None),
        CreateStep::Converge { release, .. } => (
            Verdict::Same(
                ReportOutcome::Converged,
                Some(release.id),
                Some(release.html_url.clone()),
            ),
            Some(*release),
        ),
        // As the real run answers it: `create`'s `Outcome::Unknown`, through `one_shot`.
        CreateStep::StillOpen { intent } => (
            Verdict::Refuse(crate::write::unknown_error(
                &intent,
                w.releases().still_open(&intent, &req.tag_name),
            )),
            None,
        ),
        CreateStep::Refuse(error) => (Verdict::Refuse(error), None),
    })
}

/// `release create --dry-run`: `Releases::create_step`'s verdict.
///
/// # Errors
///
/// The reads' errors, and Invalid for notes over 125,000 characters.
pub(super) fn release_create(
    w: &Write<'_>,
    req: &DraftRequest,
    report: &WriteReport,
) -> Result<Answer> {
    let (verdict, _) = create_verdict(w, req)?;
    Ok(answered(verdict.report(report)))
}

/// One upload's verdict over the assets the release holds (plan §10, body 9's table): no
/// asset of the name → `Send`; the same digest → `converged` (`Never`) or `unchanged`
/// (`Clobber`); another digest → Conflict `already_exists` (`Never`) or `Send` (`Clobber`); an
/// asset of the name still `starter` or `open` → Conflict `guard_failed` (`Never`, attempt 1's
/// guard, with its remedy), or `Send` (`Clobber`, which replaces it); an immutable release →
/// Refused `immutable`. The library's local refusals come in its order: under `Clobber` a
/// temporary name `{name}.agh-{8 hex}` over 255 bytes is Invalid unless the bytes are there, and
/// a `Send` onto a release already holding 1000 assets, none of the name it uploads as, is
/// Invalid, as attempt 1's guard answers. The assets are those the release read carries and
/// `added`, the names this run's earlier files would add (the real run lists the assets again
/// before each file, so it sees them).
fn upload_verdict(
    w: &Write<'_>,
    release: &ReleaseObject,
    added: &BTreeSet<String>,
    file: &UploadFile,
    replace: Replace,
) -> Result<Verdict> {
    if release.immutable {
        return Ok(Verdict::Refuse(w.releases().immutable(release.id)));
    }
    let temporary = clobber_temporary(&file.name, &file.sha256);
    let verdict = held_verdict(w, release, added, file, replace, &temporary)?;
    // The real clobber answers `unchanged` for the same bytes first, and judges its temporary
    // name only before it would write.
    if replace == Replace::Clobber
        && !matches!(verdict, Verdict::Same(..))
        && let Some(error) = w.releases().temporary_fault(&temporary)
    {
        return Ok(Verdict::Refuse(error));
    }
    Ok(verdict)
}

/// `upload_verdict` over a release that is not immutable, before the clobber's temporary name
/// (`temporary`) is judged.
fn held_verdict(
    w: &Write<'_>,
    release: &ReleaseObject,
    added: &BTreeSet<String>,
    file: &UploadFile,
    replace: Replace,
    temporary: &str,
) -> Result<Verdict> {
    let clobber = replace == Replace::Clobber;
    let holds = |name: &str| release.assets.iter().any(|a| a.name == name) || added.contains(name);
    // A clobber that finds its temporary asset already there renames or clears it rather
    // than adding one, so only a release without it can be full.
    let full = |sent_as: &str| {
        let adds = !holds(sent_as) && !(clobber && holds(temporary));
        (adds && release.assets.len() + added.len() >= ASSETS_MAX)
            .then(|| Verdict::Refuse(w.releases().full(release.id)))
    };
    let Some(asset) = release.assets.iter().find(|a| a.name == file.name) else {
        return Ok(full(&file.name).unwrap_or(Verdict::Send));
    };
    if !asset.is_uploaded() {
        if clobber {
            return Ok(full(temporary).unwrap_or(Verdict::Send));
        }
        return Ok(Verdict::Refuse(w.releases().upload_in_progress(
            &file.name,
            release,
            &asset.state,
        )));
    }
    let theirs = asset.sha256()?;
    if theirs == Some(file.sha256) {
        let outcome = if clobber {
            ReportOutcome::Unchanged
        } else {
            ReportOutcome::Converged
        };
        return Ok(Verdict::Same(
            outcome,
            Some(asset.id),
            Some(asset.browser_download_url.clone()),
        ));
    }
    if clobber {
        return Ok(full(temporary).unwrap_or(Verdict::Send));
    }
    Ok(Verdict::Refuse(w.releases().held_otherwise(
        &file.name,
        release.id,
        theirs,
        file.sha256,
    )))
}

/// `upload_verdict`, its error (an asset whose digest does not parse, as the real guard's
/// read refuses it) the file's refusal, so the reports before it still print.
///
/// A `Send` that adds an asset (its name neither on the release nor added by an earlier file,
/// and, under `Clobber`, no temporary asset of it left to rename) adds its name to `added`,
/// so a later file of the same run is judged against the release as the real run would find
/// it.
fn upload_verdict_or_refuse(
    w: &Write<'_>,
    release: &ReleaseObject,
    added: &mut BTreeSet<String>,
    file: &UploadFile,
    replace: Replace,
) -> Verdict {
    let verdict = upload_verdict(w, release, added, file, replace).unwrap_or_else(Verdict::Refuse);
    if matches!(verdict, Verdict::Send) {
        let holds =
            |name: &str| release.assets.iter().any(|a| a.name == name) || added.contains(name);
        let temporary = clobber_temporary(&file.name, &file.sha256);
        if !holds(&file.name) && !(replace == Replace::Clobber && holds(&temporary)) {
            added.insert(file.name.clone());
        }
    }
    verdict
}

/// `release upload --dry-run`: one report per file, ending at the first that would be
/// refused, as the real run ends.
pub(super) fn release_upload(
    w: &Write<'_>,
    release: &ReleaseObject,
    files: &[UploadFile],
    replace: Replace,
) -> Answer {
    let mut reports = Vec::with_capacity(files.len());
    let mut added = BTreeSet::new();
    for file in files {
        let report = w.report(WriteOp::AssetUpload, &file.name);
        let verdict = upload_verdict_or_refuse(w, release, &mut added, file, replace);
        if let Some(answer) = push_step(&mut reports, verdict.report(&report)) {
            return answer;
        }
    }
    items(&reports)
}

/// The publish guard's verdict on `release`, whose assets would be `seen` when the PATCH is
/// sent, against `want`: the library's own judgement (`Releases::publish_judgement`), so
/// published as this publish would leave it → `converged`; a draft holding exactly `want`,
/// every asset uploaded → `Send`; otherwise its Conflict `guard_failed`.
fn publish_verdict(
    w: &Write<'_>,
    release: &ReleaseObject,
    seen: &AssetSet,
    want: &AssetSet,
    patch: &PublishPatch,
) -> Verdict {
    match w
        .releases()
        .publish_judgement(release.clone(), seen, want, patch)
    {
        Ok(Some(release)) => Verdict::Same(
            ReportOutcome::Converged,
            Some(release.id),
            Some(release.html_url),
        ),
        Ok(None) => Verdict::Send,
        Err(error) => Verdict::Refuse(error),
    }
}

/// `release publish --dry-run`: the publish guard's verdict on the release as read.
pub(super) fn release_publish(
    w: &Write<'_>,
    release: &ReleaseObject,
    guard: &PublishGuard,
    patch: &PublishPatch,
    report: &WriteReport,
) -> Result<Answer> {
    let seen = asset_set(release)?;
    let want = guard.assets.as_ref().map_or_else(
        || seen.clone(),
        |assets| assets.iter().map(|(n, d)| (n.clone(), Some(*d))).collect(),
    );
    let verdict = publish_verdict(w, release, &seen, &want, patch);
    Ok(answered(verdict.report(report)))
}

/// `release edit --dry-run`: notes over 125,000 characters → Invalid, as the real edit
/// refuses them before it sends; every given field already as asked → `unchanged`; else
/// `would_send`.
///
/// # Errors
///
/// Invalid for notes over 125,000 characters.
pub(super) fn release_edit(
    w: &Write<'_>,
    release: &ReleaseObject,
    edit: &ReleaseEdit,
    report: &WriteReport,
) -> Result<Answer> {
    if let Some(notes) = &edit.body {
        notes_bound(w, notes)?;
    }
    let report = if edit.holds(release) {
        as_outcome(report.clone(), ReportOutcome::Unchanged)
            .with_object(Some(release.id), Some(&release.html_url))
    } else {
        as_outcome(report.clone(), ReportOutcome::WouldSend)
    };
    Ok(done(report))
}

/// `release ensure --dry-run`: the create's verdict, then each asset's over the release it
/// would converge on (every asset `would_send` when the create would send), then the
/// publish's over the assets the release would hold by then.
///
/// # Errors
///
/// The create's reads' errors, and Invalid for notes over 125,000 characters, which
/// `Write::finish_items` prints as an array of the release's report alone, as the real run's.
pub(super) fn release_ensure(
    w: &Write<'_>,
    spec: &EnsureSpec,
    first: &WriteReport,
) -> Result<Answer> {
    let (verdict, release) = create_verdict(w, &spec.draft)?;
    let mut reports = Vec::with_capacity(spec.assets.len() + 2);
    match verdict.report(first) {
        Ok(report) => reports.push(report),
        Err(ended) => return Ok(failed_all(&[ended.0], ended.1)),
    }
    let mut seen = release.as_ref().map(asset_set).transpose()?;
    let mut added = BTreeSet::new();
    for file in &spec.assets {
        let report = w.report(WriteOp::AssetUpload, &file.name);
        let verdict = match &release {
            None => Verdict::Send,
            Some(release) if release.draft => {
                upload_verdict_or_refuse(w, release, &mut added, file, Replace::Never)
            }
            Some(release) => match uploaded_before(w, release, file) {
                Ok(ensured) => Verdict::Same(
                    ensured.effect.into(),
                    Some(ensured.object.id),
                    Some(ensured.object.browser_download_url),
                ),
                Err(error) => Verdict::Refuse(error),
            },
        };
        if matches!(verdict, Verdict::Send)
            && let Some(seen) = &mut seen
        {
            seen.insert(file.name.clone(), Some(file.sha256));
        }
        if let Some(answer) = push_step(&mut reports, verdict.report(&report)) {
            return Ok(answer);
        }
    }
    if let Some(patch) = &spec.publish {
        let report = w.report(WriteOp::ReleasePublish, &spec.draft.tag_name);
        let want: AssetSet = spec
            .assets
            .iter()
            .map(|file| (file.name.clone(), Some(file.sha256)))
            .collect();
        let verdict = match (&release, &seen) {
            (Some(release), Some(seen)) => publish_verdict(w, release, seen, &want, patch),
            _ => Verdict::Send,
        };
        if let Some(answer) = push_step(&mut reports, verdict.report(&report)) {
            return Ok(answer);
        }
    }
    Ok(items(&reports))
}

/// `asset delete --dry-run`: an immutable release → Refused `immutable`; no asset of the name
/// → `unchanged`; else `would_send`.
pub(super) fn asset_delete(
    w: &Write<'_>,
    release: &ReleaseObject,
    name: &str,
    report: &WriteReport,
) -> Result<Answer> {
    if release.immutable {
        return Err(w.releases().immutable(release.id));
    }
    let outcome = if release.assets.iter().any(|a| a.name == name) {
        ReportOutcome::WouldSend
    } else {
        ReportOutcome::Unchanged
    };
    Ok(done(as_outcome(report.clone(), outcome)))
}

/// This repository's refs read as the writer, once `Write::writer_repo` has read the
/// repository itself: the ref's NotFound says the ref is absent only after that read found
/// the repository. `ref create`'s dry run reads through it; `ref delete`'s makes the real
/// run's own push check instead (`Refs::push_check`).
///
/// # Errors
///
/// `Write::writer_repo`'s (NotFound or NotVisible for a repository that is not there,
/// Forbidden `NoPush` without push).
fn writer_refs<'w>(w: &'w Write<'_>) -> Result<crate::github::Refs<'w>> {
    w.writer_repo()?;
    Ok(w.gh.refs(&w.slug).how(w.writer_how()))
}

/// `ref create --dry-run`: the repository, then `Refs::get`, as the writer. NotFound →
/// `would_send`; the same sha → `converged`; another sha → Conflict `already_exists`.
///
/// # Errors
///
/// The reads' errors.
pub(super) fn ref_create(
    w: &Write<'_>,
    reference: &str,
    sha: &str,
    report: &WriteReport,
) -> Result<Answer> {
    let refs = writer_refs(w)?;
    let there = match refs.get(reference) {
        Err(error) if error.kind == Kind::NotFound => {
            return Ok(done(as_outcome(report.clone(), ReportOutcome::WouldSend)));
        }
        found => found?,
    };
    if there.object.sha == sha {
        return Ok(done(
            as_outcome(report.clone(), ReportOutcome::Converged)
                .with_object(None, Some(&there.url)),
        ));
    }
    let error = refs.points_elsewhere(reference, &there.object.sha, sha);
    Ok(failed(refused_as(report.clone(), &error), error))
}

/// `ref delete --dry-run`: the real run's push check (`Refs::push_check`), then `Refs::get`,
/// as the writer. NotFound → `unchanged`; another sha → Conflict `guard_failed`; the expected
/// sha → `would_send`.
///
/// # Errors
///
/// The push check's (the repository read's error, Forbidden `NoPush` without push, Forbidden
/// `other` for an answer with no permissions block) and the ref read's.
pub(super) fn ref_delete(
    w: &Write<'_>,
    reference: &str,
    expect: &str,
    report: &WriteReport,
) -> Result<Answer> {
    let refs = w.gh.refs(&w.slug).how(w.writer_how());
    refs.push_check()?;
    let there = match refs.get(reference) {
        Err(error) if error.kind == Kind::NotFound => {
            return Ok(done(as_outcome(report.clone(), ReportOutcome::Unchanged)));
        }
        found => found?,
    };
    if there.object.sha == expect {
        return Ok(done(as_outcome(report.clone(), ReportOutcome::WouldSend)));
    }
    let error = refs.lease_refused(&there.object.sha, expect);
    Ok(failed(refused_as(report.clone(), &error), error))
}
