// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact native ref writes. An uncertain outcome is never a lost lease.
//!
//! Every journaled write here (an atomic batch or a control transaction) goes through one
//! path, [`Remote::journaled`]. agh keeps one intent bucket for a repository's atomic ref
//! writes, and its `begin` refuses a write while another's intent is open in it: a landing's
//! push from this Mac, another cutter's, or this process's own lease worker. That refusal
//! sent nothing, so the path waits for the named intent to close, reading agh's journal
//! through its library (`Journal::view`, no subprocess), and makes the write again; the
//! native push then judges the refs as they are. Behind an intent whose writer is gone, each
//! push reads that intent's batch from origin, and a write it still refuses is made again
//! after a doubling pause, a bounded number of times ([`RECONCILE_PAUSE`]). The threads of
//! one process take turns before they begin, so they never meet each other's intents at all.

use super::{Remote, STALL_WINDOW, origin_slug};
use agh::IntentStore as _;
use agh::git::GitRun;
use agh::github::refs::{AtomicRefBatch, RefUpdate};
use agh::policy::{RouteRequest, RouteTarget};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// How long one native ref write waits, in all, behind other writers' open intents before it
/// refuses. A write in flight was measured open for up to about 50 s on this lab's Macs (agh
/// opens the intent before it verifies the pushed history and closes it when the push
/// ends; intent 1967-fc387e8b41a0 of 2026-11-13: begun 11:13:01, created by 11:13:54), and a
/// cut, its lease worker and three landings were writing at once. Five minutes is six such
/// writes in turn, and under the half hour of the shortest lease
/// (`line_claim::lease_s`). An intent whose writer, agh and its git child alike, is gone is
/// `closable` (`agh::Journal::closable`): that closes nothing, and no window passes after which
/// it does. It lets the next native write read the intent's recorded batch from origin before
/// it begins, closing it only where every ref reads as before its write (retracted) or every
/// one as after it (found), so the wait on it ends and the write is made again, paced
/// ([`RECONCILE_PAUSE`]). No time releases an intent: one whose writer still runs or could not
/// be judged gone, one that records no writer, and one whose refs origin answers neither way
/// stay open, and once this wait is spent a write one of them still stops ends refused, naming
/// it.
pub(super) const INTENT_WAIT: Duration = Duration::from_secs(300);
/// How often a waiting write reads the journal for the intent it waits on, and the slice of
/// a pause ([`RECONCILE_PAUSE`]) between two looks at whether the write should stop.
pub(super) const INTENT_POLL: Duration = Duration::from_millis(500);
/// The first pause before a write is made again behind an intent whose writer is gone, once a
/// push made after this write found that writer gone was refused again: agh read the intent's
/// refs on origin neither wholly as before its write nor wholly as after it, or could not read
/// them, and its refusal is the same for both. The pause doubles each time (2, 4, 8, 16 and
/// 32 s, 62 s in all, inside [`INTENT_WAIT`]), so a read failure that clears is outlasted and a
/// mixed origin, which stays mixed until someone retracts the intent, ends the write after
/// seven pushes, each a read of origin that sent nothing, instead of one read after another
/// for the whole wait.
pub(super) const RECONCILE_PAUSE: Duration = Duration::from_secs(2);
/// How many pauses ([`RECONCILE_PAUSE`]) a write takes in a row behind one intent whose writer
/// is gone before it ends refused, naming the intent and `agh intent retract`.
pub(super) const RECONCILE_RETRIES: u32 = 5;

/// What a write asked of each ref: the object it holds afterwards, or `None` for its absence.
type Wanted = Vec<(String, Option<String>)>;

/// The turns of this process's threads, one lock for each repository's journal: a write holds
/// its journal's from before its intent begins until the intent is settled, so the cut's main
/// thread and its lease worker ([`crate::line_claim`]) are one writer in turn, never two that
/// refuse each other. Other processes are met through the journal ([`Remote::journaled`]).
static TURNS: Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>> = Mutex::new(BTreeMap::new());

