// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native exec account preparation. Peer/claim proof and kernel launch facts
//! replace the TUI record requirement; account policy remains the shared pool's.
//! A selected proposal is not a commit, launcher capability or execution grant.
//! A tab keeps one preparation and one operation for each process that asks
//! ([`ProcessKey`]), so no run's handoff takes another run's place. Its reads
//! run under the asking process's read pace, the one `codex-rotation` keeps for
//! that process too ([`renewal::observe_paced`]), to which every request of the
//! lane is noted: `prepare`, `arm` and each verb of the operation it armed.

use super::*;
use aterm_agent::harness::{
    accounts,
    cli::Env,
    codex_exec_handoff::{ExecCommand, ExecSource},
    codex_renewal, codex_rotation,
};
use renewal::ProcessKey;
use std::collections::BTreeSet;
use std::time::Instant;

#[path = "codex_exec_operation.rs"]
mod operation;
use kept::{Kept, arm_selection};
use operation::Operation;
pub(super) use operation::{cancel_owner, pending, retain_claim};
pub(super) use table::Table;

const WAITING: &str = "OK codex-exec-handoff state=waiting\n";
const NOT_NEEDED: &str = "OK codex-exec-handoff state=not-needed\n";

/// How old a selection `arm` still takes, from when its read came back: past it the usage that
/// read saw is too old to move a session on.
const SELECTION_ARMS: Duration = Duration::from_secs(60);

/// How old a selection `prepare` still offers: ten seconds short of [`SELECTION_ARMS`], so a
/// session offered one has that long to bring it back as its `arm`. A `reserve` whose writer said
/// nothing of a selection no `reserve`'s writer has held waits only this long from that
/// selection's read, too ([`after_writer`]).
const SELECTION_OFFERED: Duration = Duration::from_secs(50);

/// How long an answer that the session's login stands at no wall a handoff leaves
/// ([`codex_rotation::NoHandoff::NoWallToLeave`]) answers its later asks. A headless run is one
/// root turn that keeps its request in flight, so its job here is the same one at every wall it
/// meets, and an answer that stood for good would be given at a later wall, the week's included,
/// with no read. The latest reset that answer waited out is a five-hour one
/// [`accounts::RESET_SOON_S`] away; by then it has passed and so has its grace
/// ([`accounts::RESET_GRACE_S`]), so a session still asking stands at a wall again, or still, and
/// its login is read again. A wait that is read as no wall again stands longer each time
/// ([`no_wall_stands`]). The clock is the monotonic one, which a sleeping host may hold still: a
/// sleep only lengthens the stand.
const NO_WALL_STANDS: Duration =
    Duration::from_secs((accounts::RESET_SOON_S + accounts::RESET_GRACE_S).unsigned_abs());

/// The longest time between two asks of one wait. A session asks every few seconds while it
/// stands at its wall, so one that asks again after longer than this had stopped asking: it ran,
/// and stands at another wall, whose wait takes up none of the stands the earlier one climbed to.
const STILL_ASKING: Duration = Duration::from_secs(60);

/// How long an answer of no wall to leave stands when it is the `reads`th such read in a row of
/// one wait: [`NO_WALL_STANDS`] for the first, and for one whose session has since asked anew,
/// then the rungs a wall that names no reset stands by ([`accounts::NO_RESET_WALL_S`]). The
/// first stand ends past a reset a quarter of an hour away. A login read as no wall again while
/// its session kept asking stands at something no such reset ends (a spent window that names no
/// reset, a limit the read does not show), so its reads thin out as the pool's do for a wall
/// with no reset, to one in six hours, where a flat stand would read it every
/// [`NO_WALL_STANDS`] for as long as the run asks.
fn no_wall_stands(reads: u32) -> Duration {
    match reads.checked_sub(1) {
        None | Some(0) => NO_WALL_STANDS,
        Some(again) => {
            Duration::from_secs(accounts::rung(&accounts::NO_RESET_WALL_S, again).unsigned_abs())
        }
    }
}

