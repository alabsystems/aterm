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
//!
//! Every stage the check reads itself reads through one look at the vault ([`VaultLook`]): one
//! shared observation of its state lock, held from the first stage that reads to the last, and one
//! look at each file for every memo those stages ask, which read the same files. A stage after a
//! look that holds nothing (a writer held the lock when it was taken, or its lock could not be
//! observed or found at the vault's path: [`VaultLook::holds_vault`]) takes a look of its own. An
//! enrollment's Acknowledged stage runs its caller's judgement instead ([`Judged::Enrollment`]),
//! which reads the vault itself when the stage runs and may open its Commons custody, so the check
//! lets its look go first. Nothing else a stage does while the look lives takes the state lock
//! exclusively or does network I/O: an exclusive take on this thread under it would wait for good
//! (a build with debug assertions panics).
//!
//! The model reads each view at its own stage, and the kept look keeps that true. What moves the
//! Held stage's view, every record this Mac holds, is written only under the state lock. A look
//! that holds the vault keeps every writer out from the stage that took it to the last stage the
//! check reads through it (an enrollment's Held): a removal such a writer makes lands after those
//! reads, as in a run of the model whose reads all come before the change, and the next check, the
//! next grant's, reads it, while the model's bound of one grant per check in flight once every
//! record held removes the peer allows this check's; one that lands before that look is taken is
//! read through it, as before. A look that holds nothing keeps no writer out, and the one stage
//! read through it meets such a writer as each stage did when it took a look of its own. A change
//! made without the lock (a mode or owner change, a process that ignores the lock, `.lock` replaced
//! with a writer under the new file) is seen no later than the next check: at the next check where
//! a stage's read stands for the stamps the look first took, and at the stage that reads afresh on
//! a memo miss, as before; each such read still happens no later than its stage, as it did within a
//! stage when each stage took a look of its own.

use super::{
    Custody, Error, Request, Result, Trust, denied_or_last, failure, held_records, local_failure,
    roster_or_last_in, roster_to_check,
};
use aterm_db_core::peer_check::{self as kernel, Read, Stage};
use aterm_secrets::{
    client::{Paths, VaultLook},
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
    // The look every stage reads through ([`through`]), let go when the check returns.
    let mut kept: Option<VaultLook<'_>> = None;
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
                let look = through(&mut kept, paths);
                match roster_to_check(code, roster_or_last_in(look), denied_or_last(look)) {
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
                held(through(&mut kept, paths), trust, judged)
            }
            (Stage::Acknowledged, Custody::Journal(paths) | Custody::Log(paths)) => match judged {
                Judged::Peer { request, .. } => {
                    let (read, log) = acknowledged_log(
                        through(&mut kept, paths),
                        trust,
                        request,
                        roster.as_ref(),
                    );
                    acknowledged = log;
                    read
                }
                Judged::Enrollment {
                    acknowledged: judge,
                    ..
                } => {
                    // The judgement may open this Mac's Commons custody: no observation of the
                    // state lock may stand over it.
                    kept = None;
                    match judge() {
                        Ok(()) => Reading::clear(),
                        Err(error) => Reading::of(Read::Out, error),
                    }
                }
            },
            (Stage::Combined, Custody::Journal(paths) | Custody::Log(paths)) => match judged {
                Judged::Peer {
                    request,
                    log: Some(peer_log),
                } => combined(
                    through(&mut kept, paths),
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
        #[cfg(test)]
        if let Custody::Journal(paths) | Custody::Log(paths) = custody {
            tape::between(paths);
        }
        stage = next(code, stage, read.outcome, witness);
    }
    #[cfg(test)]
    tape::note(tape::Step::Refuse);
    Err(why.unwrap_or_else(|| Error::new("Commons peer check refused the peer")))
}

/// The look a check's next stage reads through: the one `kept` holds while it holds the vault
/// ([`VaultLook::holds_vault`]), and otherwise a new one in its place, the old one's observation
/// let go before the new one observes.
fn through<'k, 'p>(kept: &'k mut Option<VaultLook<'p>>, paths: &'p Paths) -> &'k VaultLook<'p> {
    if !kept.as_ref().is_some_and(VaultLook::holds_vault) {
        *kept = None;
    }
    kept.get_or_insert_with(|| {
        let look = VaultLook::new(paths);
        #[cfg(test)]
        tape::look_taken(paths);
        look
    })
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

/// Every record this Mac holds, read through `look` for `judged`'s binding: out where they remove
/// it.
fn held(look: &VaultLook<'_>, trust: Trust, judged: &Judged<'_>) -> Reading {
    let held = match held_records(look) {
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

/// The acknowledged log, read through `look` and judged for `request`: out where it, its roster
/// or `roster` denies the peer or a seat that shares its actor or recipient; foreign where it is
/// another root's or lineage's; absent where it holds no log or no Commons epoch to judge by
/// ([`aterm_secrets::client::CommonsLog`]). Clear gives the log the combined stage adds the
/// peer's evidence to.
fn acknowledged_log(
    look: &VaultLook<'_>,
    trust: Trust,
    request: &Request,
    roster: Option<&SwarmRoster>,
) -> (Reading, Option<Log>) {
    #[cfg(test)]
    let judged = super::faults::records_read().and_then(|()| look.acknowledged_commons_log(trust));
    #[cfg(not(test))]
    let judged = look.acknowledged_commons_log(trust);
    match acknowledged_reading(judged) {
        Err(reading) => (reading, None),
        Ok(log) if denied_by(look, &log, request, roster) => {
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
/// `request` through `look`: another network's evidence is foreign; evidence that does not
/// combine, or a combined log that denies the peer or gives it no historical seat, is out.
fn combined(
    look: &VaultLook<'_>,
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
    // Where this Mac's log holds each of the peer's heads it holds every record the peer's does,
    // and each insert would answer `Known` ([`Log::holds_all`]).
    if !combined.holds_all(peer_log) {
        for record in peer_log.records().skip(1) {
            if let Err(error) = combined.insert(record.clone()) {
                return Reading::of(Read::Out, failure(error));
            }
        }
    }
    if denied_by(look, &combined, request, roster)
        || !combined.has_historical_seat(request.binding())
    {
        return Reading::of(Read::Out, removed());
    }
    Reading::clear()
}

/// Whether `log` denies `request`'s peer: it removes the peer ([`Log::seat_removed`]), leaves its
/// recipient out of a newest epoch, the log's roster (read through `look`, or, while the log has
/// none, `roster`) denies it, or a seat that shares its actor or recipient is left out or denied.
/// A log roster that does not read denies it.
fn denied_by(
    look: &VaultLook<'_>,
    log: &Log,
    request: &Request,
    roster: Option<&SwarmRoster>,
) -> bool {
    let binding = request.binding();
    let (actor, recipient) = (binding.actor(), binding.recipient());
    let Ok(log_roster) = look.local_swarm_roster_for_log(log) else {
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
#[path = "peer_check_tape_tests.rs"]
pub(crate) mod tape;

#[cfg(test)]
#[path = "peer_check_tests.rs"]
mod tests;
