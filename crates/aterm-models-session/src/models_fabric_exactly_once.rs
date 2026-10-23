// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EXACTLY ONCE ACROSS THE BRIDGE'S ROUND (`aterm-link`'s `bridge.rs`: the inbox
//! group drained into `deliver`, committed only once a record is accounted for;
//! aterm-gui-fabric's `cmd_deliver`, which takes one row per offset on the log it was
//! told, and `cmd_link_log`, which forgets the offsets a move took): one OFFSET of
//! this node's inbox lane, `O`, followed through every record that sits there, every
//! delivery, commit, kill and relaunch of the bridge, and every move of the log's
//! generation.

use aterm_spec::derive::Model;

/// Exactly once across the bridge's round, at one inbox offset (module doc).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_exactly_once_model() -> Model {
    aterm_spec::ty_model! {
        FabricExactlyOnce {
            const Buggy = 0;
            const O = 1;
            const MaxG = 2;
            const MaxRows = 3;
            var occ = 0;
            var lost = 0;
            var g = 0;
            var c = 0;
            var pend = 0;
            var alive = 1;
            var dd = 0;
            var nr = 0;
            var nr2 = 0;
            var told = 1;
            var mv = 0;

            action Publish when (occ == 0 && lost == 0 && nr == 0) {
                occ = 1;
            }
            action Reuse when (occ == 0 && lost == 1 && nr2 == 0) {
                occ = 2;
            }
            action Deliver when (alive == 1 && (told == 1 || occ == 1) && occ > 0 && O + 1 > c && pend == 0 && MaxRows > nr + nr2) {
                nr = if occ == 1 && (dd == 0 || Buggy == 1) { nr + 1 } else { nr };
                nr2 = if occ == 2 && (dd == 0 || Buggy == 1) { nr2 + 1 } else { nr2 };
                dd = 1;
                pend = 1;
            }
            action DeliverTaken when (alive == 1 && told == 0 && mv == 1 && lost == 1 && pend == 0 && MaxRows > nr + nr2) {
                nr = if dd == 0 || Buggy == 1 { nr + 1 } else { nr };
                dd = 1;
                pend = 1;
            }
            action Refill when (alive == 1 && told == 1 && occ > 0 && MaxRows > nr + nr2) {
                nr = if occ == 1 && (dd == 0 || Buggy == 1) { nr + 1 } else { nr };
                nr2 = if occ == 2 && (dd == 0 || Buggy == 1) { nr2 + 1 } else { nr2 };
                dd = 1;
            }
            action Commit when (alive == 1 && told == 1 && pend == 1) {
                c = O + 1;
                pend = 0;
            }
            action CommitEarly when (Buggy == 2 && alive == 1 && told == 1 && occ > 0 && O + 1 > c && pend == 0) {
                c = O + 1;
            }
            action Kill when (alive == 1) {
                alive = 0;
                pend = 0;
            }
            action Relaunch when (alive == 0) {
                alive = 1;
            }
            action MoveTakes when (occ == 1 && MaxG > g) {
                occ = 0;
                lost = 1;
                g = g + 1;
                c = if c <= O { c } else { O };
                told = 0;
                mv = 1;
            }
            action MoveKeeps when (occ + lost > 0 && MaxG > g) {
                g = g + 1;
                told = 0;
                mv = if mv == 1 { 1 } else { 2 };
            }
            action MoveKeepsUncommits when (occ > 0 && MaxG > g && c > O) {
                g = g + 1;
                c = O;
                told = 0;
                mv = if mv == 1 { 1 } else { 2 };
            }
            action Tell when (alive == 1 && told == 0) {
                dd = if mv == 1 {
                    if Buggy == 3 { dd } else { 0 }
                } else {
                    if Buggy == 4 { 0 } else { dd }
                };
                pend = if mv == 1 {
                    if Buggy == 5 { pend } else { 0 }
                } else {
                    pend
                };
                told = 1;
                mv = 0;
            }

            invariant NoDuplicate: nr <= 1 && nr2 <= 1;
            invariant NoSkip: O + 1 > c || occ == 0 || (occ == 1 && nr > 0) || (occ == 2 && nr2 > 0);
            bound MovesBounded: g <= MaxG;
            bound CursorBounded: c <= O + 1;
            bound RowsBounded: nr <= MaxRows;
            bound RowsTwoBounded: nr2 <= MaxRows;
        }
    }
}