#[derive(Debug)]
struct Preparation {
    source: ExecSource,
    intent: codex_renewal::Intent,
    proof: crate::self_claim::Proven,
    revision: u64,
    human_seq: u64,
    /// The session the shared pool knows this process's move by: the owner of a canary its read
    /// takes, and the `sid` of the receipt its `arm` writes, which names that owner when the move
    /// is struck or rolled back. One name for both. A process that holds its tab's claim moves
    /// under the tab's.
    mover: String,
    progress: Mutex<Progress>,
}

#[derive(Debug)]
enum Progress {
    /// A read is out, started at the time this holds. Before it, this many reads in a row of the
    /// wait it was started in said there is no wall to leave.
    Reading(u32, Instant),
    Retry(Instant),
    NotNeeded(NoneComing),
    Selected(Box<codex_rotation::Proposal<ExecCommand>>, Instant),
}

/// The answer that no handoff is coming, as its job keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NoneComing {
    /// Why none is.
    end: codex_rotation::NoHandoff,
    /// When its read said so.
    decided: Instant,
    /// How many reads in a row of the session's wait, this one included, said there is no wall
    /// to leave; none where the session has since stopped asking and asked again.
    reads: u32,
    /// The session's latest ask, or `decided` before one.
    asked: Instant,
}

impl NoneComing {
    /// Take an ask at `now`, and say whether this answer still answers it. No wall to leave, and
    /// no entry for the limit asked about, are each a reading of the login at one time, and
    /// stand [`no_wall_stands`]; a login that is no row of the pool, or that names another
    /// account than its row's, is nothing a wait changes, and stands while the job does.
    fn stands(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.asked) > STILL_ASKING {
            self.reads = 0;
        }
        self.asked = now;
        match self.end {
            codex_rotation::NoHandoff::NoWallToLeave | codex_rotation::NoHandoff::LimitAbsent => {
                now.saturating_duration_since(self.decided) < no_wall_stands(self.reads)
            }
            codex_rotation::NoHandoff::NoRowOfThePool
            | codex_rotation::NoHandoff::LoginNamesAnother => true,
        }
    }
}

/// A step of a handoff that asks the selection's writer
/// ([`codex_rotation::with_prepared_writer_now`]) before it goes on.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// `arm`, which asked the writer to create the operation's receipt: only a created one arms.
    Arm,
    /// `reserve` of an armed operation whose pool commit was refused; the operation lapses at
    /// `deadline_us`, and what it keeps of its selection is `armed`.
    Reserve { deadline_us: u64, armed: Armed },
}

/// What an armed operation keeps of its selection ([`Kept`]), as its `reserve`s judge a writer's
/// silence by it ([`after_writer`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Armed {
    /// When the read that selected its account came back.
    read: Instant,
    /// Whether the writer of one of its `reserve`s has held the selection (`Ok(Some(_))`). The
    /// `arm` that made the operation is not counted: every operation's writer held its selection
    /// once, at its `arm`.
    held: bool,
}

/// Whether `why`, a refusal of [`codex_rotation::with_prepared_writer_now`], says the selection it
/// judged no longer holds, so that no later `arm` of it can go through and only a new read of the
/// logins selects again:
/// - `not-ready`: the receipt is no ready one, is revoked, or carries evidence of an execution;
/// - `rotation-policy`, `policy-changed`: aterm.toml forbids a handoff, or it or either row's
///   source documents changed while the writer judged;
/// - `pool-commit-changed`: the pool's record of this receipt binds another move;
/// - `account-lineage`: the roster no longer holds one row for each of the two logins;
/// - `selection-changed`: the two rows are one, either is held for another Mac, or the target's
///   row or login generation is not the one selected;
/// - `target-unavailable`: the pool is pinned, or the target is no longer one a session moves to;
/// - `source-held`, `source-conflict`, `same-login`, `identity-mismatch`: the custody of the two
///   logins refuses them (a row held for another Mac, a fleet roster that names a row otherwise,
///   one login file for both, a login that is no file store's or names another identity than the
///   selection's).
///
/// Every other refusal says nothing of the selection: a lock another writer holds (`signin-busy`,
/// `signin-lock`, `auth-busy`, `auth-lock`, `receipt-lock`), a file that did not read
/// (`roster-unreadable`, `bindings-unreadable`, `state-unreadable`, `source-unreadable`), a
/// receipt that could not be written, or a word this list does not know. Keeping a selection a
/// word really refuted, or one a word that stands (a missing home) keeps refusing, costs no read:
/// each `arm` of it is denied until it is too old to arm ([`SELECTION_ARMS`]), and each `reserve`
/// waits until its read is [`SELECTION_OFFERED`] old, as long as it is offered, and is denied
/// after, unless a `reserve`'s writer has held it ([`Armed::held`]): such a one waits to its
/// operation's deadline. Each of those steps asks the writer once, which tries the sign-in turn
/// and `accounts.lock` once each, so a `reserve` retried every 250 ms asks it at most 200 times,
/// or 480 over its operation's two minutes once a writer has held its selection. Dropping a
/// selection a word did not refute costs a read of the session's login and of each candidate's.
fn refutes(why: &str) -> bool {
    matches!(
        why,
        "not-ready"
            | "rotation-policy"
            | "policy-changed"
            | "pool-commit-changed"
            | "account-lineage"
            | "selection-changed"
            | "target-unavailable"
            | "source-held"
            | "source-conflict"
            | "same-login"
            | "identity-mismatch"
    )
}

