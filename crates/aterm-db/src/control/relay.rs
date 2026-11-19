// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The trade of the members' time-signed version reports that ends a completed frontier round
//! (tracker LT-04D): the four flights of [`hearing`]'s relay, over the round's own channel,
//! after its version reports and its credential handoff. A device hears through it of a
//! member it never meets, from any peer that keeps that member's report.
//!
//! It is best effort. A peer on a release before [`hearing::RELAYS`] is sent nothing, so it
//! receives the bytes it always did; a peer that sends no flight, half of one or one that does
//! not read ends the trade and nothing else: the round stays complete and its reports stand.
//! A peer that left an offer unanswered is next sent an offer of nothing, 10 bytes, until it
//! answers one ([`Relay`]).

use super::{Endpoint, Repair, Side, failure, receive_frame, send_frame};
use crate::Result;
use astream_wire::sync::Actor;
use aterm_secrets::commons::hearing::{self, Mark, Offer, REPORT2_LEN, Reports, Said};
use std::{
    io::{Read, Write},
    sync::Mutex,
    time::Instant,
};

/// What a peer's repair state keeps of this trade from one round to the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Relay {
    /// How far the peer has been offered the version reports this device keeps.
    pub(super) mark: Mark,
    /// The peer left this device's last offer without a whole answer, as a build that does not
    /// trade and a peer whose store has no roster yet do. The next offer to it names nothing
    /// ([`Offer::none`]), so a peer that never answers costs 10 bytes a dialled round and not
    /// the kept reports' names each time. A whole answer, or a whole offer of the peer's own,
    /// ends that, and the round after names the reports again.
    pub(super) unanswered: bool,
}

impl Endpoint {
    /// Trade kept version reports with `actor` over `stream`, once `repair`'s round completed
    /// its application exchange, its version reports and its credential handoff. Nothing is
    /// sent or read unless all of these hold: this endpoint keeps reports
    /// ([`Self::with_reports`]); the peer's report in this round verified and names this
    /// endpoint's floor, [`hearing::RELAYS`], or later, as this build's own does; the handoff
    /// did not fail, which can leave its bytes unread on the channel; the wall clock reads;
    /// `until` has not passed; and the store has its roster, without which it would answer that
    /// it wants none of reports it takes once the roster is set, and the peer would not offer
    /// them again. Where the two sides decide differently (one keeps no reports or has no
    /// roster yet, or read no valid report of the other) the side that trades meets the end of
    /// the session.
    ///
    /// The initiator offers what it keeps that this peer was not offered
    /// ([`hearing::Reports::offer`]); the responder answers which of those it wants and offers
    /// its own; the initiator sends what was wanted and says which of the responder's it
    /// wants; the responder sends those. The third flight goes only where the second wanted
    /// or offered something, the fourth only where the third wanted something. A report a peer
    /// hands over is kept only for a member of this device's roster, after its signature
    /// verified under that member's binding.
    ///
    /// Every error ends the trade and is dropped. `repair`'s mark for this peer moves to what
    /// an offer covered only once the peer's answer to that offer arrived and what the answer
    /// wanted was flushed, so an offer that was cut short is made again once the peer answers
    /// an offer of nothing or sends an offer of its own ([`Relay`]).
    pub(super) fn trade_relayed(
        &self,
        stream: &mut (impl Read + Write),
        actor: Actor,
        side: Side,
        repair: &mut Repair,
        until: Instant,
    ) {
        let Some(reports) = self.reports.as_deref() else {
            return;
        };
        let Said::Version(theirs) = repair.said else {
            return;
        };
        if theirs < self.relay_floor || matches!(repair.handoff, Some(Err(_))) {
            return;
        }
        let Some(now_ms) = aterm_time::unix_millis() else {
            return;
        };
        if Instant::now() >= until {
            return;
        }
        let kept = Kept { reports, now_ms };
        if !kept.with(|reports| reports.enrolled()) {
            return;
        }
        let _ = match side {
            Side::Initiator => kept.offer_first(stream, &actor, &mut repair.relay),
            Side::Responder => kept.answer_first(stream, &actor, &mut repair.relay),
        };
    }
}

/// How many entries `want` asks for.
fn wanted(want: &[bool]) -> usize {
    want.iter().filter(|want| **want).count()
}

