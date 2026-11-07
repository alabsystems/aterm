// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The peer check: the one driver over [`aterm_db_core`]'s handshake kernels (P4,
//! `docs/design/one-mesh-proofs/P4-handshake.md`).
//!
//! A check walks the stages [`aterm_db_core::peer_check::next`] names, performs only the read the
//! stage names, classifies that read, and asks the kernel for the next stage.
//! Nothing here decides whether a peer passes: the kernel does, and `peer_check_island.rs`
//! proves what it decides (a log endpoint passes a peer only after every view it read read, was
//! its own and cleared the peer; once every record held removes the peer, each check in flight
//! clears at most one grant). A [`Pass`] exists only where the kernel reached [`Stage::Pass`] for
//! this check, and every grant takes one by value, so a grant without a fresh check does not
//! compile.

use super::{
    Custody, Error, Request, Result, Trust, denied_or_last, failure, held_records, local_failure,
    roster_or_last, roster_to_check,
};
use aterm_db_core::peer_check::{self as kernel, Read, Stage};
use aterm_secrets::{
    commons::binding::Binding,
    membership_log::{Body, EpochAudience, Log},
    swarm_roster::SwarmRoster,
};

/// A passed peer check: minted by [`check`] alone, where the kernel reached [`Stage::Pass`] for
/// this check, and spent by the one grant it clears. It is neither `Clone` nor `Copy`, and its
/// field is private to this module, so nothing else makes one.
#[derive(Debug)]
#[must_use = "a pass clears the one grant that takes it"]
pub(crate) struct Pass(());

impl Pass {
    /// Spend this pass on the grant it clears.
    pub(crate) fn spend(self) {
        #[cfg(test)]
        tape::note(tape::Step::Grant);
        let Self(()) = self;
    }

    /// Let this pass clear nothing: the phase its check guarded is not offered.
    pub(crate) fn waive(self) {
        let Self(()) = self;
    }
}

/// What a check judges.
pub(crate) enum Judged<'a> {
    /// An authenticated peer, and the log its evidence decoded to, if it sent one: the
    /// acknowledged log with that evidence added is judged too ([`Stage::Combined`]).
    Peer {
        request: &'a Request,
        log: Option<&'a Log>,
    },
    /// A pending device asking this member to enroll it. `acknowledged` is the member's own
    /// enrollment judgement (its acknowledged log's, or its journal's): what it refuses is out.
    Enrollment {
        binding: &'a Binding,
        acknowledged: &'a dyn Fn() -> Result<()>,
    },
}

impl Judged<'_> {
    fn binding(&self) -> &Binding {
        match self {
            Self::Peer { request, .. } => request.binding(),
            Self::Enrollment { binding, .. } => binding,
        }
    }
}

impl Custody<'_> {
    /// This custody as the kernel names it.
    pub(crate) const fn code(self) -> kernel::Custody {
        match self {
            Self::None => kernel::Custody::None,
            Self::Journal(_) => kernel::Custody::Journal,
            Self::Log(_) => kernel::Custody::Log,
        }
    }
}

/// One stage's read, classified: its outcome, and why it does not clear the peer (kept for the
/// refusal).
struct Reading {
    outcome: Read,
    why: Option<Error>,
}

impl Reading {
    const fn clear() -> Self {
        Self {
            outcome: Read::Clear,
            why: None,
        }
    }

    const fn of(outcome: Read, why: Error) -> Self {
        Self {
            outcome,
            why: Some(why),
        }
    }
}

fn removed() -> Error {
    Error::new("Commons peer was removed from the membership log")
}