/// What a step is answered once the selection's writer said `outcome`, or `None` where the step
/// goes on, and what becomes of the selection in `progress`. The one place a selection is dropped
/// for what a writer said, and the one answer `arm` and `reserve` both return through.
///
/// - `Ok(Some(_))`, the writer holds the selection: an `arm` goes on with the receipt it created,
///   and a `reserve` waits, since a healthy destination may still be another session's canary.
/// - A refusal that refutes the selection ([`refutes`]): the step is denied and, where the
///   selection in `progress` is the one the writer judged (`ours`), it is dropped at `now`, so
///   the next ask reads the logins again. A progress that has moved on is left as it is.
/// - Anything else (`Ok(None)`, the store's writer taken, and every other refusal): the selection
///   is kept until it is too old. An `arm` is denied, and the next one takes the same selection
///   with no read. A `reserve` waits, so its session asks again inside its grant, while the read
///   behind its selection is younger than [`SELECTION_OFFERED`], as an offer of it stands; past
///   that it is denied, so a refusal that keeps standing ends its retries there, not at the
///   operation's deadline. A `reserve` of a selection an earlier `reserve`'s writer held
///   ([`Armed::held`]) waits at any age, as one whose writer holds it now does: a lock another
///   writer holds a moment later says nothing against what that writer judged.
fn after_writer<T>(
    progress: &mut Progress,
    ours: bool,
    step: Step,
    outcome: &Result<Option<T>, &'static str>,
    now: Instant,
) -> Option<String> {
    let refuted = matches!(outcome, Err(why) if refutes(why));
    if refuted && ours && matches!(progress, Progress::Selected(_, _)) {
        *progress = Progress::Retry(now);
    }
    let waits = |deadline_us: u64| {
        Some(format!(
            "OK codex-exec-handoff state=waiting deadline_us={deadline_us}\n"
        ))
    };
    match (step, outcome) {
        (Step::Arm, Ok(Some(_))) => None,
        (Step::Reserve { deadline_us, .. }, Ok(Some(_))) => waits(deadline_us),
        (Step::Reserve { deadline_us, armed }, _)
            if !refuted
                && (armed.held
                    || now.saturating_duration_since(armed.read) < SELECTION_OFFERED) =>
        {
            waits(deadline_us)
        }
        _ => Some(DENIED.into()),
    }
}

/// The selection in `progress` that an `arm` naming the destination `account` and `user` takes
/// at `now`, with when its read came back: one of that destination, read less than
/// [`SELECTION_ARMS`] ago.
fn selection_to_arm<'a>(
    progress: &'a Progress,
    account: &str,
    user: &str,
    now: Instant,
) -> Option<(&'a codex_rotation::Proposal<ExecCommand>, Instant)> {
    match progress {
        Progress::Selected(proposal, observed)
            if now.saturating_duration_since(*observed) < SELECTION_ARMS
                && proposal.destination_identity.account == account
                && proposal.destination_identity.user == user =>
        {
            Some((proposal, *observed))
        }
        _ => None,
    }
}

/// What an armed operation keeps of its selection, kept where neither `arm` nor `serve` can reach
/// into it: the one way to make it is the selection an `arm` takes, and the one way to read it is a
/// refused `reserve`'s answer, so no caller stores or passes another read than the one taken.
mod kept {
    use super::*;