/// The store one trade reads and fills, and the clock reading its rules use. The store's lock is
/// taken for one read or one change of it at a time ([`Self::with`]): never while a flight
/// moves or a signature is checked.
struct Kept<'a> {
    reports: &'a Mutex<Reports>,
    now_ms: u64,
}

impl Kept<'_> {
    /// `read`, with the store locked for that call alone. A poisoned lock is taken: the store
    /// holds only verified reports, each whole.
    fn with<T>(&self, read: impl FnOnce(&mut Reports) -> T) -> T {
        read(
            &mut self
                .reports
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        )
    }

    /// Keep each of `wires`, reports a peer handed over for entries this side wanted: the store
    /// names the member a report must be signed by where it could still take it, the signature
    /// is checked with the store unlocked, and the store then keeps the report by its own rules.
    fn take(&self, wires: Vec<&[u8; REPORT2_LEN]>) {
        for wire in wires {
            let Some(member) = self.with(|reports| reports.screen(wire, self.now_ms)) else {
                continue;
            };
            if let Ok(report) = hearing::relayed(wire, &member, self.now_ms) {
                self.with(|reports| reports.accept(report, self.now_ms));
            }
        }
    }

    /// The initiator's side: the first and third flights sent, the second and fourth read. A
    /// peer that left the last offer unanswered is offered nothing; what it offers in its
    /// answer is still taken.
    fn offer_first(
        &self,
        stream: &mut (impl Read + Write),
        peer: &Actor,
        relay: &mut Relay,
    ) -> Result<()> {
        let offer = if relay.unanswered {
            Offer::none(relay.mark)
        } else {
            self.with(|reports| reports.offer(relay.mark, peer, &[], self.now_ms))
        };
        // Unanswered until the whole answer is read, whether the offer or the answer failed.
        relay.unanswered = true;
        send_frame(stream, &hearing::offer_flight(offer.entries()))?;
        let answer = receive_frame(stream, hearing::ANSWER_MAX)?;
        let (asked, theirs) =
            hearing::read_answer(&answer, offer.entries().len()).map_err(failure)?;
        relay.unanswered = false;
        let mark = &mut relay.mark;
        if wanted(&asked) == 0 && theirs.is_empty() {
            *mark = offer.covers();
            return Ok(());
        }
        let (want, give) = self.with(|reports| {
            (
                reports.want(&theirs, self.now_ms),
                reports.give(&offer, &asked),
            )
        });
        send_frame(stream, &hearing::give_flight(&want, &give))?;
        *mark = offer.covers();
        if wanted(&want) > 0 {
            let last = receive_frame(stream, hearing::LAST_MAX)?;
            let given = hearing::read_last(&last, wanted(&want)).map_err(failure)?;
            self.take(hearing::taken(&theirs, &want, &given));
        }
        Ok(())
    }

    /// The responder's side: the first and third flights read, the second and fourth sent. What
    /// the third flight brought is kept whether or not the fourth could be sent. A peer whose
    /// whole offer arrived trades, so this side's next offer to it names its reports again.
    fn answer_first(
        &self,
        stream: &mut (impl Read + Write),
        peer: &Actor,
        relay: &mut Relay,
    ) -> Result<()> {
        let first = receive_frame(stream, hearing::OFFER_MAX)?;
        let theirs = hearing::read_offer(&first).map_err(failure)?;
        relay.unanswered = false;
        let mark = &mut relay.mark;
        let (want, offer) = self.with(|reports| {
            (
                reports.want(&theirs, self.now_ms),
                reports.offer(*mark, peer, &theirs, self.now_ms),
            )
        });
        send_frame(stream, &hearing::answer_flight(&want, offer.entries()))?;
        if wanted(&want) == 0 && offer.entries().is_empty() {
            // Nothing was offered, so no answer follows: what the offer passed over is the
            // peer's own report and what the peer's offer already names.
            *mark = offer.covers();
            return Ok(());
        }
        let third = receive_frame(stream, hearing::GIVE_MAX)?;
        let (asked, given) =
            hearing::read_give(&third, offer.entries().len(), wanted(&want)).map_err(failure)?;
        let sent = if wanted(&asked) > 0 {
            let give = self.with(|reports| reports.give(&offer, &asked));
            send_frame(stream, &hearing::last_flight(&give))
        } else {
            Ok(())
        };
        if sent.is_ok() {
            *mark = offer.covers();
        }
        self.take(hearing::taken(&theirs, &want, &given));
        sent
    }
}

#[cfg(test)]
mod tests;
