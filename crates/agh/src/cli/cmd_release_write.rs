// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The release write verbs (design §9.4): `release create`, `upload`, `publish`, `edit`,
//! `ensure` and `delete`, and the steps every write verb shares (plan §10, body 1): the local
//! checks first, the route and the permit, the journal, the report, and the release a write
//! names, always read as the writer so a write sees drafts.

use std::path::Path;

use aterm_json::Value;

use super::args::Invocation;
use super::cmd_release::prerelease_filter;
use super::dry_run;
use super::verbs::Noun;
use super::write_out::{ReportOutcome, WriteReport, done, failed, outcome_of};
use super::{Answer, Completeness, Ctx, Outcome, Receipt};
use crate::client::Github;
use crate::error::{Error, Kind, Result, Sub};
use crate::github::releases::{
    DraftRequest, EnsureSpec, MakeLatest, PublishGuard, PublishPatch, ReleaseEdit, Replace,
    UploadFile,
};
use crate::github::{How, Releases, TagObjects};
use crate::journal::{IntentId, IntentStore};
use crate::model::{AssetObject, ReleaseObject, Sha256Digest};
use crate::policy::{Access, Permit, RouteTarget};
use crate::slug::Slug;
use crate::write::{Effect, Ensured, Outcome as WriteOutcome, WriteOp};

/// A step that ended a write: its report and its error, boxed (a report and an error are large).
pub(super) type Ended = Box<(WriteReport, Error)>;

/// A write verb once its repository is named and routed (plan §10, body 1.2).
pub(super) struct Write<'a> {
    /// The command line.
    pub(super) inv: &'a Invocation,
    /// The client.
    pub(super) gh: Github,
    /// The repository.
    pub(super) slug: Slug,
    /// `inv.how(Access::Write, named)` with `--notify`.
    pub(super) how: How,
    /// `--dry-run`.
    pub(super) dry: bool,
}

impl<'a> Write<'a> {
    /// The client, the repository as `inv` names it, and its routing. A literal typed on the
    /// command line is `Named::Literal`, so a release-tier write needs no grant, and the
    /// router refuses `@TARGET` for one (design §6.3).
    ///
    /// # Errors
    ///
    /// `Ctx::github`'s (agh.toml, vault adoption) and Usage for a repository that does not
    /// parse or a target agh.toml lacks: both come before the repository is routed, so the
    /// verb prints no report for them.
    pub(super) fn begin(ctx: &mut Ctx, inv: &'a Invocation) -> Result<Write<'a>> {
        let gh = ctx.github()?;
        let (slug, named) = inv.repo(gh.router())?;
        let how = inv.how(Access::Write, named).notify(inv.has("--notify"));
        Ok(Write {
            inv,
            gh,
            slug,
            how,
            dry: inv.has("--dry-run"),
        })
    }

    /// The permit for this repository. Off unix every write, a dry run included, is
    /// `write::unix_only()`, which prints `not_sent` (plan §1, rule 12).
    ///
    /// # Errors
    ///
    /// The router's refusal; `write::unix_only()` off unix.
    pub(super) fn permit(&self) -> Result<Permit> {
        let permit = self
            .gh
            .permit(&self.how.route(RouteTarget::Repo(self.slug.clone())))?;
        if cfg!(not(unix)) {
            return Err(crate::write::unix_only());
        }
        Ok(permit)
    }

    /// A report of `op` on `key`, marked `--dry-run` when it was given. Its outcome is
    /// `refused` until a step says otherwise.
    pub(super) fn report(&self, op: WriteOp, key: &str) -> WriteReport {
        let report = WriteReport::new(ReportOutcome::Refused.slug(), op, &self.slug, key);
        if self.dry { report.dry_run() } else { report }
    }

    /// This repository's releases, routed as the write is.
    pub(super) fn releases(&self) -> Releases<'_> {
        self.gh.releases(&self.slug).how(self.how.clone())
    }

    /// This routing read as the writer (WriterRead), as a write's guard reads.
    pub(super) fn writer_how(&self) -> How {
        self.how.with_access(Access::WriterRead)
    }

    /// The repository read as the writer, as design §6.4's push check reads it before a
    /// WriterRead 404 may say an object is absent: a missing repository and a missing ref
    /// answer 404 on the same path, and under a profile that covers every repository both are
    /// NotFound, so a ref's NotFound says the ref is absent only after this read found the
    /// repository. `ref create`'s dry run makes it (`ref delete` makes `Refs::push_check`).
    ///
    /// # Errors
    ///
    /// The read's errors (NotFound or NotVisible for a repository that is not there);
    /// Forbidden `NoPush` when its answer says this identity has no push.
    pub(super) fn writer_repo(&self) -> Result<()> {
        let repo = self.gh.repos().how(self.writer_how()).get(&self.slug)?;
        if repo.permissions.as_ref().is_some_and(|p| !p.push) {
            let meter = self.gh.meter();
            let label = meter.label();
            return Err(Error::new(
                Kind::Forbidden,
                format!("as {label}: push is not granted on {}", self.slug),
            )
            .with_sub(Sub::Forbid(crate::error::Forbid::NoPush))
            .with_repo(self.slug.to_string()));
        }
        Ok(())
    }