    /// An operation's own [`Armed`]: made only by [`arm_selection`], of the selection it takes, and
    /// read only by [`Kept::reserve_refused`].
    #[derive(Debug)]
    pub(super) struct Kept(Armed);

    /// The selection an `arm` naming the destination `account` and `user` takes now from a job's
    /// `progress` ([`selection_to_arm`]), and what the operation it makes keeps of it: when its
    /// read came back, held by no `reserve`'s writer yet. The clock is read here, not by the
    /// caller, so what `arm` takes is what this answers.
    pub(super) fn arm_selection(
        progress: &Mutex<Progress>,
        account: &str,
        user: &str,
    ) -> Option<(codex_rotation::Proposal<ExecCommand>, Kept)> {
        let progress = progress.lock().unwrap_or_else(|p| p.into_inner());
        selection_to_arm(&progress, account, user, Instant::now())
            .map(|(proposal, read)| (proposal.clone(), Kept(Armed { read, held: false })))
    }

    impl Kept {
        /// What a `reserve` of the armed operation that keeps this, whose pool commit was refused,
        /// answers: the writer's word on the operation's `receipt`, through `judge`
        /// ([`writer_answered`]), of the step this selection and `deadline_us` make, in its job's
        /// `progress` for its `intent`. A writer that holds the selection is noted here for the
        /// operation's later `reserve`s. Such a `reserve` never goes on: every answer is a reply.
        pub(super) fn reserve_refused(
            &mut self,
            progress: &Mutex<Progress>,
            intent: &codex_renewal::Intent,
            deadline_us: u64,
            receipt: &codex_renewal::Receipt,
            judge: impl FnOnce(&codex_renewal::Receipt) -> Result<Option<()>, &'static str>,
        ) -> String {
            let step = Step::Reserve {
                deadline_us,
                armed: self.0,
            };
            writer_answered(progress, intent, step, receipt, |receipt| {
                let outcome = judge(receipt);
                self.0.held |= matches!(outcome, Ok(Some(())));
                outcome
            })
            .unwrap_or_else(|| DENIED.into())
        }

        /// What this keeps now, for a test to see.
        #[cfg(test)]
        pub(super) fn armed(&self) -> Armed {
            self.0
        }
    }
}

/// Ask the selection's writer, through `judge`, of the `receipt` made of a job's selection, and
/// answer `step` by what it said ([`after_writer`], now), in that job's `progress` for its
/// `intent`. The selection in hand is the one judged where it is of the receipt's intent and
/// names its successor. `judge` runs with the progress unlocked. The one way `arm` and `reserve`
/// ask the writer: each passes its writer and decides nothing of what it says.
fn writer_answered<T>(
    progress: &Mutex<Progress>,
    intent: &codex_renewal::Intent,
    step: Step,
    receipt: &codex_renewal::Receipt,
    judge: impl FnOnce(&codex_renewal::Receipt) -> Result<Option<T>, &'static str>,
) -> Option<String> {
    let outcome = judge(receipt);
    let mut progress = progress.lock().unwrap_or_else(|p| p.into_inner());
    let ours = *intent == receipt.intent
        && matches!(&*progress, Progress::Selected(proposal, _)
            if proposal.successor().is_some_and(|next| receipt.matches_successor(&next)));
    after_writer(&mut progress, ours, step, &outcome, Instant::now())
}

impl Preparation {
    fn matches(
        &self,
        source: &ExecSource,
        intent: &codex_renewal::Intent,
        proof: &crate::self_claim::Proven,
        revision: u64,
        human_seq: u64,
    ) -> bool {
        self.source == *source
            && self.intent == *intent
            && self.proof == *proof
            && self.revision == revision
            && self.human_seq == human_seq
    }
}

/// A tab's exec handoffs, kept where no step of one process can reach another's: one
/// preparation and one operation for each process that asks ([`ProcessKey`]). A preparation
/// enters only through [`Table::job_for`] and an operation only through [`Table::keep`], each in
/// the place of the process it was made for, and a step finds one only by its own process's key.
/// No `prepare`, `arm` or id/cap verb of one process reads, replaces or drops another's.
mod table {
    use super::*;

