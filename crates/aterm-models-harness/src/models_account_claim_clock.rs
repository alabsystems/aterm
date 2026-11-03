// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A Mac's accounts under what its peers say of its clock (docs/DESIGN-fluid-swarm-2026-10-28.md
/// F22). Mac A's standing moves through Unknown, Agrees, Split and Off; its view may show a
/// peer's stale senior lease on its home row; it may run a session on a row that is not its home.
/// Peer B reads A as skewed or not, and holds a claim on A's home row or not.
///
/// `st`: Unknown (0), Agrees (1), Split (2), Off (3). `plive`: A's view shows a peer's senior lease
/// on A's home row. `lease`: A runs a session on a row that is not its home. `turned`: a turn end
/// passed since `st` last changed. `homeok`: the verdict on A's home row, not yet judged since the
/// standing or the view changed (2), refused (0) or Ok (1). `askew`: B's latest reading of A holds
/// A (it does not agree within `T/4`), as B's every table read now says. `bseen`: B read A as held
/// at its last touch. `bcold`: B holds a claim on A's home row. `bnew`: B took that claim anew
/// while A was Off.
///
/// B is a peer whose reading counted A Off: the round that moved A Off read A outside `±T/2` on
/// B's side too, so `Judge` into Off makes `askew` 1, and while A stays Off B's reading stays past
/// `T/4` (aterm-db's `every_peer_that_counted_an_off_mac_holds_for_it`). A peer with no direct
/// round, or one whose clock sits with A's wrong one, is no such B (the design's §4.2, F22 items 6
/// and 4); a lease B took before A went Off yields at B's next round (item 8).
///
/// Mutants: 1, an Off Mac reads its home row by its own view; 2, `ClockOff` kept at a turn end as
/// an unrecorded row once was; 3, no hold for a skewed peer; 4, holds named only by the decisive
/// readings judged against the reader's own tolerance, as `skewed` is (an Off Mac in the band
/// between `T/2` and `T` reads as agreeing to a peer that counted it).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_claim_clock_model() -> Model {
    aterm_spec::ty_model! {
        AccountClaimClock {
            const Buggy = 0;
            var st = 0; var plive = 0; var lease = 0; var turned = 0; var homeok = 2;
            var askew = 0; var bseen = 0; var bcold = 0; var bnew = 0;
            action Judge {
                st = if st == 3 {0} else {st + 1}; turned = 0; homeok = 2;
                askew = if st == 2 {if Buggy == 4 {askew} else {1}} else {askew};
            }
            action StalePeer when (plive == 0) { plive = 1; homeok = 2; }
            action Skew when (3 > st || Buggy == 4) { askew = if askew == 1 {0} else {1}; }
            action Launch when (st <= 1 && lease == 0) { lease = 1; }
            action TurnEnd {
                turned = if 2 <= st {1} else {turned};
                lease = if 2 <= st && Buggy == 2 {lease} else {if 2 <= st {0} else {lease}};
            }
            action Verdict {
                homeok = if st == 3 && Buggy == 1 {
                    if plive == 1 {0} else {1}
                } else {
                    if st == 3 {1} else {if plive == 1 {0} else {1}}
                };
            }
            action BRound {
                bseen = askew;
                bcold = if askew == 1 && Buggy == 3 {bcold} else {if askew == 1 {0} else {bcold}};
            }
            action BCold when (askew == 0 || Buggy == 3) {
                bcold = 1; bseen = askew; bnew = if st == 3 {1} else {bnew};
            }
            invariant OffKeepsItsHome: 3 > st || homeok > 0;
            invariant UnsureLeasesNothingPastATurn: st <= 1 || turned == 0 || lease == 0;
            invariant NoColdOnASkewedHome: bseen == 0 || bcold == 0;
            invariant NoColdOnAnOffHome: bnew == 0;
        }
    }
}