fn turn(journal: &Path) -> Arc<Mutex<()>> {
    Arc::clone(
        TURNS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(journal.to_path_buf())
            .or_default(),
    )
}

/// The waits on another writer's intent that this process's threads are in now, each by the
/// line it said. A holder in such a wait runs nothing and writes nothing, and is not hung:
/// the lease heartbeat reads this as its evidence ([`intent_wait`]).
static WAITING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The line of a wait on another writer's intent that a thread of this process is in now.
/// The wait is bounded ([`INTENT_WAIT`]), so this evidence ends by itself.
pub(crate) fn intent_wait() -> Option<String> {
    WAITING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .first()
        .cloned()
}

/// Every line of a wait that this process's threads are in now: a test asks whether its own
/// wait's line is held, which [`intent_wait`] (the first line, any thread's) cannot answer
/// while other tests wait beside it.
#[cfg(test)]
pub(super) fn intent_waits() -> Vec<String> {
    WAITING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// One thread's wait, in [`WAITING`] until it is dropped.
struct Waiting(String);

impl Waiting {
    fn new(line: &str) -> Self {
        WAITING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line.into());
        Self(line.into())
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let mut waiting = WAITING.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = waiting.iter().position(|line| *line == self.0) {
            waiting.remove(at);
        }
    }
}

/// One journaled write as the path that waits for it knows it.
struct Asked<'a> {
    /// What the journal records of this write ([`batch_request`], [`control_request`]): an
    /// open intent with the same `request` is an earlier attempt at this same write.
    request: String,
    /// What the write asks of each ref.
    wanted: Wanted,
    /// Whether a wait or a pause behind another writer's intent should end now.
    stop: &'a dyn Fn() -> bool,
}

/// The `request` agh's journal records of this exact batch's native push: the digest of the
/// canonical bytes of the run agh makes of it (`Github::push_git_once` takes the same two
/// steps), naming the repository, each ref and its expected and new objects. Nothing is read
/// or sent.
pub(super) fn batch_request(slug: &agh::slug::Slug, repo: &Path, batch: &AtomicRefBatch) -> String {
    agh::fx::canon::request_digest(&agh::fx::canon::git_request(&GitRun::push_refs(
        slug,
        repo,
        batch.clone(),
        false,
    )))
}

/// As [`batch_request`], for a control transaction. agh exports no constructor for a control
/// push's run, and the canonical bytes read only the run's repository and its op, so the op
/// is set on a run of the same repository; a test holds the result equal to the request
/// agh's journal records of a real control push.
pub(super) fn control_request(
    slug: &agh::slug::Slug,
    repo: &Path,
    transaction: &agh::git::ControlRefTransaction,
) -> String {
    let mut run = GitRun::check_local(slug, repo);
    run.op = agh::git::GitOp::PushControl {
        dir: repo.to_path_buf(),
        transaction: transaction.clone(),
        dry_run: false,
    };
    agh::fx::canon::request_digest(&agh::fx::canon::git_request(&run))
}

/// The pushes made behind one intent whose writer is gone, carried across the passes of one
/// [`Remote::journaled`] call: agh's next push reads such an intent's batch from origin, and
/// each refusal after that read is paced ([`RECONCILE_PAUSE`]) and counted
/// ([`RECONCILE_RETRIES`]).
#[derive(Default)]
struct Reconciling {
    /// The intent, once a pass of this write found its writer gone.
    intent: Option<agh::IntentId>,
    /// The pauses this write took behind it so far.
    repeats: u32,
}

impl Reconciling {
    /// A write that found `intent`'s writer gone just now and has paused behind it not yet.
    fn of(intent: &agh::IntentId) -> Self {
        Self {
            intent: Some(intent.clone()),
            repeats: 0,
        }
    }
}