/// Check `judged` against this Mac's own records and roster at `custody`.
///
/// At a [`Custody::Log`] endpoint every record this Mac holds can take the peer out
/// ([`Log::seat_removed`]), acknowledged or not: a `Remove` stands from the moment it is durable
/// here, as admission holds out what records ahead of their acknowledgement hold out
/// ([`aterm_secrets::client::load_acknowledged_and_ahead`]). The rest of the judgement reads the
/// acknowledged log ([`aterm_secrets::client::acknowledged_commons_log`]).
///
/// # Errors
/// The kernel refused the peer: a view took it out, or, where the kernel requires it, a view did
/// not read, held no log, or was another root's.
pub(crate) fn check(custody: Custody<'_>, trust: Trust, judged: &Judged<'_>) -> Result<Pass> {
    #[cfg(test)]
    if super::mutants::on(super::mutants::Mutant::CheckedOnce) && PASSED.with(std::cell::Cell::get)
    {
        return Ok(Pass(()));
    }
    let code = custody.code();
    let witness = matches!(judged, Judged::Peer { log: Some(_), .. });
    #[cfg(test)]
    tape::note(tape::Step::Begin(code as u8, witness));
    let mut stage = kernel::start(code);
    let mut why = None;
    let mut roster = None;
    let mut acknowledged = None;
    loop {
        let read = match (stage, custody) {
            (Stage::Pass, _) => {
                #[cfg(test)]
                tape::note(tape::Step::Pass);
                #[cfg(test)]
                PASSED.with(|passed| passed.set(true));
                return Ok(Pass(()));
            }
            (_, Custody::None) => break,
            (Stage::Roster, Custody::Journal(paths) | Custody::Log(paths)) => {
                match roster_to_check(code, roster_or_last(paths), denied_or_last(paths)) {
                    Ok(None) => Reading {
                        outcome: Read::Absent,
                        why: None,
                    },
                    Ok(Some(checked)) => {
                        let out =
                            crate::table::binding_denied_in_roster(&checked, judged.binding());
                        roster = Some(checked);
                        if out {
                            Reading::of(Read::Out, removed())
                        } else {
                            Reading::clear()
                        }
                    }
                    Err(error) => Reading::of(Read::Unread, error),
                }
            }
            (Stage::Held, Custody::Journal(paths) | Custody::Log(paths)) => {
                held(paths, trust, judged)
            }
            (Stage::Acknowledged, Custody::Journal(paths) | Custody::Log(paths)) => match judged {
                Judged::Peer { request, .. } => {
                    let (read, log) = acknowledged_log(paths, trust, request, roster.as_ref());
                    acknowledged = log;
                    read
                }
                Judged::Enrollment {
                    acknowledged: judge,
                    ..
                } => match judge() {
                    Ok(()) => Reading::clear(),
                    Err(error) => Reading::of(Read::Out, error),
                },
            },
            (Stage::Combined, Custody::Journal(paths) | Custody::Log(paths)) => match judged {
                Judged::Peer {
                    request,
                    log: Some(peer_log),
                } => combined(
                    paths,
                    request,
                    peer_log,
                    acknowledged.take(),
                    roster.as_ref(),
                ),
                _ => Reading::of(
                    Read::Unread,
                    Error::new("Commons peer sent no log to combine"),
                ),
            },
            _ => break,
        };
        #[cfg(test)]
        tape::note(tape::Step::Read(stage as u8, read.outcome as u8));
        if read.why.is_some() {
            why = read.why;
        }
        stage = next(code, stage, read.outcome, witness);
    }
    #[cfg(test)]
    tape::note(tape::Step::Refuse);
    Err(why.unwrap_or_else(|| Error::new("Commons peer check refused the peer")))
}