    #[derive(Debug, Default)]
    pub(in crate::native_act) struct Table {
        /// Each process's preparation.
        prepare: BTreeMap<ProcessKey, Arc<Preparation>>,
        /// Each process's operation, under the key of the source that armed it.
        operation: BTreeMap<ProcessKey, Arc<Mutex<Operation>>>,
    }

    impl Table {
        /// The job a `prepare` that asks `asked` answers from: the one its process keeps where
        /// that was made for the same ask ([`Preparation::matches`]), else `asked` itself, kept
        /// in that process's place.
        pub(super) fn job_for(&mut self, asked: Preparation) -> Arc<Preparation> {
            let key = ProcessKey::of(&asked.source.process);
            if let Some(job) = self.prepare.get(&key).filter(|job| {
                job.matches(
                    &asked.source,
                    &asked.intent,
                    &asked.proof,
                    asked.revision,
                    asked.human_seq,
                )
            }) {
                return Arc::clone(job);
            }
            let job = Arc::new(asked);
            self.prepare.insert(key, Arc::clone(&job));
            job
        }

        /// Whether `job` is still the one its process keeps: a read started for a job that a
        /// later ask of its process replaced, or that went with its process, selects nothing.
        pub(super) fn holds(&self, job: &Arc<Preparation>) -> bool {
            self.prepare
                .get(&ProcessKey::of(&job.source.process))
                .is_some_and(|kept| Arc::ptr_eq(kept, job))
        }

        /// The preparation `process` keeps, which its `arm` takes its selection from.
        pub(super) fn job(&self, process: ProcessKey) -> Option<&Arc<Preparation>> {
            self.prepare.get(&process)
        }

        /// The operation `process` armed: the one its own later `arm` and each id/cap verb of it,
        /// or of the successor its same-PID execve made, find.
        pub(super) fn operation(&self, process: ProcessKey) -> Option<&Arc<Mutex<Operation>>> {
            self.operation.get(&process)
        }

        /// Keep `operation` in the place of the process whose source armed it, where it takes
        /// the place of that process's earlier one, and give it back as the table keeps it.
        pub(super) fn keep(&mut self, operation: Operation) -> Arc<Mutex<Operation>> {
            let key = operation.process();
            let kept = Arc::new(Mutex::new(operation));
            self.operation.insert(key, Arc::clone(&kept));
            kept
        }

        /// Every operation the table keeps, for a step of its tab that judges them all.
        pub(super) fn operations(&self) -> Vec<Arc<Mutex<Operation>>> {
            self.operation.values().cloned().collect()
        }

        /// Every process the table keeps a preparation or an operation for.
        pub(super) fn processes(&self) -> BTreeSet<ProcessKey> {
            self.prepare
                .keys()
                .chain(self.operation.keys())
                .copied()
                .collect()
        }

        /// Forget the preparation of each process in `gone`, and its operation once that holds
        /// nothing ([`operation::idle`]): one still in custody stays for its monitor to settle
        /// and a Stop to reach.
        pub(super) fn forget(&mut self, gone: &BTreeSet<ProcessKey>) {
            self.prepare.retain(|key, _| !gone.contains(key));
            self.operation
                .retain(|key, op| !gone.contains(key) || !operation::idle(op));
        }
    }
}

/// Forget, from a tab's `native` table, what it keeps of each process the kernel shows gone
/// ([`ProcessKey::gone`], [`Table::forget`]). The kernel is read with the table unlocked; a gone
/// process does not come back, so what it showed still holds once the table is taken again.
fn forget_gone(native: &Mutex<Operations>) {
    let kept = native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .exec
        .processes();
    let gone = kept
        .into_iter()
        .filter(|key| key.gone())
        .collect::<BTreeSet<_>>();
    if gone.is_empty() {
        return;
    }
    native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .exec
        .forget(&gone);
}