/// What a refused write does once the intent that refused it was looked at.
enum After {
    /// Make the write again, where the native push judges the refs as they then are: the
    /// intent closed, or its writer is gone and that push reads its batch from origin first.
    Again,
    /// The intent was an earlier attempt at this same write, and origin holds every ref it
    /// asked for: the write is made, and nothing is sent again.
    Made,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RefWrite {
    Written,
    Conflict,
    /// A transport/receive failure with no observed ref effects. The caller
    /// must re-read its exact preconditions before repeating the transaction.
    Transient(String),
}

pub(crate) enum ClaimWrite {
    Written,
    Conflict,
    InventoryChanged { main: String, why: String },
}

impl Remote {
    /// The canonical control protocol has its own validated operation; ordinary
    /// immutable tags and commit-ancestry updates never acquire this authority.
    pub(crate) fn control_refs(
        &self,
        transaction: agh::git::ControlRefTransaction,
    ) -> Result<RefWrite, String> {
        if origin_slug(&self.repo)? != self.slug {
            return Err("origin changed before the native control transaction".into());
        }
        let request = RouteRequest::write(RouteTarget::Repo(self.slug.clone()))
            .with_named(self.named.clone());
        let permit = self
            .client
            .permit(&request)
            .map_err(|error| self.native_write_error(&error))?;
        let refs = self.client.refs(&self.slug);
        let asked = Asked {
            request: control_request(&self.slug, &self.repo, &transaction),
            wanted: transaction
                .changes()
                .iter()
                .map(|change| (change.reference.clone(), change.after.clone()))
                .collect(),
            stop: &|| false,
        };
        let result = self.journaled(&asked, &|intents| {
            refs.push_control_with_progress_window(
                &permit,
                &self.repo,
                &transaction,
                intents,
                STALL_WINDOW,
            )
            .map(|outcome| outcome.map(drop))
        })?;
        match result {
            Err(error) if is_transient(&error) => {
                Ok(RefWrite::Transient(self.native_write_error(&error)))
            }
            result => self.ref_result(result),
        }
    }

