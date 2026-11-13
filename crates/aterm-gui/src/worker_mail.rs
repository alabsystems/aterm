// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WORKER'S MAIL TO ITS MANAGER (`docs/DESIGN-workers-subagents-2026-11-04.md` §8, Fabric):
//! each `NEEDS YOU:` line its harness tells, once while it stands, its task's conclusion, once,
//! and its tab's close before either was told, each posted to the manager's inbox as one
//! `kind=report` wherever the manager runs ([`crate::fabric::report`]): on this Mac's bus to a
//! session here, by direct fleet mail to one on the Mac a fleet spawn came from
//! ([`TaskRecord::manager_node`]). A worker started with no manager (`from=ctl`: aterm's own
//! acts, a caller in no tab) mails nobody.
//!
//! The mail is data in the fabric's shape: the sender is the worker's session (on the bus) or its
//! Mac (`fleet:<node>`, direct fleet mail), and its one line opens with what happened (`NEEDS YOU:
//! <need>`, `finished`, `failed: <why in words>`, `stalled: …`, `exited: …`, `closed …`) and then
//! names the worker (`sid=`), its task's `state=` (`closed` once its tab closed, as `await task`
//! answers), `conclusion=` and `why=`, its `result=` path and, last, its `step:`
//! ([`crate::survey::last_words`]). It is no alert, banner or badge. A manager it cannot reach (no
//! bridge here, so this Mac's bus carries nothing; a peer that refuses or does not answer; a
//! manager that is no session here and whose Mac no spawn named; a session here once the worker
//! left this Mac's roster, which the bus carries no word from) is a record in Messages and a line
//! in aterm's log, and nothing else happens: no mail is sent again. A manager's Mac whose fleet
//! access is Read Only takes no mail, a worker's report included (direct fleet mail is a `post`
//! drive, which needs Read and Write there): the record says so once, and that the manager reads
//! this worker with `await task` and `@<sid> result`, which every level keeps.
//!
//! What was told lives on the task record ([`Told`]), which a seamless update and a restore carry,
//! so a need its harness tells again sends nothing, in this process or its successor. A need
//! counts as told once its send finished, delivered or recorded; one a send still has in hand is
//! told by nobody else here, and is not carried, so a successor sends again what an update or a
//! quit cut short. A need that stops standing is forgotten, so one raised again is news again.
//! Each mail's key is its content's hash (the worker, its task, the rev it was told at, what it
//! tells and its whole line), so the same mail sent twice is one message wherever it lands (the
//! broker's dedup, the other Mac's mailbox), and no two mails share a key.

use std::collections::BTreeSet;
use std::sync::Arc;

use aterm_session::SessionId;

use crate::SessionCtx;
use crate::fabric::report::{self, To, Unsent};
use crate::session_store::Store;
use crate::task_record::{Conclusion, FROM_CTL, TaskRecord, TaskState};

/// The most bytes of a need a mail quotes, once folded to one line.
const NEED_MAX: usize = 1024;

/// One thing a worker's mail tells, by its identity within its task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Item {
    /// A need, by [`need_hash`].
    Need(u64),
    /// The conclusion the record entered at this rev.
    End(u64),
}

/// WHAT A WORKER'S MAIL HAS TOLD ITS MANAGER of its task ([`TaskRecord::told`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Told {
    /// The needs standing that a finished send told, by [`need_hash`].
    needs: BTreeSet<u64>,
    /// The `entered` rev of the conclusion a finished send told.
    end: Option<u64>,
    /// What a send in hand took and has not finished: owed nobody else in this process, and
    /// never carried, so a successor owes it again.
    sending: BTreeSet<Item>,
}

/// One thing a worker's manager is owed word of.
#[derive(Clone, Debug, PartialEq, Eq)]
enum News {
    /// A `NEEDS YOU:` line, as its harness told it.
    Need(String),
    /// The task's conclusion, as its record stands.
    End,
    /// The worker's tab closed before its conclusion was told.
    Closed,
}