/// What an ask at `now` is answered from its job's progress, and whether it starts a read of the
/// session's login, which [`ask`] does once the progress is unlocked. A read that is out answers
/// `waiting` and starts none, until it has been out for longer than [`renewal::READ_LAPSES`]: it
/// is then taken for one that will not end, and the ask starts another in its place, of the same
/// wait.
fn answer(progress: &mut Progress, intent: &codex_renewal::Intent, now: Instant) -> (String, bool) {
    match progress {
        Progress::Reading(_, since)
            if now.saturating_duration_since(*since) <= renewal::READ_LAPSES =>
        {
            (WAITING.into(), false)
        }
        Progress::Reading(earlier, _) => {
            aterm_log::warn!(
                "codex exec handoff: a read has been out for longer than {:?}; it is taken for \
                 one that will not end, and another starts",
                renewal::READ_LAPSES
            );
            *progress = Progress::Reading(*earlier, now);
            (WAITING.into(), true)
        }
        Progress::Retry(at) if now < *at => (WAITING.into(), false),
        Progress::Retry(_) => {
            *progress = Progress::Reading(0, now);
            (WAITING.into(), true)
        }
        Progress::NotNeeded(none) => {
            if none.stands(now) {
                return (NOT_NEEDED.into(), false);
            }
            *progress = Progress::Reading(none.reads, now);
            (WAITING.into(), true)
        }
        Progress::Selected(proposal, observed)
            if now.saturating_duration_since(*observed) < SELECTION_OFFERED =>
        {
            (
                format!(
                    "OK codex-exec-handoff state=selected thread={} turn={} source_account={} source_user={} target_account={} target_user={}\n",
                    intent.thread,
                    intent.wall,
                    proposal.source_identity.account,
                    proposal.source_identity.user,
                    proposal.destination_identity.account,
                    proposal.destination_identity.user
                ),
                false,
            )
        }
        Progress::Selected(_, _) => {
            *progress = Progress::Retry(now);
            (WAITING.into(), false)
        }
    }
}

/// Answer an ask at `now` from its job's progress and, where that answer starts a read of the
/// session's login, run `read`, once and with the progress unlocked, with `now`, the time the
/// read is out from, by which its answer is written ([`observed`]): a read that cannot start
/// writes the progress itself.
fn ask(
    progress: &Mutex<Progress>,
    intent: &codex_renewal::Intent,
    now: Instant,
    read: impl FnOnce(Instant),
) -> String {
    let (reply, starts) = answer(
        &mut progress.lock().unwrap_or_else(|p| p.into_inner()),
        intent,
        now,
    );
    if starts {
        read(now);
    }
    reply
}

/// The job's `progress` once the read it started at `since` came back at `now` with `result`
/// ([`progress_after_observation`]): written only while that read is still the one out. A read
/// [`answer`] took for one that will not end and started another in place of changes nothing
/// should it end after all.
fn observed(
    progress: &mut Progress,
    since: Instant,
    result: Result<Option<codex_rotation::Proposal<ExecCommand>>, &'static str>,
    current: bool,
    now: Instant,
) {
    if matches!(progress, Progress::Reading(_, at) if *at == since) {
        *progress = progress_after_observation(progress, result, current, now);
    }
}

/// The job's progress once the read `before` was waiting on came back at `now`; `current` says
/// the session, its claim and the person's input are still the ones the read was started for.
fn progress_after_observation(
    before: &Progress,
    result: Result<Option<codex_rotation::Proposal<ExecCommand>>, &'static str>,
    current: bool,
    now: Instant,
) -> Progress {
    let retry = Progress::Retry(now + Duration::from_secs(5));
    match result {
        Ok(Some(proposal)) if current => Progress::Selected(Box::new(proposal), now),
        // No handoff is coming to this session, and a wait would only spend its turn. The log
        // says which kind ended it; [`NoneComing::stands`] says for how long.
        Err(why) if current => match codex_rotation::prepare_refusal_stands(why) {
            Some(end) => {
                aterm_log::info!("codex exec handoff: none is coming: {}", end.as_str());
                let earlier = match before {
                    Progress::Reading(earlier, _) => *earlier,
                    _ => 0,
                };
                Progress::NotNeeded(NoneComing {
                    end,
                    decided: now,
                    reads: earlier.saturating_add(1),
                    asked: now,
                })
            }
            None => retry,
        },
        _ => retry,
    }
}

