// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An `@*` subscription never tells its reader a session is watched before the
//! watch exists, and the fabric bridge reading that subscription never loses a
//! session's `topic add` silently: it stops reading a session's opt-ins only on
//! the evidence of a watch, a `sub` ack aterm writes after the watch's seed.

use super::Model;

/// One session S, one `@*` subscription that also asked for the `sessions`
/// stream, and the fabric bridge that holds it.
///
/// THE ENDPOINT HALF is the push loop's membership pass (`aterm-gui`
/// `subscribe.rs`, `drain_membership`): `Read` is the store read adoption
/// decides from, `Drain` the roster drain that decides what `session-created`
/// line S gets, `Seed` builds the adopted watches after the guard drops, and
/// `Write` puts the pass's frames on the wire — S's `sub <local> <sid>` ack
/// (`ackwire`) when the pass adopted it, then its line. `Register`, `Exit`
/// and `FreeSlot` (a slot under `MAX_SUBSCRIBE_TARGETS` opening) are the
/// environment, and may land at any point, a pass included — a registration
/// after the read is above both cursors' high-water, so the next pass decides
/// both for it. `line`/`told` are `0` for no line, `1` for a plain
/// `session-created` and `2` for one carrying `watch=deferred`: S was live
/// under the read and the cap left it unwatched. The law,
/// `NotAnnouncedBeforeItsWatchOrExit`: once the reader has been told S
/// plainly, S's watch exists or S has exited — the exit's line rides the same
/// frame, because a session gone under the read has its `Exited` record in
/// the same batch.
///
/// THE BRIDGE HALF (`aterm-link` `bridge.rs`). `Add` is S's agent opting into
/// a topic; a watch seeded before it pushes `EVENT <local> topic add`
/// (`pushed`), and nothing pushes an add made before the seed or with no
/// watch. `Roster` is a `sessions` read that succeeded (`listed`, and the
/// prune of every per-sid entry — the ack, the read, the opt-ins — when S is
/// gone); `Ask` sends `@<sid> topic ls` and `seen` is what the answer
/// carries, `Answer` is that read succeeding and `Fail` the endpoint
/// refusing it; `HearAck` and `HearPush`
/// are the run loop taking S's ack and the add's push line. `Outage` is the
/// broker going away and coming back: the parked loop drops every push-lane
/// line but the acks (`Bridge::park_detached`), and the re-attach owes a read
/// of every session. `DropAck` is an ack the bridge never takes — no path in
/// the code drops one today; it is here so the law is seen not to rest on an
/// ack arriving. `acked` is `Bridge::watched`'s entry for S and `stopped`
/// is the bridge having stopped reading S — acked, and its last owed read
/// made (`Bridge::topics_sampled`).
///
/// THE LEVEL-TRIGGERED RULE is `Answer`: after a successful read the bridge
/// stops reading S only if it holds S's ack, and otherwise reads S again on
/// every roster round — "not watched" is the absence of an ack, re-derived
/// every round, not a line the bridge must not miss. Losing any line (an
/// outage, a failed roster read, a session the handshake never acked) then
/// costs reads and nothing else. The laws: `StoppedOnlyWhileWatched` — the
/// bridge stops reading S only while S's watch exists (or S is gone); and
/// `NoTopicAddLostSilently` — an add S made that the bridge does not hold is
/// in flight on the watch, or the bridge is still reading S.
///
/// `Buggy=1` replays the code before these fixes (`b994cadd0`): adoption and
/// the roster drain were two store reads, adoption's first, and the roster
/// announced every `Created` record plainly; and the bridge ignored `sub`
/// lines, read a session once when it first listed it, and recorded the read
/// as done even when the endpoint refused it. A registration between the two
/// reads is then announced one wake before its watch exists, so is a session
/// the cap deferred, and a session read before its watch's seed is never
/// read again. Tier-1: `aterm-gui` `subscribe.rs`,
/// `membership_passes_refine_the_announcement_model`, drives the real
/// `drain_membership` through every endpoint branch and replays the
/// historical two-read pass as its caught negative control; `aterm-link`
/// `bridge.rs`, `bridge_rounds_refine_the_announcement_model`, drives the real
/// bridge through every bridge action but `DropAck` and replays the
/// read-once rule as its caught negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn subscribe_announcement_order_model() -> Model {
    crate::ty_model! {
        SubscribeAnnouncementOrder {
            const Buggy = 0;
            // The store: S registered (ever), and S registered and not gone.
            var reg = 0;
            var live = 0;
            // A slot under the cap is free for S, or S holds one.
            var room = 0;
            // The pass: 0 between passes, 1 read taken, 2 roster drained,
            // 3 adopted watches seeded.
            var phase = 0;
            // What the pass's one read saw, and whether it adopts S.
            var snap_reg = 0;
            var snap_live = 0;
            var picked = 0;
            // S's line in this pass's frame, not yet written.
            var line = 0;
            // The roster cursor has passed S's `Created` record.
            var drained = 0;
            // S's watch exists.
            var watched = 0;
            // What the reader has been told about S.
            var told = 0;
            // S's `sub <local> <sid>` ack is on the wire, not yet taken.
            var ackwire = 0;
            // S's agent has opted in, and the add's pushed line is on the
            // wire, not yet taken.
            var added = 0;
            var pushed = 0;
            // The bridge: its roster lists S; it holds S's ack; a `topic ls`
            // of S is in flight, and what its answer carries; it holds S's
            // opt-in; it has stopped reading S.
            var listed = 0;
            var acked = 0;
            var asking = 0;
            var seen = 0;
            var known = 0;
            var stopped = 0;

            action Register when (reg == 0) {
                reg = 1;
                live = 1;
            }
            action Exit when (live == 1) {
                live = 0;
            }
            action FreeSlot when (room == 0) {
                room = 1;
            }
            action Read when (phase == 0) {
                phase = 1;
                snap_reg = reg;
                snap_live = live;
                picked = if live == 1 && room == 1 && watched == 0 { 1 } else { 0 };
            }
            action Drain when (phase == 1) {
                line = if drained == 1 {
                    0
                } else if Buggy == 1 {
                    reg
                } else if snap_reg == 0 {
                    0
                } else if snap_live == 1 && watched == 0 && picked == 0 {
                    2
                } else {
                    1
                };
                drained = if drained == 1 { 1 } else if Buggy == 1 { reg } else { snap_reg };
                phase = 2;
            }
            action Seed when (phase == 2) {
                watched = if picked == 1 { 1 } else { watched };
                phase = 3;
            }
            action Write when (phase == 3) {
                told = if line == 0 { told } else { line };
                ackwire = if picked == 1 { 1 } else { ackwire };
                picked = 0;
                line = 0;
                snap_reg = 0;
                snap_live = 0;
                phase = 0;
            }
            action Add when (live == 1 && added == 0) {
                added = 1;
                pushed = watched;
            }
            action Roster when (asking == 0) {
                listed = live;
                acked = if live == 0 { 0 } else { acked };
                stopped = if live == 0 { 0 } else { stopped };
                known = if live == 0 { 0 } else { known };
            }
            action Ask when (asking == 0 && listed == 1 && stopped == 0) {
                asking = 1;
                seen = added;
            }
            action Answer when (asking == 1) {
                asking = 0;
                known = if seen == 1 { 1 } else { known };
                stopped = if Buggy == 1 { 1 } else { acked };
                seen = 0;
            }
            action Fail when (asking == 1) {
                asking = 0;
                stopped = if Buggy == 1 { 1 } else { stopped };
                seen = 0;
            }
            action HearAck when (ackwire == 1 && asking == 0) {
                ackwire = 0;
                acked = if Buggy == 1 { acked } else { 1 };
                stopped = if Buggy == 1 { stopped } else { 0 };
            }
            action HearPush when (pushed == 1 && asking == 0) {
                pushed = 0;
                known = if listed == 1 { 1 } else { known };
            }
            action Outage when (asking == 0 && (pushed == 1 || ackwire == 1 || stopped == 1)) {
                pushed = 0;
                acked = if ackwire == 1 && Buggy == 0 { 1 } else { acked };
                ackwire = 0;
                stopped = 0;
            }
            action DropAck when (ackwire == 1) {
                ackwire = 0;
            }

            invariant NotAnnouncedBeforeItsWatchOrExit:
                told == 0 || told == 2 || watched == 1 || live == 0;
            invariant StoppedOnlyWhileWatched:
                stopped == 0 || watched == 1 || live == 0;
            invariant NoTopicAddLostSilently:
                live == 0 || added == 0 || known == 1 || pushed == 1 || stopped == 0;
        }
    }
}