#[cfg(test)]
thread_local! {
    /// Whether this thread's checks passed a peer once ([`super::mutants::Mutant::CheckedOnce`]):
    /// a test session's responder runs in a thread of its own.
    static PASSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What an `atdb/5` session's view of the records held is at `custody` after its read
/// classified to `read` ([`kernel::view`]), with the session rules' mutants on in a test thread.
pub(crate) fn view(custody: kernel::Custody, read: Read) -> kernel::View {
    #[cfg(test)]
    if super::mutants::on(super::mutants::Mutant::UnreadCustodyAdmits)
        && custody == kernel::Custody::Log
        && read == Read::Unread
    {
        return kernel::View::None;
    }
    kernel::view(custody, read)
}

/// The kernel's next stage, with the session rules' mutants on in a test thread.
fn next(custody: kernel::Custody, stage: Stage, read: Read, witness: bool) -> Stage {
    #[cfg(test)]
    {
        use super::mutants::{Mutant, on};
        let log = custody == kernel::Custody::Log;
        if on(Mutant::UnreadCustodyAdmits)
            && log
            && read == Read::Unread
            && matches!(stage, Stage::Held | Stage::Acknowledged)
        {
            return Stage::Pass;
        }
        if on(Mutant::AcknowledgedOnly) && log && stage == Stage::Roster {
            return match kernel::next(custody, stage, read, witness) {
                Stage::Held => Stage::Acknowledged,
                next => next,
            };
        }
    }
    kernel::next(custody, stage, read, witness)
}

/// Every record this Mac holds, read for `judged`'s binding: out where they remove it.
fn held(paths: &aterm_secrets::client::Paths, trust: Trust, judged: &Judged<'_>) -> Reading {
    let held = match held_records(paths) {
        Ok(held) => held,
        Err(error) => {
            return Reading::of(
                Read::Unread,
                local_failure("records read for a peer check", error),
            );
        }
    };
    let Some(log) = held.log() else {
        return Reading::of(
            Read::Absent,
            Error::new("this Mac holds no membership log to judge a Commons peer by"),
        );
    };
    if !matches!(
        log.genesis().body(),
        Body::Genesis { root, .. }
            if root.network == trust.network && root.authority == trust.authority
    ) {
        return Reading::of(
            Read::Foreign,
            Error::new("this Mac's membership records belong to another root"),
        );
    }
    if log.seat_removed(judged.binding()) {
        return Reading::of(Read::Out, removed());
    }
    Reading::clear()
}

/// The acknowledged log, read and judged for `request`: out where it, its roster or `roster`
/// denies the peer or a seat that shares its actor or recipient; foreign where it is another
/// root's or lineage's; absent where it holds no log or no Commons epoch to judge by
/// ([`aterm_secrets::client::CommonsLog`]). Clear gives the log the combined stage adds the
/// peer's evidence to.
fn acknowledged_log(
    paths: &aterm_secrets::client::Paths,
    trust: Trust,
    request: &Request,
    roster: Option<&SwarmRoster>,
) -> (Reading, Option<Log>) {
    #[cfg(test)]
    let judged = super::faults::records_read()
        .and_then(|()| aterm_secrets::client::acknowledged_commons_log_now(paths, trust));
    #[cfg(not(test))]
    let judged = aterm_secrets::client::acknowledged_commons_log_now(paths, trust);
    match acknowledged_reading(judged) {
        Err(reading) => (reading, None),
        Ok(log) if denied_by(paths, &log, request, roster) => {
            (Reading::of(Read::Out, removed()), None)
        }
        Ok(log) => (Reading::clear(), Some(log)),
    }
}

/// The acknowledged log's classification ([`aterm_secrets::client::CommonsLog`]) as
/// [`Stage::Acknowledged`] reads it: the trusted log to judge the peer by, or the reading the
/// stage stops on. A log of another root or of another vault lineage is foreign, so the kernel
/// refuses it at every custody; only a store with no log or no Commons epoch has nothing to
/// judge by.
fn acknowledged_reading(
    judged: aterm_secrets::Result<aterm_secrets::client::CommonsLog>,
) -> std::result::Result<Log, Reading> {
    use aterm_secrets::client::CommonsLog;
    #[cfg(test)]
    let judged = judged.map(|log| match log {
        CommonsLog::OtherLineage
            if super::mutants::on(super::mutants::Mutant::LineageIsNoJudgement) =>
        {
            CommonsLog::NoCommonsEpochs
        }
        log => log,
    });
    let foreign = |why: &str| Reading::of(Read::Foreign, Error::new(why));
    match judged {
        Err(error) => Err(Reading::of(
            Read::Unread,
            local_failure("records read for a peer check", error),
        )),
        Ok(CommonsLog::NoLog | CommonsLog::NoCommonsEpochs) => Err(Reading::of(
            Read::Absent,
            Error::new("this Mac's acknowledged membership log cannot judge a Commons peer"),
        )),
        Ok(CommonsLog::OtherRoot) => Err(foreign(
            "this Mac's acknowledged membership log belongs to another root",
        )),
        Ok(CommonsLog::OtherLineage) => Err(foreign(
            "this Mac's acknowledged membership log is not of its vault's lineage",
        )),
        Ok(CommonsLog::Trusted(log, _)) => Ok(*log),
    }
}

/// The acknowledged log `local` with the peer's evidence `peer_log` added, judged for
/// `request`: another network's evidence is foreign; evidence that does not combine, or a
/// combined log that denies the peer or gives it no historical seat, is out.
fn combined(
    paths: &aterm_secrets::client::Paths,
    request: &Request,
    peer_log: &Log,
    local: Option<Log>,
    roster: Option<&SwarmRoster>,
) -> Reading {
    let Some(mut combined) = local else {
        return Reading::of(
            Read::Unread,
            Error::new("this Mac's acknowledged membership log was not read"),
        );
    };
    if combined.network() != peer_log.network() {
        return Reading::of(
            Read::Foreign,
            Error::new("Commons peer belongs to another membership log"),
        );
    }
    for record in peer_log.records().skip(1) {
        if let Err(error) = combined.insert(record.clone()) {
            return Reading::of(Read::Out, failure(error));
        }
    }
    if denied_by(paths, &combined, request, roster)
        || !combined.has_historical_seat(request.binding())
    {
        return Reading::of(Read::Out, removed());
    }
    Reading::clear()
}

/// Whether `log` denies `request`'s peer: it removes the peer ([`Log::seat_removed`]), leaves its
/// recipient out of a newest epoch, the log's roster (or, while the log has none, `roster`)
/// denies it, or a seat that shares its actor or recipient is left out or denied. A log roster
/// that does not read denies it.
fn denied_by(
    paths: &aterm_secrets::client::Paths,
    log: &Log,
    request: &Request,
    roster: Option<&SwarmRoster>,
) -> bool {
    let binding = request.binding();
    let (actor, recipient) = (binding.actor(), binding.recipient());
    let Ok(log_roster) = aterm_secrets::client::local_swarm_roster_for_log_now(paths, log) else {
        return true;
    };
    let log_roster = log_roster.as_ref().or(roster);
    let membership = log.membership_ref();
    log.seat_removed(binding)
        || log.audience_left_out_recipient(&EpochAudience::Commons, &recipient)
        || log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)
        || log_roster.is_some_and(|r| crate::table::binding_denied_in_roster(r, binding))
        || membership.members().iter().any(|(m_actor, m)| {
            (*m_actor == actor || m.binding.recipient() == recipient)
                && (log.audience_left_out(&EpochAudience::Commons, m_actor)
                    || log.audience_left_out(&EpochAudience::Everyone, m_actor)
                    || log_roster
                        .is_some_and(|r| aterm_secrets::client::member_denied_by_roster(r, m)))
        })
}