impl Told {
    /// What of `record` and the needs standing beside it (`standing`, in the worker's order;
    /// `None` where an entry stands that no told set describes yet) is owed now, each marked as a
    /// send's in hand: every standing need neither told nor in hand, then the conclusion where
    /// the record has one neither told nor in hand. A told need that no longer stands is
    /// forgotten, and so is one in hand, whose send then marks nothing.
    fn take(&mut self, record: &TaskRecord, standing: Option<&[String]>) -> Vec<News> {
        let mut news = Vec::new();
        if let Some(standing) = standing {
            let hashes: Vec<u64> = standing.iter().map(|n| need_hash(record, n)).collect();
            self.needs.retain(|hash| hashes.contains(hash));
            self.sending.retain(|item| match item {
                Item::Need(hash) => hashes.contains(hash),
                Item::End(_) => true,
            });
            for (need, hash) in standing.iter().zip(hashes) {
                if !self.needs.contains(&hash) && self.sending.insert(Item::Need(hash)) {
                    news.push(News::Need(need.clone()));
                }
            }
        }
        if record.conclusion().is_some()
            && !self.took_end(record.entered)
            && self.sending.insert(Item::End(record.entered))
        {
            news.push(News::End);
        }
        news
    }

    /// Whether the conclusion entered at `entered` was told, or a send has it in hand.
    fn took_end(&self, entered: u64) -> bool {
        self.end == Some(entered) || self.sending.contains(&Item::End(entered))
    }

    /// The send that had `item` in hand finished, delivered or recorded: it is told, unless it was
    /// forgotten meanwhile ([`Self::take`]).
    fn finish(&mut self, item: Item) {
        if self.sending.remove(&item) {
            match item {
                Item::Need(hash) => {
                    self.needs.insert(hash);
                }
                Item::End(entered) => self.end = Some(entered),
            }
        }
    }

    /// No send took `item` after all: it is owed again.
    fn unsend(&mut self, item: Item) {
        self.sending.remove(&item);
    }

    /// What was told, as a carry row holds it, for a record whose current state was entered at
    /// `entered`: ` told=<16 hex>,…` for the needs and ` told_end=1` for its conclusion; nothing a
    /// send has in hand.
    pub(crate) fn push_carry(&self, entered: u64, out: &mut String) {
        if !self.needs.is_empty() {
            let needs: Vec<String> = self.needs.iter().map(|h| format!("{h:016x}")).collect();
            out.push_str(" told=");
            out.push_str(&needs.join(","));
        }
        if self.end == Some(entered) {
            out.push_str(" told_end=1");
        }
    }

    /// What a carry row's `told=` and `told_end=` say was told ([`Self::push_carry`]), of a record
    /// whose state was entered at `entered`. A `told=` that does not read is no need told: its
    /// needs are sent again, one message where they land while their lines are the same.
    pub(crate) fn of_carry(needs: Option<&str>, end: Option<&str>, entered: u64) -> Self {
        let needs = needs
            .and_then(|needs| {
                needs
                    .split(',')
                    .map(|hash| {
                        (hash.len() == 16)
                            .then(|| u64::from_str_radix(hash, 16).ok())
                            .flatten()
                    })
                    .collect::<Option<BTreeSet<u64>>>()
            })
            .unwrap_or_default();
        Self {
            needs,
            end: (end == Some("1")).then_some(entered),
            sending: BTreeSet::new(),
        }
    }
}

/// A need's identity within its task: its words, folded as every reader folds them.
fn need_hash(record: &TaskRecord, need: &str) -> u64 {
    let folded = crate::needs::fold_clip(need, usize::MAX);
    aterm_hash::fnv1a_64(format!("{}\0{folded}", record.started_unix_ms).as_bytes())
}

/// Where a worker's manager runs ([`TaskRecord::from`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Manager {
    /// A session of this Mac.
    Here(String),
    /// A session on the Mac `node`, which the worker's fleet spawn came from.
    There { sid: String, node: String },
    /// A session that is not here, on no Mac a spawn named: its tab closed, or it was never this
    /// Mac's.
    Unknown(String),
}

impl Manager {
    /// The manager of `record`, a task started with one (no `from=ctl`), as `store` holds this
    /// Mac's sessions.
    fn of(record: &TaskRecord, store: &Store) -> Self {
        let sid = record.from.clone();
        let here = || {
            store
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .by_sid(&SessionId::new(&sid))
                .is_some()
        };
        match &record.manager_node {
            Some(node) => Self::There {
                sid,
                node: node.clone(),
            },
            None if here() => Self::Here(sid),
            None => Self::Unknown(sid),
        }
    }

    /// The manager as aterm's log and Messages name it.
    fn label(&self) -> String {
        match self {
            Self::Here(sid) | Self::Unknown(sid) => sid.clone(),
            Self::There { sid, node } => format!("{sid}@{node}"),
        }
    }
}

/// Why a mail reached no manager ([`Owed::send`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Undelivered {
    /// Its transport took nothing ([`report::post`]).
    Unsent(Unsent),
    /// The manager is no session here, and no fleet spawn named the Mac it runs on.
    NoRoute(String),
}

