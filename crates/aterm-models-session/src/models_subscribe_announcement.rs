// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An `@*` subscription never tells its reader a session is watched before the watch exists.

use aterm_spec::derive::Model;

/// One session S.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn subscribe_announcement_order_model() -> Model {
    aterm_spec::ty_model! {
        SubscribeAnnouncementOrder {
            const Buggy = 0;
            var reg = 0;
            var live = 0;
            var room = 0;
            var phase = 0;
            var snap_reg = 0;
            var snap_live = 0;
            var picked = 0;
            var line = 0;
            var drained = 0;
            var watched = 0;
            var told = 0;
            var ackwire = 0;
            var added = 0;
            var pushed = 0;
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