/// The tape of one test thread's checks and grants: what each check read, classified, and
/// whether it passed, and each pass spent. A conformance test replays it through the island's
/// step (`peer_check_island.rs`) to hold real sessions to the model's event order.
#[cfg(test)]
pub(crate) mod tape {
    /// One event of the tape, its kernel values as codes (the kernel's enums carry no `Debug`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Step {
        /// A check began at this custody code, with or without a peer log to combine.
        Begin(u8, bool),
        /// A stage (its code) read, classified to this outcome code.
        Read(u8, u8),
        /// The check passed: it minted a pass.
        Pass,
        /// The check refused.
        Refuse,
        /// A grant spent a pass.
        Grant,
    }

    thread_local! {
        static TAPE: std::cell::RefCell<Option<Vec<Step>>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn note(step: Step) {
        TAPE.with(|tape| {
            if let Some(tape) = tape.borrow_mut().as_mut() {
                tape.push(step);
            }
        });
    }

    /// Run `run` with this thread's checks and grants taped, and the tape.
    pub(crate) fn recorded<T>(run: impl FnOnce() -> T) -> (T, Vec<Step>) {
        struct Restore(Option<Vec<Step>>);
        impl Drop for Restore {
            fn drop(&mut self) {
                TAPE.with(|tape| *tape.borrow_mut() = self.0.take());
            }
        }
        let restore = Restore(TAPE.with(|tape| tape.replace(Some(Vec::new()))));
        let out = run();
        let steps = TAPE.with(|tape| tape.replace(None)).unwrap_or_default();
        drop(restore);
        (out, steps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_secrets::client::CommonsLog;

    /// The stage a log endpoint's check reaches after its acknowledged log classified to `log`.
    fn after(log: CommonsLog) -> Stage {
        let read =
            acknowledged_reading(Ok(log)).map_or_else(|reading| reading.outcome, |_| Read::Clear);
        next(kernel::Custody::Log, Stage::Acknowledged, read, false)
    }

    /// PF4: an acknowledged log of another vault lineage, or of another root, refuses the peer at
    /// a log endpoint, where a store with no log or no Commons epoch, which has nothing to judge
    /// by, leaves the verdict to the records held ([`Stage::Held`]) and passes. Before PF4
    /// `trusted_commons_log` answered `Ok(None)` for all four, so the check could not refuse the
    /// first two without refusing the last two.
    ///
    /// NEGATIVE CONTROL (in the test): read as one with no Commons epoch
    /// ([`super::super::mutants::Mutant::LineageIsNoJudgement`], as the `Ok(None)` reading did),
    /// a log of another lineage passes the peer.
    #[test]
    fn an_acknowledged_log_of_another_lineage_refuses_where_one_with_nothing_to_judge_passes() {
        use super::super::mutants::{Mutant, with};
        for (log, stage, why) in [
            (CommonsLog::NoLog, Stage::Pass, "no log"),
            (CommonsLog::NoCommonsEpochs, Stage::Pass, "no Commons epoch"),
            (CommonsLog::OtherLineage, Stage::Refuse, "another lineage"),
            (CommonsLog::OtherRoot, Stage::Refuse, "another root"),
        ] {
            let reached = after(log);
            assert!(reached == stage, "{why}: stage {}", reached as u8);
        }
        let reached = with(Mutant::LineageIsNoJudgement, || {
            after(CommonsLog::OtherLineage)
        });
        assert!(
            reached == Stage::Pass,
            "NEGATIVE CONTROL: another lineage read as nothing to judge passes the peer: stage {}",
            reached as u8
        );
    }
}