impl std::fmt::Display for Undelivered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsent(why) => why.fmt(f),
            Self::NoRoute(sid) => write!(
                f,
                "{sid} is no session here, and no fleet spawn named the Mac it runs on"
            ),
        }
    }
}

/// One mail: what it tells, the idempotency key it goes under, and its line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mail {
    /// What happened, the line's head (`NEEDS YOU: …`, `finished`, …).
    pub(crate) subject: String,
    pub(crate) key: String,
    pub(crate) text: String,
    /// What its send tells ([`Told::finish`]); none for a close, whose record leaves with it.
    item: Option<Item>,
}

/// What a worker owes its manager word of now, already taken in hand ([`owed`]).
pub(crate) struct Owed {
    worker: Arc<SessionCtx>,
    /// The task it is of, by when it began: what its sends mark told.
    task: u64,
    /// The Messages lane of the thread that took it (`0` outside a unit test).
    lane: u64,
    /// Whether the worker is leaving this Mac's roster ([`report::post_leaving`]).
    leaving: bool,
    pub(crate) manager: Manager,
    pub(crate) mails: Vec<Mail>,
}

/// WHAT WORKER `ctx` OWES ITS MANAGER WORD OF NOW, taken in hand on its task record as it is taken
/// ([`Told::take`]); `None` where it owes nothing: no task, no manager, nothing new. `store` is
/// this Mac's sessions, where a manager here is found and where the worker's step is read.
pub(crate) fn owed(ctx: &Arc<SessionCtx>, store: &Store) -> Option<Owed> {
    let managed = |record: &&TaskRecord| record.from != FROM_CTL;
    let timeline = || {
        ctx.timeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    // Most sessions are no manager's workers: they read no needs.
    timeline().task().filter(managed)?;
    let standing = standing_needs(ctx);
    let (record, news) = {
        let mut timeline = timeline();
        let record = timeline.task().filter(managed).cloned()?;
        let mut told = record.told.clone();
        let news = told.take(&record, standing.as_deref());
        if let Some(held) = timeline.task_told_mut(record.started_unix_ms) {
            *held = told;
        }
        (record, news)
    };
    if news.is_empty() {
        return None;
    }
    let manager = Manager::of(&record, store);
    Some(owed_of(ctx, store, &record, manager, &news, false))
}

/// WHAT WORKER `ctx` OWES ITS MANAGER AS ITS TAB CLOSES, on its way off this Mac's roster: word
/// of the close (`state=closed`, as `await task` answers it), where its task's conclusion was
/// neither told nor in a send's hand; `None` where it owes nothing.
pub(crate) fn owed_at_close(ctx: &Arc<SessionCtx>, store: &Store) -> Option<Owed> {
    let record = ctx
        .timeline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .task()
        .filter(|record| record.from != FROM_CTL)
        .cloned()?;
    if record.conclusion().is_some() && record.told.took_end(record.entered) {
        return None;
    }
    let manager = Manager::of(&record, store);
    Some(owed_of(ctx, store, &record, manager, &[News::Closed], true))
}

/// The mails of `news` from worker `ctx`, whose record is `record`, to `manager`.
fn owed_of(
    ctx: &Arc<SessionCtx>,
    store: &Store,
    record: &TaskRecord,
    manager: Manager,
    news: &[News],
    leaving: bool,
) -> Owed {
    let handle = store
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .by_sid(&ctx.self_id)
        .cloned();
    let step = handle
        .as_ref()
        .and_then(|h| crate::survey::last_words(h, record.conclusion().is_some()));
    let sid = ctx.self_id.as_str();
    Owed {
        worker: Arc::clone(ctx),
        task: record.started_unix_ms,
        lane: crate::message_inbox::current_lane(),
        leaving,
        manager,
        mails: news
            .iter()
            .map(|news| mail(sid, news, record, step.as_deref()))
            .collect(),
    }
}

/// Every need standing on `ctx`'s session as its harness told them (a proven self-claim's own, or
/// aterm's loop's: [`crate::needs::told_of`]), beside the `owner=needs` entry they describe: none
/// where no entry stands, and `None` where one stands that no told set describes yet.
fn standing_needs(ctx: &SessionCtx) -> Option<Vec<String>> {
    let entry = ctx
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .attention_owners
        .get(crate::harness_host::NEEDS_ATTENTION_OWNER)
        .map(str::to_owned);
    let Some(entry) = entry else {
        return Some(Vec::new());
    };
    crate::needs::told_of(ctx, crate::metrics::now_us())
        .words_for(Some(&entry))
        .map(<[String]>::to_vec)
}

/// The mail of `news` from worker `sid`, whose record is `record` and whose step is `step`.
fn mail(sid: &str, news: &News, record: &TaskRecord, step: Option<&str>) -> Mail {
    let (subject, what, item) = match news {
        News::Need(need) => (
            format!(
                "NEEDS YOU: {}",
                crate::needs::fold_clip(&crate::survey::redact(need), NEED_MAX)
            ),
            "need",
            Some(Item::Need(need_hash(record, need))),
        ),
        News::End => (end_words(record), "end", Some(Item::End(record.entered))),
        News::Closed => (
            match record.conclusion() {
                Some(_) => format!("{}, then its tab closed", end_words(record)),
                None => "closed before its task concluded".to_owned(),
            },
            "end",
            None,
        ),
    };
    let text = line(sid, &subject, record, step, matches!(news, News::Closed));
    let id = match item {
        Some(Item::Need(hash)) => format!("need:{hash:016x}"),
        Some(Item::End(entered)) => format!("end:{entered}"),
        None => "closed".to_owned(),
    };
    let hash = aterm_hash::fnv1a_64(
        format!("{}\0{}\0{id}\0{text}", record.started_unix_ms, record.rev).as_bytes(),
    );
    Mail {
        subject,
        key: key(sid, what, hash),
        text,
        item,
    }
}

/// The idempotency key of a mail of worker `sid`: `<sid>.<what>.<16 hex>`, or, for a sid too long
/// for a key, the sid's own hash in its place.
fn key(sid: &str, what: &str, hash: u64) -> String {
    let key = format!("{sid}.{what}.{hash:016x}");
    if key.len() <= crate::fabric::KEY_MAX {
        key
    } else {
        format!(
            "w{:016x}.{what}.{hash:016x}",
            aterm_hash::fnv1a_64(sid.as_bytes())
        )
    }
}

/// How `record`'s task ended, the head of its conclusion's mail.
fn end_words(record: &TaskRecord) -> String {
    match (record.state, record.conclusion()) {
        (TaskState::Done, _) => "finished".to_owned(),
        (TaskState::Exited, Some(Conclusion::Finished)) => {
            "finished, then its agent exited".to_owned()
        }
        (TaskState::Stalled, _) => {
            format!("stalled: {}", crate::task_record::stall_words(record.why))
        }
        (state, _) => format!(
            "{}: {}",
            state.as_str(),
            crate::task_record::failed_words(state, record.why)
        ),
    }
}

/// The mail's one line: `subject`, then `| sid= state= conclusion= why= result=`, the result
/// path percent-encoded and `-` for none, then `| step: <step>` where the worker has one, folded
/// to one line. A worker whose tab `closed` reads `state=closed` and, with no conclusion of its
/// own, `conclusion=failed`, as `await task` answers it. A line longer than a report carries
/// ([`report::REPORT_MAX`]) goes without its step, then without its result path, which `aterm ctl
/// @<sid> result` reads all the same.
fn line(sid: &str, subject: &str, record: &TaskRecord, step: Option<&str>, closed: bool) -> String {
    let (state, conclusion) = if closed {
        (
            "closed",
            record.conclusion().unwrap_or(Conclusion::Failed).word(),
        )
    } else {
        (
            record.state.as_str(),
            record.conclusion().map_or("-", Conclusion::word),
        )
    };
    let head = format!(
        "{subject} | sid={sid} state={state} conclusion={conclusion} why={}",
        record.why.unwrap_or("-"),
    );
    let result = format!(
        " result={}",
        record.result_path.as_ref().map_or_else(
            || "-".to_owned(),
            |path| crate::control::pct_encode(&path.to_string_lossy())
        )
    );
    let step = step
        .map(|step| crate::needs::fold_clip(step, usize::MAX))
        .filter(|step| !step.is_empty())
        .map(|step| format!(" | step: {step}"));
    let step = step.as_deref().unwrap_or("");
    [format!("{head}{result}{step}"), format!("{head}{result}")]
        .into_iter()
        .find(|line| line.len() <= report::REPORT_MAX)
        .unwrap_or(head)
}

impl Owed {
    /// Send each mail in order, on this thread, from the worker's session: what each send answered
    /// ([`report::post`]), each told on the worker's record as its send finishes. One nothing took
    /// is a record in Messages and a line in aterm's log.
    pub(crate) fn send(&self) -> Vec<Result<String, Undelivered>> {
        self.send_as(&*self.worker)
    }

    /// [`Self::send`] through `worker`, the worker's session as the fabric reaches it (a test's
    /// fleet drive).
    pub(crate) fn send_as(
        &self,
        worker: &dyn crate::fabric::FabricSession,
    ) -> Vec<Result<String, Undelivered>> {
        let sid = worker.self_id().as_str();
        let manager = self.manager.label();
        let post = if self.leaving {
            report::post_leaving
        } else {
            report::post
        };
        self.mails
            .iter()
            .map(|mail| {
                let sent = match &self.manager {
                    Manager::Here(to) => post(worker, To::Here(to), &mail.key, &mail.text),
                    Manager::There { sid: to, node } => {
                        post(worker, To::There { sid: to, node }, &mail.key, &mail.text)
                    }
                    Manager::Unknown(to) => {
                        return self.unsent(sid, &manager, mail, Undelivered::NoRoute(to.clone()));
                    }
                };
                match sent {
                    Ok(answer) => {
                        aterm_log::info!(
                            "worker {sid}: mailed its manager {manager} (key {}): {answer}",
                            mail.key
                        );
                        self.told(mail);
                        Ok(answer)
                    }
                    Err(why) => self.unsent(sid, &manager, mail, Undelivered::Unsent(why)),
                }
            })
            .collect()
    }

    /// `mail`, which reached no manager for `why`: a line in aterm's log and a record in Messages,
    /// and told all the same, so it is not sent again.
    fn unsent(
        &self,
        sid: &str,
        manager: &str,
        mail: &Mail,
        why: Undelivered,
    ) -> Result<String, Undelivered> {
        aterm_log::info!(
            "worker {sid}: its manager {manager} took no mail (key {}): {why}",
            mail.key
        );
        crate::message_inbox::queue_message_in(
            self.lane,
            crate::message_reporters::worker_mail_unsent_record(
                sid,
                manager,
                &mail.subject,
                &why.to_string(),
                &mail.key,
            ),
        );
        self.told(mail);
        Err(why)
    }

    /// `mail`'s send finished: what it tells is told on the worker's record.
    fn told(&self, mail: &Mail) {
        if let Some(item) = mail.item {
            self.mark(|told| told.finish(item));
        }
    }

    /// `edit` on what the worker's record says was told, where it is still this task's.
    fn mark(&self, edit: impl Fn(&mut Told)) {
        let mut timeline = self
            .worker
            .timeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(told) = timeline.task_told_mut(self.task) {
            edit(told);
        }
    }

    /// [`Self::send`] on a thread of its own, since a drive to another Mac may take its whole
    /// deadline: the window never waits on it. With no thread to send them, its mails are owed
    /// again.
    pub(crate) fn send_beside(self) {
        let sid = self.worker.self_id.as_str().to_owned();
        let (worker, task) = (Arc::clone(&self.worker), self.task);
        let items: Vec<Item> = self.mails.iter().filter_map(|mail| mail.item).collect();
        if let Err(e) = std::thread::Builder::new()
            .name("worker-mail".to_owned())
            .spawn(move || {
                let _ = self.send();
            })
        {
            aterm_log::warn!("worker {sid}: no thread started to mail its manager: {e}");
            let mut timeline = worker
                .timeline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(told) = timeline.task_told_mut(task) {
                for item in items {
                    told.unsend(item);
                }
            }
        }
    }
}

impl crate::App {
    /// Send what worker `session` owes its manager word of now ([`owed`]), beside the window: on
    /// its task record's move, a needs fact and a metadata move (a needs entry that came after its
    /// set).
    pub(crate) fn mail_worker_news(&self, session: u64) {
        let Some(ctx) = self.pool.get(session).map(|s| Arc::clone(&s.ctx)) else {
            return;
        };
        if let Some(owed) = owed(&ctx, &self.store) {
            owed.send_beside();
        }
    }

    /// Send what worker `ctx`, whose tab is closing, owes its manager ([`owed_at_close`]), beside
    /// the window, while its session is still on this Mac's roster to read its step. Not where a
    /// quit took the session into its restore manifest (`App::quit_capture`): the worker comes
    /// back with the next launch, and tells how its task ended then. Returns whether a mail went.
    pub(crate) fn mail_worker_close(&self, ctx: &Arc<SessionCtx>) -> bool {
        if self.quit_capture.is_some() {
            return false;
        }
        owed_at_close(ctx, &self.store)
            .map(Owed::send_beside)
            .is_some()
    }
}

#[cfg(test)]
#[path = "worker_mail_tests.rs"]
mod tests;
