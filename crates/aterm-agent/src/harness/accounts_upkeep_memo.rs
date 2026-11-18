// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ACCOUNTS' UPKEEP'S VERDICTS: whether a duty of the window's upkeep pass ([`Duty`]) needs
//! to run again, judged by replaying what its last run read and compared
//! (`aterm_uds::trace`).
//!
//! On the keeper's thread (`aterm_uds::trace::in_keeper`), within a pass ([`pass`]), each duty
//! is made through [`run`]. Where the duty's kept verdict stands (the same inputs given, and its
//! trace replays as it was at the pass's [`Now`] and at its monotonic reading, which every
//! `aterm_uds::trace::Since` deadline the trace recorded is judged by), its work is skipped, its
//! kept answer is returned, and its trace is recorded into any scope armed around it, as if it
//! had read the same again. Otherwise the work runs under a trace scope of its own, and its run
//! is kept as the duty's verdict only where it may be kept (`aterm_uds::trace::Trace::unkept`:
//! no effect, no retry, no poison, every read settled). A run not kept leaves the duty with no
//! verdict, so it runs at the next pass too.
//!
//! Anywhere else (a session's thread, the network move's, a verb, any thread that is not the
//! keeper's) [`pass`] and [`run`] make the work as it is: nothing is recorded, kept or skipped,
//! and nothing is counted.
//!
//! The verdicts are the keeper's thread's own, and stand for one pass context: a pass whose
//! context (what the upkeep was called with) differs from the last one's drops them all.
//!
//! What the memo did is counted (`accounts_upkeep_counts`): each duty run, skipped, kept and
//! poisoned.
//!
//! Its two decisions refine the `AccountUpkeepVerdict` model
//! (`aterm_models_harness::account_upkeep_verdict_model`): a kept verdict answers only while
//! nothing its trace read moved and no deadline it compared came (`Hit`, [`standing`]), and a
//! run is kept only where it did nothing and read only what had settled (`Run`,
//! [`run_replaying`]). The keeper's look is its third action (`aterm_gui`'s accounts' keeper).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::time::Instant;

use aterm_uds::trace::{self, Now, Trace, Unkept};

use super::accounts::Cli;
use super::accounts_upkeep_counts::{Event, count};

/// One duty of the window's upkeep pass, in the order the pass makes them
/// (`accounts_sync::upkeep`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Duty {
    /// The first adoption of a shared or native preparation (`shared_renewal::adopt_first`).
    Prepare,
    /// Claude Code's periodic sync from its source, where one is called for.
    ClaudeSync,
    /// The default login's heal against the roster.
    Heal,
    /// The settling of the lab's mailbox needs.
    MailboxNeeds,
    /// The reading of the rows on their vault token here (`tokens_here.json`).
    TokensHere,
    /// The completion of a saved explicit choice of Claude Code's account.
    ClaudePin,
    /// The sync of Claude Code's roster where it is missing.
    ClaudeMissing,
    /// The automatic renewal of Claude Code's next due login.
    ClaudeRenew,
    /// One other CLI's pool sync.
    PoolSync(Cli),
    /// The completion of Codex's default login.
    CodexDefault,
    /// One other CLI's saved choice, else its next due login's renewal.
    PoolRenew(Cli),
    /// The log line of the account sheet's conflicts, once per change.
    SheetLog,
    /// The observation of one CLI's committed logins, where the ladder does not sign it in.
    Observe(Cli),
}

/// A duty's kept verdict: what its run was given and answered, and what it read and compared.
struct Kept {
    /// The duty.
    duty: Duty,
    /// What the run was given ([`run`]'s `given`).
    given: Box<dyn Any>,
    /// What the run answered.
    answer: Box<dyn Any>,
    /// What the run read and compared.
    trace: Trace,
}

/// The keeper's thread's verdicts, and the pass context they stand for.
struct Memo {
    /// What the pass the verdicts were kept in was called with ([`pass`]'s `context`).
    context: Option<Box<dyn Any>>,
    /// Each duty's kept verdict.
    kept: Vec<Kept>,
}

impl Memo {
    /// Take `duty`'s kept verdict out, where it has one.
    fn take(&mut self, duty: Duty) -> Option<Kept> {
        let at = self.kept.iter().position(|kept| kept.duty == duty)?;
        Some(self.kept.swap_remove(at))
    }
}

std::thread_local! {
    /// This thread's verdicts.
    static MEMO: RefCell<Memo> = const {
        RefCell::new(Memo {
            context: None,
            kept: Vec::new(),
        })
    };
    /// The wall and monotonic readings of the pass armed on this thread ([`pass`]); `None`
    /// outside one, and on any thread that is not the keeper's.
    static ARMED: Cell<Option<(Now, Instant)>> = const { Cell::new(None) };
}