    /// Whether this verb's row takes `--id` (every release verb but `asset delete`), so a
    /// hint may name it.
    fn takes_id(&self) -> bool {
        self.takes("--id")
    }

    /// Whether this verb's row takes `--confirm` (a Destructive verb), so a hint that names
    /// the release by `--id` adds `--confirm TAG`.
    fn takes_confirm(&self) -> bool {
        self.takes("--confirm")
    }

    /// Whether this verb's row takes `flag`.
    fn takes(&self, flag: &str) -> bool {
        super::verbs::find(&self.inv.path)
            .is_some_and(|verb| verb.flags.iter().any(|spec| spec.name == flag))
    }

    /// The release `which` names, read as the writer (plan §10, body 1.6): `None` when no
    /// release has the tag, drafts included, among the newest 500.
    ///
    /// # Errors
    ///
    /// Conflict `AmbiguousTag` for several releases with the tag, its next the line that names
    /// one by `--id` (`--confirm TAG` added for `release delete`), or, for a verb that takes
    /// no `--id`, the listing that shows them; the reads' errors (a release `--id` names that
    /// is gone is NotFound).
    pub(super) fn find(&self, which: &Which<'_>) -> Result<Option<ReleaseObject>> {
        self.find_cut(which).map(|(found, _)| found)
    }

    /// `find`, and whether a tag's window was cut (`Releases::recent_objects_and_cut`): the
    /// listing went on past the newest 500, so `None` does not say no release has the tag. An
    /// `--id` is never cut.
    ///
    /// # Errors
    ///
    /// As `find`.
    pub(super) fn find_cut(&self, which: &Which<'_>) -> Result<(Option<ReleaseObject>, bool)> {
        let reader = self.gh.releases(&self.slug).how(self.writer_how());
        match which {
            Which::Id(id) => reader.get(*id).map(|release| (Some(release), false)),
            Which::Tag(tag) => match reader.recent_objects_and_cut(tag)? {
                (TagObjects::None, cut) => Ok((None, cut)),
                (TagObjects::One(release), cut) => Ok((Some(*release), cut)),
                (TagObjects::Many(found), _) => {
                    let ids: Vec<u64> = found.iter().map(|release| release.id).collect();
                    let verb = self.inv.path.join(" ");
                    let next = if !self.takes_id() {
                        format!("agh release list {} --drafts", self.slug)
                    } else if self.takes_confirm() {
                        format!("name one: agh {verb} {} --id ID --confirm {tag}", self.slug)
                    } else {
                        format!("name one: agh {verb} {} --id ID", self.slug)
                    };
                    Err(Error::new(
                        Kind::Conflict,
                        format!("{} releases have tag {tag}", ids.len()),
                    )
                    .with_sub(Sub::AmbiguousTag { ids })
                    .with_next(next)
                    .with_repo(self.slug.to_string()))
                }
            },
        }
    }

    /// Whether `error`, raised by `find(which)`, says the release `--id` names is gone: a
    /// NotFound of the release's own read. The push check's NotFound (a repository that is not
    /// there, read as a profile that covers every repository) is not.
    fn release_gone(which: &Which<'_>, error: &Error) -> bool {
        let Which::Id(id) = which else {
            return false;
        };
        error.kind == Kind::NotFound
            && error
                .http
                .as_ref()
                .is_some_and(|http| http.path.ends_with(&format!("/releases/{id}")))
    }

    /// `find`, where no release is NotFound (exit 6).
    ///
    /// # Errors
    ///
    /// As `find`, and NotFound for a tag no release has (its why naming `--id` only for a verb
    /// that takes it).
    pub(super) fn release(&self, which: &Which<'_>) -> Result<ReleaseObject> {
        match (self.find(which)?, which) {
            (Some(release), _) => Ok(release),
            (None, Which::Tag(tag)) => Err(self.no_release_with(tag)),
            (None, Which::Id(id)) => Err(Error::new(
                Kind::NotFound,
                format!("release {id} is not found"),
            )
            .with_repo(self.slug.to_string())),
        }
    }

    /// NotFound (exit 6) for a tag no release among the newest 500 has, drafts included; its
    /// why names `--id` only for a verb that takes it.
    fn no_release_with(&self, tag: &str) -> Error {
        Error::new(
            Kind::NotFound,
            format!(
                "no release of {} has tag {tag}, drafts included, among the newest 500{}",
                self.slug,
                if self.takes_id() {
                    "; name an older draft with --id ID"
                } else {
                    ""
                }
            ),
        )
        .with_next(format!("agh release list {} --drafts", self.slug))
        .with_repo(self.slug.to_string())
    }

    /// Conflict `GuardFailed { what }`, `what` redacted.
    pub(super) fn guard_failed(&self, what: &str) -> Error {
        self.releases().guard_failed(what.to_string())
    }