    /// Publish one exact local object with the caller's expected old ref.
    /// Native agh owns authentication, ancestry checks and the durable intent.
    pub(crate) fn write_ref(
        &self,
        reference: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<RefWrite, String> {
        let batch = AtomicRefBatch::new(vec![RefUpdate {
            reference: reference.into(),
            before: before
                .unwrap_or("0000000000000000000000000000000000000000")
                .into(),
            after: after.into(),
        }])
        .map_err(|error| super::words(&self.client, &error))?;
        self.write_batch(batch)
    }

    /// Preserve a caller's one atomic expected-OID transaction.
    pub(crate) fn write_batch(&self, batch: AtomicRefBatch) -> Result<RefWrite, String> {
        self.ref_result(self.write_batch_result(batch, &|| false)?)
    }

    /// A claim's compare-and-swap. `stop` ends a wait or a pause behind another writer's intent
    /// (the lease worker's monitor stopping): the write is then refused, having sent nothing.
    pub(crate) fn write_claim_ref(
        &self,
        reference: &str,
        before: Option<&str>,
        after: &str,
        main: Option<&str>,
        stop: &dyn Fn() -> bool,
    ) -> Result<ClaimWrite, String> {
        let mut batch = AtomicRefBatch::new(vec![RefUpdate {
            reference: reference.into(),
            before: before.unwrap_or(super::NULL_OID).into(),
            after: after.into(),
        }])
        .map_err(|error| self.native_write_error(&error))?;
        if let Some(main) = main {
            batch = batch
                .observing_main(main)
                .map_err(|error| self.native_write_error(&error))?;
        }
        let result = self.write_batch_result(batch, stop)?;
        if let Err(error) = &result
            && error.kind == agh::Kind::CouldNotRun
            && error.side_effect == agh::SideEffect::None
            && let agh::Sub::GitInventoryChanged { main } = &error.sub
        {
            return Ok(ClaimWrite::InventoryChanged {
                main: main.clone(),
                why: self.native_write_error(error),
            });
        }
        match self.ref_result(result)? {
            RefWrite::Written => Ok(ClaimWrite::Written),
            RefWrite::Conflict => Ok(ClaimWrite::Conflict),
            RefWrite::Transient(why) => Err(why),
        }
    }

    fn write_batch_result(
        &self,
        batch: AtomicRefBatch,
        stop: &dyn Fn() -> bool,
    ) -> Result<agh::Result<()>, String> {
        if origin_slug(&self.repo)? != self.slug {
            return Err("origin changed before the native ref write".into());
        }
        let request = RouteRequest::write(RouteTarget::Repo(self.slug.clone()))
            .with_named(self.named.clone());
        let permit = self
            .client
            .permit(&request)
            .map_err(|error| self.native_write_error(&error))?;
        let refs = self.client.refs(&self.slug);
        let asked = Asked {
            request: batch_request(&self.slug, &self.repo, &batch),
            wanted: batch
                .updates()
                .iter()
                .map(|update| (update.reference.clone(), Some(update.after.clone())))
                .collect(),
            stop,
        };
        // run_git is only the isolated runner. The resource boundary opens and
        // settles the repository-wide intent, preserving unknown outcomes.
        self.journaled(&asked, &|intents| {
            refs.push_atomic_with_progress_window(
                &permit,
                &self.repo,
                &batch,
                intents,
                STALL_WINDOW,
            )
            .map(|outcome| outcome.map(drop))
        })
    }

    /// The one path of every journaled native ref write: `push` begins the write's intent,
    /// sends it and settles it, in this process's turn ([`TURNS`]).
    ///
    /// A refusal that names an open intent and sent nothing (agh's `intent_open`) is not the
    /// end of the write: [`Self::after_intent`] waits for that intent to close, within
    /// [`INTENT_WAIT`] over the whole call, and `push` runs again, where the native push
    /// reads the refs as they then are. Behind an intent whose writer is gone, where that push
    /// reads the intent's batch from origin first, each refusal after the first is paced and
    /// counted ([`RECONCILE_PAUSE`], [`RECONCILE_RETRIES`]). An intent that is an earlier
    /// attempt at this same write (the same `request`) is never posted again while origin
    /// holds what it asked for: that is agh's own rule for an uncertain push (`git::run`: only
    /// a complete read of every requested ref at its new object establishes the write;
    /// anything else leaves the intent open), recorded as agh records a discovery
    /// (`IntentStore::observe`). This write's own lost answer is settled the same way before
    /// the call returns. Every other refusal is returned as it came.
    fn journaled(
        &self,
        asked: &Asked<'_>,
        push: &dyn Fn(&agh::Journal) -> agh::Result<agh::Outcome<()>>,
    ) -> Result<agh::Result<()>, String> {
        let intents = self
            .client
            .journal(&self.slug)
            .map_err(|error| self.native_write_error(&error))?;
        let turn = turn(intents.path());
        let mut since = None;
        let mut reconciling = Reconciling::default();
        loop {
            let outcome = {
                let _turn = turn.lock().unwrap_or_else(PoisonError::into_inner);
                push(&intents)
            };
            let error = match outcome {
                Ok(agh::Outcome::Unknown { intent, error }) => {
                    let unknown = agh::Outcome::<()>::Unknown {
                        intent: intent.clone(),
                        error,
                    }
                    .into_result();
                    let Err(unknown) = unknown else {
                        return Ok(Ok(()));
                    };
                    let held = self.holds(&asked.wanted).map_err(|why| {
                        format!(
                            "{}; origin could not be read to settle it: {why}",
                            self.native_write_error(&unknown)
                        )
                    })?;
                    if !held {
                        return Ok(Err(unknown));
                    }
                    self.found(&intents, &intent)?;
                    (self.notice)(&format!(
                        "intent {intent}: this write's answer was lost and origin holds every \
                         ref it asked for; it is recorded found"
                    ));
                    return Ok(Ok(()));
                }
                Ok(outcome) => return Ok(outcome.into_result()),
                Err(error) => error,
            };
            let Some(intent) = blocking_intent(&error) else {
                return Ok(Err(error));
            };
            let began = match since {
                Some(began) => began,
                None => *since.insert(self.mono()?),
            };
            match self.after_intent(&intents, &intent, &error, asked, began, &mut reconciling)? {
                After::Again => {}
                After::Made => return Ok(Ok(())),
            }
        }
    }

    /// Look at the open intent that refused a write (`error`, which sent nothing), and wait
    /// for it to close: until `began` + [`INTENT_WAIT`], saying one line, and for as long as
    /// `stop` allows and no signal interrupted the release. Behind an intent whose writer is
    /// gone, the first refusal makes the write again at once (the push it came from may have
    /// found that writer alive, or not read the intent from origin), and each later one after a
    /// pause ([`Self::pace`]), counted in `reconciling` across the call's passes.
    ///
    /// # Errors
    /// The original refusal, with how long the write waited, once the bound passes with the
    /// intent still open: its words name the intent and `agh intent show`. A journal or
    /// origin that could not be read, a stopped or interrupted wait, and a gone writer's intent
    /// that origin settled neither way through [`RECONCILE_RETRIES`] pauses refuse the same way.
    fn after_intent(
        &self,
        intents: &agh::Journal,
        intent: &agh::IntentId,
        error: &agh::Error,
        asked: &Asked<'_>,
        began: Duration,
        reconciling: &mut Reconciling,
    ) -> Result<After, String> {
        let refused = |why: &str| format!("{}; {why}", self.native_write_error(error));
        let view = |what: &str| match intents.view(intent) {
            Ok(Some(view)) => Ok(view),
            Ok(None) => Err(refused(&format!("the journal holds no intent {intent}"))),
            Err(read) => Err(refused(&format!(
                "the journal could not be read {what}: {}",
                self.client.redactor().apply(&read.why)
            ))),
        };
        let mut seen = view("for it")?;
        // An earlier attempt at this same write: the same repository, refs, expected and new
        // objects. An exact origin observation can establish success without
        // posting it again. Otherwise a live writer stays held; a dead writer
        // permits the native boundary to reconcile its recorded batch.
        let own = seen.intent.op == agh::write::WriteOp::RefUpdateAtomic
            && seen.intent.request == asked.request;
        let made = |what: &str| {
            (self.notice)(&format!(
                "intent {intent} is an earlier attempt at this write {what}: origin holds every \
                 ref it asked for, and nothing is sent again"
            ));
            Ok(After::Made)
        };
        let holds = || self.holds(&asked.wanted).map_err(|why| refused(&why));
        // A dead writer lets the next atomic write read its exact batch from origin
        // (`agh::Journal::closable`): the wait on it ends, and the write is made again.
        let stops = |seen: &agh::journal::IntentView| {
            intents
                .closable(seen)
                .map(|closable| seen.open && !closable)
                .map_err(|clock| refused(&clock.why))
        };
        // A stopped monitor, or a release a signal interrupted, starts no further remote
        // operation.
        let halted = || match (self.interrupted)() {
            Some(signal) => Err(refused(&format!(
                "stopped by signal {signal} while waiting for intent {intent} to close"
            ))),
            None if (asked.stop)() => Err(refused(&format!(
                "stopped while waiting for intent {intent} to close"
            ))),
            None => Ok(()),
        };
        if seen.open && own && holds()? {
            self.found(intents, intent)?;
            return made("that was left open");
        }
        if stops(&seen)? {
            let line = if own {
                format!("waiting for intent {intent} of an earlier attempt at this write")
            } else {
                format!("waiting for intent {intent} of another writer on this Mac")
            };
            let _waiting = Waiting::new(&line);
            (self.notice)(&line);
            while stops(&seen)? {
                let waited = self.mono()?.saturating_sub(began);
                if waited >= INTENT_WAIT {
                    // A delayed receive of the earlier attempt may have landed meanwhile.
                    if own && holds()? {
                        self.found(intents, intent)?;
                        return made("that was left open");
                    }
                    return Err(refused(&format!(
                        "waited {} s for intent {intent} to close and it is still open",
                        waited.as_secs()
                    )));
                }
                halted()?;
                self.client
                    .fx()
                    .sleep(INTENT_POLL)
                    .map_err(|clock| refused(&clock.why))?;
                seen = view("while waiting")?;
            }
            halted()?;
            if seen.open {
                // The wait ended on its writer's death: the push made next reads the intent
                // from origin, so a refusal after it is paced.
                *reconciling = Reconciling::of(intent);
            }
        } else {
            let waited = self.mono()?.saturating_sub(began);
            if waited >= INTENT_WAIT {
                return Err(refused(&format!(
                    "other writers' intents kept this write from beginning for {} s",
                    INTENT_WAIT.as_secs()
                )));
            }
            halted()?;
            // Open and not waited on: closable, so its writer is gone.
            if seen.open {
                if reconciling.intent.as_ref() == Some(intent) {
                    self.pace(
                        intent,
                        reconciling,
                        INTENT_WAIT.saturating_sub(waited),
                        &refused,
                        &halted,
                    )?;
                    seen = view("after a pause")?;
                } else {
                    *reconciling = Reconciling::of(intent);
                }
            }
        }
        // Whether the intent closed (its writer settled it, a read found it, or another write's
        // origin read retracted or found it once its writer was gone) or its writer is gone and
        // the next write reads it from origin, what origin holds decides whether this write
        // landed.
        if own && holds()? {
            if seen.open {
                self.found(intents, intent)?;
            }
            return made("that its writer finished");
        }
        // The last look before the write is made again: a stop or a signal that came while the
        // intent closed, or while origin was read above, starts no further push.
        halted()?;
        Ok(After::Again)
    }

    /// Pause before the write is made again behind `intent`, whose writer is gone and which a
    /// push made since this write found that writer gone refused again: [`RECONCILE_PAUSE`],
    /// doubled for each pause `reconciling` counts, within `room` (what is left of
    /// [`INTENT_WAIT`]), in [`INTENT_POLL`] slices with a look at `halted` before each and after
    /// the last, saying one line that the lease heartbeat reads as this thread's evidence
    /// ([`intent_wait`]).
    ///
    /// # Errors
    /// `refused`'s words, naming the intent and `agh intent show`, once [`RECONCILE_RETRIES`]
    /// pauses were taken; `halted`'s; and a clock that could not sleep.
    fn pace(
        &self,
        intent: &agh::IntentId,
        reconciling: &mut Reconciling,
        room: Duration,
        refused: &dyn Fn(&str) -> String,
        halted: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if reconciling.repeats >= RECONCILE_RETRIES {
            return Err(refused(&format!(
                "intent {intent}'s writer is gone, and the {} pushes made since each found its \
                 refs on origin neither wholly as before nor wholly as after its write, or could \
                 not read them; nothing was sent; `agh intent show {} {intent}` shows its batch, \
                 and once origin shows what that write did, `agh intent retract` closes it",
                RECONCILE_RETRIES + 1,
                self.slug
            )));
        }
        let pause = RECONCILE_PAUSE
            .saturating_mul(1 << reconciling.repeats)
            .min(room);
        reconciling.repeats += 1;
        let line = format!(
            "intent {intent}'s writer is gone and origin did not settle it; this write sent \
             nothing and is made again in {} s ({} of {RECONCILE_RETRIES})",
            pause.as_secs_f64(),
            reconciling.repeats
        );
        let _waiting = Waiting::new(&line);
        (self.notice)(&line);
        let mut left = pause;
        while !left.is_zero() {
            halted()?;
            let slice = left.min(INTENT_POLL);
            self.client
                .fx()
                .sleep(slice)
                .map_err(|clock| refused(&clock.why))?;
            left = left.saturating_sub(slice);
        }
        halted()
    }

    /// Whether origin now holds every ref as the write asked: one complete read of each.
    fn holds(&self, wanted: &Wanted) -> Result<bool, String> {
        for (reference, after) in wanted {
            if self.get(reference)? != *after {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Record that a read of origin found what `intent` asked for, which closes it.
    fn found(&self, intents: &agh::Journal, intent: &agh::IntentId) -> Result<(), String> {
        intents.observe(intent, Some(0)).map_err(|error| {
            format!(
                "origin holds what intent {intent} asked for, and the journal could not record \
                 it: {}",
                self.native_write_error(&error)
            )
        })
    }

    fn mono(&self) -> Result<Duration, String> {
        self.client
            .fx()
            .mono()
            .map_err(|error| self.native_write_error(&error))
    }

    /// Delete one exact owned ref. A grouped delete needs its own atomic native
    /// operation and must never be decomposed into calls to this method.
    pub(crate) fn delete_ref(
        &self,
        reference: &str,
        expected_oid: &str,
    ) -> Result<RefWrite, String> {
        self.run(GitRun::check_local(&self.slug, &self.repo))?;
        if origin_slug(&self.repo)? != self.slug {
            return Err("origin changed before the native ref delete".into());
        }
        let run = GitRun::push_delete(&self.slug, reference, expected_oid)
            .with_named(self.named.clone())
            .with_progress_window(STALL_WINDOW);
        // Ordinary exact deletes converge on absence. Mutable control refs use
        // the separate journaled control transaction, including grouped unlock.
        self.ref_result(self.client.run_git(&run))
    }

    fn ref_result<T>(&self, result: agh::Result<T>) -> Result<RefWrite, String> {
        match result {
            Ok(_) => Ok(RefWrite::Written),
            Err(error) if is_conflict(&error) => {
                eprintln!("{}", self.native_write_error(&error));
                Ok(RefWrite::Conflict)
            }
            Err(error) => Err(self.native_write_error(&error)),
        }
    }

    fn native_write_error(&self, error: &agh::Error) -> String {
        self.client.redactor().apply(&format!(
            "native ref write: {} ({}; side_effect={}); {}",
            error.why,
            error.kind.slug(),
            error.side_effect.slug(),
            error.next
        ))
    }
}

fn is_conflict(error: &agh::Error) -> bool {
    error.side_effect == agh::error::SideEffect::None && error.kind == agh::error::Kind::Conflict
}

/// The open intent a refused write names, where the refusal sent nothing (agh's
/// `IntentStore::begin`: another write's intent stands in the bucket, or began since this
/// write looked).
fn blocking_intent(error: &agh::Error) -> Option<agh::IntentId> {
    let agh::Sub::IntentOpen { intent } = &error.sub else {
        return None;
    };
    if error.kind != agh::error::Kind::OutcomeUnknown
        || error.side_effect != agh::error::SideEffect::None
    {
        return None;
    }
    agh::IntentId::parse(intent)
}

fn is_transient(error: &agh::Error) -> bool {
    error.kind == agh::error::Kind::CouldNotRun && error.side_effect == agh::error::SideEffect::None
}

/// A scratch-only writer route for actual native local-bare claim controls.
#[cfg(test)]
pub(crate) fn engine_fixture(
    repo: &std::path::Path,
    source: &std::path::Path,
) -> Result<Remote, String> {
    let git = crate::ledger::GitCli::new(source);
    let recorded = crate::ledger::git_ok(&git, &["config", "--get", "remote.fixture.url"])
        .or_else(|_| crate::ledger::git_ok(&git, &["remote", "get-url", "origin"]))
        .map_err(|e| e.to_string())?
        .stdout_utf8();
    let bare = std::path::Path::new(recorded.trim());
    if !bare.is_absolute() || !bare.is_dir() {
        return Err("engine fixture needs its explicit owned bare repository".into());
    }
    if repo != source {
        let git = crate::ledger::GitCli::new(repo);
        for (key, value) in [
            ("remote.fixture.url", recorded.trim()),
            ("remote.origin.url", "https://github.com/fixture/app.git"),
        ] {
            crate::ledger::git_ok(&git, &["config", key, value]).map_err(|e| e.to_string())?;
        }
    }
    Ok(claim_fixture(repo, bare))
}

#[cfg(test)]
pub(crate) fn claim_fixture(repo: &std::path::Path, bare: &std::path::Path) -> Remote {
    claim_fixture_with_fx(
        repo,
        bare,
        std::sync::Arc::new(agh::Fx::simulated(1_760_000_000)),
    )
}

#[cfg(test)]
pub(crate) fn claim_fixture_with_fx(
    repo: &std::path::Path,
    bare: &std::path::Path,
    fx: std::sync::Arc<agh::Fx>,
) -> Remote {
    // Libtest does not enter the cutter's native lifecycle dispatch. Use the
    // real dev binary to hold each writer before exec while its identity is
    // journaled; the independent credential helper below still refuses.
    #[cfg(unix)]
    aterm_uds::tether::through(aterm_tempfile::bin_exe("aterm-release"));
    let mut command = crate::ledger::object_git(repo).unwrap();
    command.args(["rev-parse", "--absolute-git-dir"]);
    let out = aterm_uds::deadline::output_while_process_progressing_complete_stdout(
        &mut command,
        STALL_WINDOW,
        "git rev-parse absolute metadata directory",
    )
    .unwrap();
    assert!(
        out.status.success(),
        "fixture needs a Git metadata directory"
    );
    let git_dir = std::path::PathBuf::from(std::str::from_utf8(&out.stdout).unwrap().trim());
    assert!(git_dir.is_absolute());
    let state = git_dir.join("claim-fixture");
    aterm_uds::private_file::create_private_dir(&state).unwrap();
    let dirs = agh::config::Dirs::under(&state);
    aterm_uds::private_file::create_private_dir(&dirs.home).unwrap();
    std::fs::create_dir_all(dirs.config.parent().unwrap()).unwrap();
    aterm_uds::private_file::create_private_dir(&dirs.config).unwrap();
    aterm_uds::private_file::write_private(
        &dirs.home,
        "token",
        b"fixture-claim-not-a-real-credential\n",
    )
    .unwrap();
    let config = r#"
        [profiles.fixture]
        token_file = "~/token"
        tier = "dev"
        login = "fixture"
        [[owners]]
        name = "fixture"
        tier = "dev"
        read = ["fixture"]
        write = "fixture"
        [default]
        read = ["fixture"]
        "#;
    aterm_uds::private_file::write_private(
        &dirs.config,
        dirs.config_file().file_name().unwrap().to_str().unwrap(),
        config.as_bytes(),
    )
    .unwrap();
    let router = agh::policy::Router::from_validated(agh::config::load(&dirs).unwrap().unwrap());
    let env = agh::identity::MapEnv::new().with("PATH", &std::env::var("PATH").unwrap());
    let transport = std::sync::Arc::new(agh::wire::scripted::ScriptedTransport::new());
    // Only repository/identity guard reads use this HTTP fixture. The ref wire
    // remains the real local-bare native transport, not a scripted write.
    let metadata = br#"{"id":1,"login":"fixture","name":"app","full_name":"fixture/app","owner":{"login":"fixture","id":1},"private":true,"permissions":{"push":true}}"#;
    for _ in 0..128 {
        transport.answer(200, &[], metadata);
    }
    #[cfg(unix)]
    let helper = agh::git::HelperCommand {
        exe: "/usr/bin/false".into(),
        args: Vec::new(),
    };
    #[cfg(windows)]
    let helper = agh::git::HelperCommand {
        exe: std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/cmd.exe"),
        args: ["/D", "/C", "exit", "1"]
            .into_iter()
            .map(Into::into)
            .collect(),
    };
    let client = agh::GithubBuilder::new(router)
        .env(std::sync::Arc::new(env))
        .transport(transport)
        .fx(fx)
        .journal(dirs)
        // Local-bare transport exchanges no credentials. Any accidental helper
        // invocation must fail rather than run this integration test as a CLI.
        .git_helper_for_tests(helper)
        .git_remote_for_tests("fixture/app", bare)
        .build();
    Remote::new(client, agh::Slug::parse("fixture/app").unwrap(), repo).unwrap()
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