/// Make the pass `work` at `now`, the reading every duty of it compares through, and at `mono`,
/// a reading taken as the pass begins of the monotonic clock its duties compare their waits
/// against. On the keeper's thread, its duties are made through their verdicts ([`run`]), judged
/// at both readings, which stand for the pass `context` alone: those kept in a pass of another
/// context are dropped first. `context` is called only there. Anywhere else, and within a pass
/// already armed on this thread, `work` runs as it is.
pub(crate) fn pass<C, R>(
    context: impl FnOnce() -> C,
    now: Now,
    mono: Instant,
    work: impl FnOnce() -> R,
) -> R
where
    C: PartialEq + 'static,
{
    if !trace::in_keeper() || ARMED.get().is_some() {
        return disarmed(work);
    }
    let context = context();
    MEMO.with_borrow_mut(|memo| {
        let same = memo
            .context
            .as_ref()
            .and_then(|kept| kept.downcast_ref::<C>())
            .is_some_and(|kept| *kept == context);
        if !same {
            memo.kept.clear();
            memo.context = Some(Box::new(context));
        }
    });
    armed(Some((now, mono)), work)
}

/// Run `work` with no pass armed on this thread.
fn disarmed<R>(work: impl FnOnce() -> R) -> R {
    armed(None, work)
}

/// Run `work` with this thread's armed pass set to `at`, put back as `work` returns or unwinds.
fn armed<R>(at: Option<(Now, Instant)>, work: impl FnOnce() -> R) -> R {
    /// Puts the armed pass back.
    struct Restore(Option<(Now, Instant)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ARMED.set(self.0);
        }
    }
    let _restore = Restore(ARMED.replace(at));
    work()
}

/// Make `duty`'s `work`, given `given` (each input it is handed that it does not read for
/// itself), through its verdict: within a pass on the keeper's thread ([`pass`]), its kept
/// answer where its verdict stands, and otherwise its work, run under a scope of its own and
/// kept as its verdict where the run may be kept; anywhere else, its work.
pub(crate) fn run<G, R>(duty: Duty, given: G, work: impl FnOnce() -> R) -> R
where
    G: PartialEq + 'static,
    R: Clone + 'static,
{
    run_replaying(duty, given, work, |_| {})
}

/// [`run`], calling `replayed` with the kept answer where the work is skipped: for a duty whose
/// run notes something in memory that its answer says, which a skip must note again.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountUpkeepVerdict",
        action = "Run",
        project = "aterm_agent::harness::accounts_upkeep_memo::tests::verdict_transition"
    )
)]
pub(crate) fn run_replaying<G, R>(
    duty: Duty,
    given: G,
    work: impl FnOnce() -> R,
    replayed: impl FnOnce(&R),
) -> R
where
    G: PartialEq + 'static,
    R: Clone + 'static,
{
    let Some((now, mono)) = ARMED.get() else {
        return work();
    };
    if let Some(answer) = standing(duty, &given, now, mono) {
        count(Event::DutySkipped);
        replayed(&answer);
        return answer;
    }
    count(Event::DutyRun);
    let (answer, ran) = trace::scope(work);
    match ran.unkept() {
        None => {
            count(Event::DutyKept);
            let kept = Kept {
                duty,
                given: Box::new(given),
                answer: Box::new(answer.clone()),
                trace: ran,
            };
            MEMO.with_borrow_mut(|memo| memo.kept.push(kept));
        }
        Some(Unkept::Poison(_)) => count(Event::DutyPoisoned),
        Some(_) => {}
    }
    answer
}

/// `duty`'s kept answer, where it has a verdict, kept for the same `given`, whose trace stands
/// at the pass's readings `now` and `mono`; that trace is then recorded into the scope armed
/// around this call, if any. A verdict that does not stand is dropped.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountUpkeepVerdict",
        action = "Hit",
        project = "aterm_agent::harness::accounts_upkeep_memo::tests::verdict_transition"
    )
)]
fn standing<G, R>(duty: Duty, given: &G, now: Now, mono: Instant) -> Option<R>
where
    G: PartialEq + 'static,
    R: Clone + 'static,
{
    let kept = MEMO.with_borrow_mut(|memo| memo.take(duty))?;
    let answer = kept
        .given
        .downcast_ref::<G>()
        .filter(|kept| *kept == given)
        .and_then(|_| kept.answer.downcast_ref::<R>())
        .cloned()?;
    kept.trace.stands(now, mono).ok()?;
    trace::absorb(&kept.trace);
    MEMO.with_borrow_mut(|memo| memo.kept.push(kept));
    Some(answer)
}

/// What moves a duty's inputs and the clocks between its makes: no code of the memo's does it.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountUpkeepVerdict",
    action = "Soil",
    reason = "What the next run of a duty finds to do is the state it reads, not the memo's."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountUpkeepVerdict",
    action = "Stir",
    reason = "A file a duty reads is written by others, and whether it settled is the clock's."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountUpkeepVerdict",
    action = "Move",
    reason = "A file, listing, owner's digest or generation a trace read is moved by others."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountUpkeepVerdict",
    action = "Elapse",
    reason = "The clocks advance independently of the upkeep."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AccountUpkeepVerdict",
    action = "Floor",
    reason = "The monotonic clock passes the keeper's least wait independently of the upkeep."
)]
pub fn account_upkeep_verdict_environment() {}

#[cfg(test)]
#[path = "accounts_upkeep_memo_tests.rs"]
mod tests;