    /// Run the write and answer by one rule (plan §10, body 1.5). `run` may fill `report` in as
    /// it learns the object; an error it returns prints that report, its outcome
    /// `outcome_of(error)`, with the error's own exit. A Usage error prints no report.
    ///
    /// # Errors
    ///
    /// A Usage error, whatever raised it.
    pub(super) fn finish(
        &self,
        mut report: WriteReport,
        run: impl FnOnce(&mut WriteReport) -> Result<Answer>,
    ) -> Result<Outcome> {
        let answer = match run(&mut report) {
            Ok(answer) => answer,
            Err(error) if error.kind == Kind::Usage => return Err(error),
            Err(error) => failed(refused_as(report, &error), error),
        };
        // Built after the write, so the meter holds its requests and its `notify=` note.
        Ok(Outcome {
            answer,
            receipt: Receipt::for_repo(self.inv, &self.gh, &self.slug),
        })
    }

    /// `finish` for a verb that answers one report per step (`release upload`, `release
    /// ensure`): an error `run` returns before its first step's report prints as an array
    /// holding that report alone, so the verb's answer is always an array (plan §4.6.20). A
    /// Usage error prints no report.
    ///
    /// # Errors
    ///
    /// A Usage error, whatever raised it.
    pub(super) fn finish_items(
        &self,
        report: WriteReport,
        run: impl FnOnce(&mut WriteReport) -> Result<Answer>,
    ) -> Result<Outcome> {
        self.finish(report, |report| match run(report) {
            Err(error) if error.kind != Kind::Usage => {
                Ok(failed_all(&[refused_as(report.clone(), &error)], error))
            }
            answered => answered,
        })
    }
}

/// The release a verb names: its TAG or `--id`.
pub(super) enum Which<'t> {
    /// TAG.
    Tag(&'t str),
    /// `--id ID`.
    Id(u64),
}

impl Which<'_> {
    /// The report's key before the release is read: the tag, or the id's digits.
    fn key(&self) -> String {
        match self {
            Which::Tag(tag) => (*tag).to_string(),
            Which::Id(id) => id.to_string(),
        }
    }
}

/// TAG or `--id ID`, as M1's `named_release` reads them: its Usage texts and the dot-segment
/// rule unchanged.
fn which<'t>(inv: &Invocation, tag: Option<&'t str>) -> Result<Which<'t>> {
    match (tag, inv.flag("--id")) {
        (None, None) => Err(inv.usage("name the release: TAG or --id ID")),
        (Some(_), Some(_)) => Err(inv.usage("TAG and --id contradict each other")),
        (None, Some(id)) => Ok(Which::Id(release_id(inv, id)?)),
        (Some(tag), None) => {
            crate::path::refuse_bad_tag(tag)?;
            Ok(Which::Tag(tag))
        }
    }
}

/// `--id`'s digits.
fn release_id(inv: &Invocation, id: &str) -> Result<u64> {
    crate::path::digits::<u64>(id).ok_or_else(|| inv.usage("--id wants a release id (digits)"))
}

/// The TAG of a verb whose row requires it, under the dot-segment rule.
fn required_tag(inv: &Invocation) -> Result<&str> {
    let tag = inv
        .rest()
        .first()
        .map(String::as_str)
        .ok_or_else(|| inv.usage("TAG is required"))?;
    crate::path::refuse_bad_tag(tag)?;
    Ok(tag)
}

/// `--notes TEXT` or `--notes-file F` (plan §10, body 1.7). Read before the repository is
/// routed, so a refusal here prints no report and nothing is read or sent.
///
/// # Errors
///
/// Usage when both are given; `read_notes`'s errors for the file.
fn notes(inv: &Invocation) -> Result<Option<String>> {
    match (inv.flag("--notes"), inv.flag("--notes-file")) {
        (Some(_), Some(_)) => Err(inv.usage("--notes and --notes-file contradict each other")),
        (Some(notes), None) => Ok(Some(notes.to_string())),
        (None, Some(file)) => read_notes(file).map(Some),
        (None, None) => Ok(None),
    }
}

/// The most bytes a notes file is read to: 125,000 characters (GitHub's most notes, design
/// §11.2) of at most four bytes each. A longer file is refused before it is decoded.
const NOTES_FILE_BYTES_MAX: u64 = 4 * crate::github::releases::NOTES_MAX as u64;