pub(crate) fn serve(ctx: &Arc<crate::SessionCtx>, master: i32, line: &str) -> String {
    if line.split_ascii_whitespace().next() != Some("prepare") {
        return operation::serve(ctx, master, line);
    }
    let Some(codex_renewal::Request::Prepare(intent)) =
        codex_renewal::Request::parse_rotation(line)
    else {
        return "ERR codex-exec-handoff unsupported request\n".into();
    };
    if crate::control::serving_principal() == crate::control::Principal::Harness
        || !ctx.native_policy.quota_rotation_enabled()
    {
        return DENIED.into();
    }
    let Some(pid) = crate::control::serving_peer_pid() else {
        return DENIED.into();
    };
    let Some(proof) =
        crate::control::prove_self_claim_for(ctx, &format!("codex-alab@{pid}"), master)
    else {
        return DENIED.into();
    };
    let Some(source) = ExecSource::capture(pid) else {
        return DENIED.into();
    };
    let env = match Env::from_process() {
        Ok(env) => env,
        // This process's working directory and state directory do not come back by themselves:
        // no handoff is coming, and a wait would only spend the session's turn.
        Err(why) => {
            aterm_log::warn!("codex exec handoff: none is coming: {why}");
            return NOT_NEEDED.into();
        }
    };
    serve_captured(ctx, (proof, source, env), intent)
}

/// A `prepare` of `intent` by the claimed process `proof`, captured as `source`, served in `env`:
/// everything the lane decides once the process is captured. Every such ask is one of its
/// process's series, whatever it is answered ([`renewal::note_ask`]).
fn serve_captured(
    ctx: &Arc<crate::SessionCtx>,
    (proof, source, env): (crate::self_claim::Proven, ExecSource, Env),
    intent: codex_renewal::Intent,
) -> String {
    renewal::note_ask(ctx, ProcessKey::of(&source.process), env.now);
    if ctx.fabric.hold().is_some() || !renewal::human_grace_ready(ctx, &env) {
        return WAITING.into();
    }
    forget_gone(&ctx.fenced_writes.native);
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let authority = meta.supervisor_authority(crate::metrics::now_us());
    if authority.identity() != Some(&proof)
        || authority.connection() != crate::control::serving_connection()
        || authority
            .scopes()
            .is_some_and(|scopes| !scopes.contains(Scope::Relaunch))
    {
        return DENIED.into();
    }
    let asked = Preparation {
        source,
        intent,
        proof,
        revision: authority.revision(),
        human_seq: ctx.human_input.seq(),
        mover: ctx.self_id.as_str().to_owned(),
        progress: Mutex::new(Progress::Retry(Instant::now())),
    };
    let job = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .exec
        .job_for(asked);
    drop(meta);
    let reader = Arc::clone(&job);
    ask(&job.progress, &job.intent, Instant::now(), move |since| {
        start(ctx, env, reader, since);
    })
}

/// Start `job`'s read, the one its ask started at `since`.
fn start(ctx: &Arc<crate::SessionCtx>, env: Env, job: Arc<Preparation>, since: Instant) {
    let weak = Arc::downgrade(ctx);
    let roots = ctx.native_policy.quota_fixture_roots();
    let reader = Arc::clone(&job);
    let worker = Arc::clone(&job);
    if renewal::observe_paced(
        ctx,
        "codex-exec-quota-probe",
        ProcessKey::of(&job.source.process),
        move |pace| {
            renewal::observe_exec(
                &env,
                &reader.mover,
                &reader.source,
                &reader.intent,
                roots,
                pace,
            )
        },
        move |result| {
            let current = weak.upgrade().is_some_and(|ctx| {
                let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                let authority = meta.supervisor_authority(crate::metrics::now_us());
                authority.identity() == Some(&worker.proof)
                    && authority.revision() == worker.revision
                    && ctx.fabric.hold().is_none()
                    && ctx.human_input.seq() == worker.human_seq
                    && worker.source.current()
                    && ctx
                        .fenced_writes
                        .native
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .exec
                        .holds(&worker)
            });
            observed(
                &mut worker.progress.lock().unwrap_or_else(|p| p.into_inner()),
                since,
                result,
                current,
                Instant::now(),
            );
        },
    )
    .is_err()
    {
        *job.progress.lock().unwrap_or_else(|p| p.into_inner()) =
            Progress::Retry(Instant::now() + Duration::from_secs(5));
    }
}

#[cfg(test)]
#[path = "codex_exec_handoff_control_tests.rs"]
mod tests;