/// `--notes-file F`, opened without waiting (on unix `O_NONBLOCK`: a blocking open of a FIFO
/// with no writer never returns) and judged on the opened handle, as `UploadFile::open` judges
/// a FILE: a regular file, read through at most `NOTES_FILE_BYTES_MAX` bytes and one more.
///
/// # Errors
///
/// could_not_run, `Sub::Local`, why `{F}: {e}`, for a file that cannot be read or is not
/// UTF-8; Invalid (exit 10) for one that is not a regular file or holds more than
/// `NOTES_FILE_BYTES_MAX` bytes.
fn read_notes(file: &str) -> Result<String> {
    use std::io::Read as _;
    let unread = |e: &dyn std::fmt::Display| {
        Error::local(format!("{file}: {e}"))
            .with_next("name a readable UTF-8 file with --notes-file")
    };
    let invalid = |what: String| Error::new(Kind::Invalid, format!("{file}: {what}"));
    let opened = crate::download::open_nonblocking(Path::new(file)).map_err(|e| unread(&e))?;
    let meta = opened.metadata().map_err(|e| unread(&e))?;
    if !meta.is_file() {
        return Err(invalid("not a regular file".to_string()));
    }
    if meta.len() > NOTES_FILE_BYTES_MAX {
        return Err(invalid(format!(
            "over {NOTES_FILE_BYTES_MAX} bytes; GitHub takes at most {} characters of notes",
            crate::github::releases::NOTES_MAX
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    opened
        .take(NOTES_FILE_BYTES_MAX.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| unread(&e))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > NOTES_FILE_BYTES_MAX {
        return Err(invalid(format!(
            "over {NOTES_FILE_BYTES_MAX} bytes; GitHub takes at most {} characters of notes",
            crate::github::releases::NOTES_MAX
        )));
    }
    String::from_utf8(bytes).map_err(|e| unread(&e))
}

/// A value a one-shot write journals as written (its key, the TAG or REF; `--target`; and the
/// routed repository's key, lowercased as the journal writes it, so an upper-case prefix typed
/// is judged by the bytes written), refused before anything is read when it is shaped like a
/// credential, as `IntentStore::begin` would refuse it only after the write's reads (plan §1, rule 11; §10,
/// body 1.1). So the dry run, which begins no intent, refuses it as the real run does.
///
/// # Errors
///
/// Usage, `the {op}'s {field} is shaped like a credential; agh journals it as written or not
/// at all`, next `agh help {verb}`.
pub(super) fn journaled(inv: &Invocation, op: WriteOp, field: &str, value: &str) -> Result<()> {
    if crate::redact::Redactor::shapes(value) == value {
        return Ok(());
    }
    Err(inv.usage(format!(
        "the {}'s {field} is shaped like a credential; agh journals it as written or not at all",
        op.slug()
    )))
}

/// Body 2's draft: titled `--title` or the tag, the notes, `--target`, `--prerelease`, and
/// `make_latest` false. The tag and `--target` pass `journaled` first.
fn draft_request(inv: &Invocation, tag: &str) -> Result<DraftRequest> {
    journaled(inv, WriteOp::ReleaseCreate, "key", tag)?;
    if let Some(target) = inv.flag("--target") {
        journaled(inv, WriteOp::ReleaseCreate, "target", target)?;
    }
    let mut req = DraftRequest::new(tag);
    if let Some(title) = inv.flag("--title") {
        req.name = title.to_string();
    }
    if let Some(notes) = notes(inv)? {
        req.body = notes;
    }
    req.target_commitish = inv.flag("--target").map(str::to_string);
    req.prerelease = inv.has("--prerelease");
    req.make_latest = Some(MakeLatest::False);
    Ok(req)
}

/// Each FILE opened and hashed, in order, before the repository is routed, so a refusal here
/// prints no report and nothing is read or sent: a file over 2 GiB, one whose name GitHub
/// would rename, and one whose asset name an earlier FILE already has (the second upload of
/// the name would be refused after the first was sent) are Invalid (exit 10); one that cannot
/// be read is could_not_run, `Sub::Local`.
fn open_files(paths: &[&str]) -> Result<Vec<UploadFile>> {
    let mut files: Vec<UploadFile> = Vec::with_capacity(paths.len());
    for path in paths {
        let file = UploadFile::open(Path::new(path), None)?;
        if let Some(earlier) = files.iter().find(|earlier| earlier.name == file.name) {
            return Err(Error::new(
                Kind::Invalid,
                format!(
                    "{path}: asset {} is also {}",
                    file.name,
                    earlier.path.display()
                ),
            )
            .with_next("give each FILE its own name"));
        }
        files.push(file);
    }
    Ok(files)
}

/// `report` with `outcome`.
pub(super) fn as_outcome(
    mut report: WriteReport,
    outcome: impl Into<ReportOutcome>,
) -> WriteReport {
    report.outcome = outcome.into().slug().to_string();
    report
}

/// `report` as `error` prints it: its outcome `ReportOutcome::of(error)`, and, outside a dry
/// run, the open intent an `intent_open` refusal names.
pub(super) fn refused_as(mut report: WriteReport, error: &Error) -> WriteReport {
    report.outcome = outcome_of(error).to_string();
    if !report.dry_run
        && report.intent.is_none()
        && let Sub::IntentOpen { intent } = &error.sub
    {
        report.intent = Some(intent.clone());
    }
    report
}

/// A release's report: what the write did, its id and its `html_url`.
pub(super) fn release_report(report: WriteReport, ensured: &Ensured<ReleaseObject>) -> WriteReport {
    as_outcome(report, ensured.effect)
        .with_object(Some(ensured.object.id), Some(&ensured.object.html_url))
}

/// An asset's report: what the write did, its id and its `browser_download_url`.
pub(super) fn asset_report(report: WriteReport, ensured: &Ensured<AssetObject>) -> WriteReport {
    as_outcome(report, ensured.effect).with_object(
        Some(ensured.object.id),
        Some(&ensured.object.browser_download_url),
    )
}

/// Several reports, one per step, every one done.
pub(super) fn items(reports: &[WriteReport]) -> Answer {
    Answer::Items {
        noun: Noun::Write,
        items: reports.iter().map(WriteReport::to_value).collect(),
        completeness: Completeness::Complete,
    }
}

/// Several reports, the last the step `error` ended.
pub(super) fn failed_all(reports: &[WriteReport], error: Error) -> Answer {
    Answer::FailedWith {
        value: Value::Array(reports.iter().map(WriteReport::to_value).collect()),
        error,
    }
}

/// One step's report pushed onto `reports`: `None` to go on, or, for a step that ended the
/// run, its refused report pushed and the whole array's failed answer.
pub(super) fn push_step(
    reports: &mut Vec<WriteReport>,
    step: std::result::Result<WriteReport, Ended>,
) -> Option<Answer> {
    match step {
        Ok(report) => {
            reports.push(report);
            None
        }
        Err(ended) => {
            let (report, error) = *ended;
            reports.push(report);
            Some(failed_all(reports, error))
        }
    }
}

/// A one-step write's answer: done, or its report and the error that ended it.
pub(super) fn answered(step: std::result::Result<WriteReport, Ended>) -> Answer {
    match step {
        Ok(report) => done(report),
        Err(ended) => {
            let (report, error) = *ended;
            failed(report, error)
        }
    }
}

/// A sent step's result as [`push_step`] takes it: its report, made by `made` from what it
/// ensured, or its refused report and error.
///
/// # Errors
///
/// A Usage error, which prints no report, passes through as the run's own.
fn stepped<T>(
    report: WriteReport,
    step: Result<Ensured<T>>,
    made: impl FnOnce(WriteReport, &Ensured<T>) -> WriteReport,
) -> Result<std::result::Result<WriteReport, Ended>> {
    match step {
        Ok(ensured) => Ok(Ok(made(report, &ensured))),
        Err(error) if error.kind == Kind::Usage => Err(error),
        Err(error) => Ok(Err(Box::new((refused_as(report, &error), error)))),
    }
}

/// The intent of the newest `op` on `key` in `journal`: the one a one-shot write began, or
/// the one whose object it converged on. A journal that cannot be read now names none.
pub(super) fn latest_intent(journal: &dyn IntentStore, op: WriteOp, key: &str) -> Option<IntentId> {
    journal
        .latest(op, key)
        .ok()
        .flatten()
        .map(|view| view.intent.id)
}

/// `error` as a one-shot write raised it, its report naming the intent the write left: when
/// its outcome is unknown and the journal's newest `op` on the report's key is not `before`
/// (the newest before the write), this run began that intent, and an unknown outcome leaves it
/// open (a lost create's discovery that found several releases, a settle that failed, an
/// answer that did not parse, a ref found elsewhere after a lost POST).
pub(super) fn naming_intent(
    report: &mut WriteReport,
    journal: &dyn IntentStore,
    op: WriteOp,
    before: Option<&IntentId>,
    error: Error,
) -> Error {
    if ReportOutcome::of(&error) == ReportOutcome::Unknown && report.intent.is_none() {
        let after = latest_intent(journal, op, &report.key);
        if after.as_ref() != before {
            report.intent = after.map(|intent| intent.to_string());
        }
    }
    error
}

/// A one-shot write's `Outcome` as its report (plan §10, body 1.5): `Done` → the effect's
/// slug, the object and the intent; `NeverSent` → `not_sent`; `Refused` → `refused`;
/// `Unknown` → `unknown` with its intent, and the OutcomeUnknown error (exit 13).
fn one_shot<T>(
    report: &WriteReport,
    op: WriteOp,
    journal: &dyn IntentStore,
    outcome: WriteOutcome<Ensured<T>>,
    object: impl FnOnce(&T) -> (Option<u64>, Option<String>),
) -> std::result::Result<(WriteReport, T), Ended> {
    let report = as_outcome(report.clone(), &outcome);
    match outcome {
        WriteOutcome::Done(ensured) => {
            let intent = latest_intent(journal, op, &report.key);
            let (id, url) = object(&ensured.object);
            let report = report
                .with_object(id, url.as_deref())
                .with_intent(intent.as_ref());
            Ok((report, ensured.object))
        }
        WriteOutcome::NeverSent(error) | WriteOutcome::Refused(error) => {
            Err(Box::new((report, error)))
        }
        WriteOutcome::Unknown { intent, error } => {
            let report = report.with_intent(Some(&intent));
            let error = crate::write::unknown_error(&intent, error);
            Err(Box::new((report, error)))
        }
    }
}

/// A one-shot write's whole answer: its report, done or failed.
pub(super) fn one_shot_answer<T>(
    report: &WriteReport,
    op: WriteOp,
    journal: &dyn IntentStore,
    outcome: WriteOutcome<Ensured<T>>,
    object: impl FnOnce(&T) -> (Option<u64>, Option<String>),
) -> Answer {
    answered(one_shot(report, op, journal, outcome, object).map(|(report, _)| report))
}

/// A release's id and `html_url`.
fn release_object(release: &ReleaseObject) -> (Option<u64>, Option<String>) {
    (Some(release.id), Some(release.html_url.clone()))
}

/// `--confirm` against what a destructive verb removes (plan §10, body 7), before anything is
/// read: absent, or not exactly `what`, is Refused (exit 1).
///
/// # Errors
///
/// As above.
pub(super) fn confirm(inv: &Invocation, what: &str) -> Result<()> {
    match inv.flag("--confirm") {
        None => Err(confirm_missing(inv, what)),
        Some(given) => confirm_equals(given, what),
    }
}

/// Refused: `{verb} removes {what}: type it again with --confirm {what}`.
fn confirm_missing(inv: &Invocation, what: &str) -> Error {
    let verb = inv.path.join(" ");
    Error::refused(format!(
        "{verb} removes {what}: type it again with --confirm {what}"
    ))
    .with_next(format!("type it again: --confirm {what}"))
}

/// Refused unless `given` is exactly `expected`: `--confirm {given} is not {expected}`.
pub(super) fn confirm_equals(given: &str, expected: &str) -> Result<()> {
    if given == expected {
        return Ok(());
    }
    Err(
        Error::refused(format!("--confirm {given} is not {expected}"))
            .with_next(format!("type it again: --confirm {expected}")),
    )
}

/// `agh release create REPO TAG --draft …`: a draft release, once (plan §10, body 2).
///
/// # Errors
///
/// Usage without `--draft`, for the flag rules, and for a TAG or `--target` shaped like a
/// credential (`journaled`); `read_notes`'s errors for a `--notes-file` and `Ctx::github`'s
/// refusals (agh.toml, vault adoption), before the repository is routed, with no report; Usage,
/// with no report and nothing read, for a routed repository whose lowercased key is shaped like
/// a credential (`journaled`); every other error after the repository is routed prints its
/// report.
pub(crate) fn release_create(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let tag = required_tag(inv)?;
    if !inv.has("--draft") {
        let repo = inv
            .global
            .repo
            .as_deref()
            .or_else(|| inv.positional(0))
            .unwrap_or("REPO");
        return Err(
            Error::usage("agh release create makes drafts only: add --draft")
                .with_next(format!("agh release publish {repo} {tag} publishes it")),
        );
    }
    let req = draft_request(inv, tag)?;
    let w = Write::begin(ctx, inv)?;
    journaled(inv, WriteOp::ReleaseCreate, "repo", &w.slug.key())?;
    let report = w.report(WriteOp::ReleaseCreate, tag);
    w.finish(report, |report| {
        let permit = w.permit()?;
        if w.dry {
            return dry_run::release_create(&w, &req, report);
        }
        let journal = w.gh.journal(&w.slug)?;
        let before = latest_intent(&journal, WriteOp::ReleaseCreate, tag);
        let outcome = w.releases().create(&permit, &req, &journal).map_err(|e| {
            naming_intent(report, &journal, WriteOp::ReleaseCreate, before.as_ref(), e)
        })?;
        Ok(one_shot_answer(
            report,
            WriteOp::ReleaseCreate,
            &journal,
            outcome,
            release_object,
        ))
    })
}

/// `agh release upload REPO TAG | --id ID FILE... [--clobber]`: one report per file (plan §10,
/// body 3).
///
/// # Errors
///
/// Usage for the positional and flag rules; `open_files`'s errors (Invalid for a FILE GitHub
/// would not take as it is or a name given twice, could_not_run for one that cannot be read)
/// and `Ctx::github`'s refusals (agh.toml, vault adoption), before the repository is routed,
/// with no report; every error after the repository is routed prints the reports so far, an
/// array even when no file was tried.
pub(crate) fn release_upload(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let rest: Vec<&str> = inv.rest().iter().map(String::as_str).collect();
    let (which, paths) = match inv.flag("--id") {
        Some(id) => (Which::Id(release_id(inv, id)?), rest.as_slice()),
        None => match rest.split_first() {
            None => return Err(inv.usage("name the release: TAG or --id ID")),
            Some((tag, paths)) => {
                crate::path::refuse_bad_tag(tag)?;
                (Which::Tag(tag), paths)
            }
        },
    };
    if paths.is_empty() {
        return Err(inv.usage("FILE is required"));
    }
    let files = open_files(paths)?;
    let replace = if inv.has("--clobber") {
        Replace::Clobber
    } else {
        Replace::Never
    };
    let w = Write::begin(ctx, inv)?;
    let first = files
        .first()
        .map_or_else(String::new, |file| file.name.clone());
    let report = w.report(WriteOp::AssetUpload, &first);
    w.finish_items(report, |_| {
        let permit = w.permit()?;
        let release = w.release(&which)?;
        if w.dry {
            return Ok(dry_run::release_upload(&w, &release, &files, replace));
        }
        let releases = w.releases();
        let mut reports = Vec::with_capacity(files.len());
        for file in &files {
            let report = w.report(WriteOp::AssetUpload, &file.name);
            let step = stepped(
                report,
                releases.upload(&permit, &release, file, replace),
                asset_report,
            )?;
            if let Some(answer) = push_step(&mut reports, step) {
                return Ok(answer);
            }
        }
        Ok(items(&reports))
    })
}

/// `--expect-asset NAME=SHA256`, each `HEX` 64 lowercase hex, each NAME once; `None` when none
/// is given.
fn expected_assets(
    inv: &Invocation,
) -> Result<Option<std::collections::BTreeMap<String, Sha256Digest>>> {
    let given = inv.flag_all("--expect-asset");
    if given.is_empty() {
        return Ok(None);
    }
    let mut assets = std::collections::BTreeMap::new();
    for value in given {
        let bad = || {
            inv.usage(format!(
                "--expect-asset wants NAME=SHA256 (64 lowercase hex): {value}"
            ))
        };
        let (name, hex) = value.split_once('=').ok_or_else(bad)?;
        if name.is_empty() || !aterm_codec::hex::is_lower(hex, 64) {
            return Err(bad());
        }
        let digest = Sha256Digest::parse(&format!("sha256:{hex}")).ok_or_else(bad)?;
        if assets.insert(name.to_string(), digest).is_some() {
            return Err(inv.usage(format!("--expect-asset {name} given twice")));
        }
    }
    Ok(Some(assets))
}

/// `agh release publish REPO TAG | --id ID [--prerelease | --no-prerelease]
/// [--expect-asset NAME=SHA256]...` (plan §10, body 4).
///
/// # Errors
///
/// Usage for the flag rules, and `Ctx::github`'s refusals (agh.toml, vault adoption), before
/// the repository is routed, with no report; every error after the repository is routed
/// prints its report.
pub(crate) fn release_publish(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let which = which(inv, inv.rest().first().map(String::as_str))?;
    let prerelease = prerelease_filter(inv)?;
    let expected = expected_assets(inv)?;
    let w = Write::begin(ctx, inv)?;
    let report = w.report(WriteOp::ReleasePublish, &which.key());
    w.finish(report, |report| {
        let permit = w.permit()?;
        let release = w.release(&which)?;
        report.key.clone_from(&release.tag_name);
        let patch = PublishPatch {
            prerelease: prerelease.unwrap_or(release.prerelease),
            make_latest: MakeLatest::False,
        };
        let guard = expected
            .clone()
            .map_or_else(PublishGuard::default, PublishGuard::expecting);
        if w.dry {
            return dry_run::release_publish(&w, &release, &guard, &patch, report);
        }
        let ensured = w.releases().publish(&permit, release.id, &guard, &patch)?;
        Ok(done(release_report(report.clone(), &ensured)))
    })
}

/// `agh release edit REPO TAG | --id ID [--title T] [--notes …] [--prerelease |
/// --no-prerelease]` (plan §10, body 5).
///
/// # Errors
///
/// Usage with no change flag, and for the flag rules; `read_notes`'s errors for a
/// `--notes-file` and `Ctx::github`'s refusals (agh.toml, vault adoption), before the
/// repository is routed, with no report; every error after the repository is routed prints its
/// report.
pub(crate) fn release_edit(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let which = which(inv, inv.rest().first().map(String::as_str))?;
    let prerelease = prerelease_filter(inv)?;
    let body = notes(inv)?;
    let name = inv.flag("--title").map(str::to_string);
    if name.is_none() && body.is_none() && prerelease.is_none() {
        return Err(inv.usage(
            "release edit changes nothing: give --title, --notes, --notes-file, --prerelease or \
             --no-prerelease",
        ));
    }
    let edit = ReleaseEdit {
        name,
        body,
        prerelease,
    };
    let w = Write::begin(ctx, inv)?;
    let report = w.report(WriteOp::ReleaseUpdate, &which.key());
    w.finish(report, |report| {
        let permit = w.permit()?;
        let release = w.release(&which)?;
        report.key.clone_from(&release.tag_name);
        if w.dry {
            return dry_run::release_edit(&w, &release, &edit, report);
        }
        let ensured = w.releases().edit(&permit, release.id, &edit)?;
        Ok(done(release_report(report.clone(), &ensured)))
    })
}

/// `agh release ensure REPO TAG [--title T] [--notes …] [--target REF] [--prerelease]
/// [--file F]... [--publish]`: the release, each asset, then the publish, one report per step
/// (plan §10, body 6).
///
/// # Errors
///
/// Usage for the flag rules, and for a TAG or `--target` shaped like a credential
/// (`journaled`); `read_notes`'s and `open_files`'s errors and `Ctx::github`'s refusals
/// (agh.toml, vault adoption), before the repository is routed, with no report; Usage, with no
/// report and nothing read, for a routed repository whose lowercased key is shaped like a
/// credential (`journaled`); every other error after the repository is routed prints the
/// reports so far, an array even when the release's own step failed.
pub(crate) fn release_ensure(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let tag = required_tag(inv)?;
    let req = draft_request(inv, tag)?;
    let files = open_files(&inv.flag_all("--file"))?;
    let mut spec = EnsureSpec::new(req).with_assets(files);
    if inv.has("--publish") {
        spec = spec.with_publish(PublishPatch {
            prerelease: inv.has("--prerelease"),
            make_latest: MakeLatest::False,
        });
    }
    let w = Write::begin(ctx, inv)?;
    journaled(inv, WriteOp::ReleaseCreate, "repo", &w.slug.key())?;
    let report = w.report(WriteOp::ReleaseCreate, tag);
    w.finish_items(report, |report| {
        let permit = w.permit()?;
        if w.dry {
            return dry_run::release_ensure(&w, &spec, report);
        }
        let journal = w.gh.journal(&w.slug)?;
        ensure_steps(&w, &permit, &spec, &journal, report)
    })
}

/// `Releases::ensure`'s steps, driven here one by one so that a later step's error prints the
/// reports of the steps before it: the draft (created or converged on), each asset with
/// `Replace::Never` (or, on a release this ensure's earlier run published, each asset as it
/// was uploaded), then the publish guarded by exactly those assets.
fn ensure_steps(
    w: &Write<'_>,
    permit: &Permit,
    spec: &EnsureSpec,
    journal: &dyn IntentStore,
    first: &mut WriteReport,
) -> Result<Answer> {
    // An error of the release's own step (`create`'s refusal before the POST, or an unknown
    // outcome naming its intent in `first`) is returned for `Write::finish_items`, which
    // prints it as an array of `first` alone.
    let releases = w.releases();
    let before = latest_intent(journal, WriteOp::ReleaseCreate, &first.key);
    let created = releases
        .create(permit, &spec.draft, journal)
        .map_err(|e| naming_intent(first, journal, WriteOp::ReleaseCreate, before.as_ref(), e))?;
    let (report, release) = match one_shot(
        first,
        WriteOp::ReleaseCreate,
        journal,
        created,
        release_object,
    ) {
        Ok(done) => done,
        Err(ended) => return Ok(failed_all(&[ended.0], ended.1)),
    };
    let mut reports = vec![report];
    for file in &spec.assets {
        let report = w.report(WriteOp::AssetUpload, &file.name);
        let step = if release.draft {
            releases.upload(permit, &release, file, Replace::Never)
        } else {
            uploaded_before(w, &release, file)
        };
        if let Some(answer) = push_step(&mut reports, stepped(report, step, asset_report)?) {
            return Ok(answer);
        }
    }
    if let Some(patch) = &spec.publish {
        let report = w.report(WriteOp::ReleasePublish, &spec.draft.tag_name);
        let expected = spec
            .assets
            .iter()
            .map(|file| (file.name.clone(), file.sha256))
            .collect();
        let step = releases.publish(
            permit,
            release.id,
            &PublishGuard::expecting(expected),
            patch,
        );
        if let Some(answer) = push_step(&mut reports, stepped(report, step, release_report)?) {
            return Ok(answer);
        }
    }
    Ok(items(&reports))
}

/// On a release already published (only this ensure's earlier run can have published it: the
/// created-intent rule found it), `file` must be there as it was uploaded: `Converged`, else
/// Conflict `GuardFailed`.
pub(super) fn uploaded_before(
    w: &Write<'_>,
    release: &ReleaseObject,
    file: &UploadFile,
) -> Result<Ensured<AssetObject>> {
    if let Some(asset) = release.assets.iter().find(|a| a.name == file.name)
        && asset.holds(file.sha256)?
    {
        return Ok(Ensured {
            object: asset.clone(),
            effect: Effect::Converged,
        });
    }
    Err(w.guard_failed(&format!(
        "release {} is published without {} as {}",
        release.id, file.name, file.sha256
    )))
}

/// `agh release delete REPO TAG | --id ID --confirm TAG` (plan §10, body 7).
///
/// # Errors
///
/// Usage for the flag rules; Refused for a `--confirm` absent or not the TAG, before anything
/// is read; `Ctx::github`'s refusals (agh.toml, vault adoption), which come before the
/// repository is routed, with no report; every error after the repository is routed prints
/// its report. A release `--id` names that is gone (the release's own read answered
/// NotFound, not the push check's read of the repository) is `unchanged`, exit 0, as a TAG
/// no release among the newest 500 has is when the listing ended there: a rerun after a lost
/// DELETE answer has nothing to do. A TAG absent past that window is NotFound (exit 6, its
/// report printed), whose why names `--id`.
pub(crate) fn release_delete(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let which = which(inv, inv.rest().first().map(String::as_str))?;
    match &which {
        Which::Tag(tag) => confirm(inv, tag)?,
        Which::Id(_) if !inv.has("--confirm") => {
            return Err(confirm_missing(inv, "the release's tag"));
        }
        Which::Id(_) => {}
    }
    let w = Write::begin(ctx, inv)?;
    let report = w.report(WriteOp::ReleaseDelete, &which.key());
    w.finish(report, |report| {
        let permit = w.permit()?;
        // A delete that finds nothing has nothing to do. `--id`'s read is `Releases::get`,
        // whose push check reads the repository first; under a profile that covers every
        // repository a repository that is not there is NotFound too, so only the release
        // read's own NotFound says the release is gone.
        // A tag no release among the newest 500 has is nothing to delete only when the listing
        // ended there; past that window an older draft may hold it, so it is NotFound, whose
        // why names `--id`.
        let found = match w.find_cut(&which) {
            Err(error) if Write::release_gone(&which, &error) => (None, false),
            found => found?,
        };
        let release = match (found, &which) {
            ((Some(release), _), _) => release,
            ((None, true), Which::Tag(tag)) => return Err(w.no_release_with(tag)),
            ((None, _), _) => {
                return Ok(done(as_outcome(report.clone(), ReportOutcome::Unchanged)));
            }
        };
        report.key.clone_from(&release.tag_name);
        report.id = Some(release.id);
        report.url = Some(release.html_url.clone());
        if let Which::Id(_) = which {
            confirm_equals(inv.flag("--confirm").unwrap_or_default(), &release.tag_name)?;
        }
        if w.dry {
            return Ok(done(as_outcome(report.clone(), ReportOutcome::WouldSend)));
        }
        let deleted = w.releases().delete(&permit, release.id)?;
        Ok(done(as_outcome(report.clone(), deleted)))
    })
}
