// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AmacsUndo`: labelled linear undo model under concurrent and agent edits.
//!
//! The document is two regions, A (`ta`) and B (`tb`), each holding a version
//! (a fresh one per commit, from `ver`). B sits after A, so a commit to A moves
//! B's text. The history is a stack of up to three frames, slot 1 the oldest:
//! a frame's region (`r`, 1 for A, 2 for B), origin (`o`: 1 keyboard, 2 agent,
//! 3 disk, 4 a keyboard frame that undid an agent frame), the version it
//! replaced (`old`), and the slot of the later frame blocking it (`b`, 0 for
//! none). A commit to a region blocks every unblocked frame on that region
//! (it touches the text they would put back); undoing a frame unblocks the
//! frames it blocked.
//!
//! `Undo` takes the newest frame, whoever made it. `UndoAgent` undoes the
//! newest agent frame not already undone, out of order, by pushing a frame
//! of its own that blocks it; it is refused while a later change blocks it.
//!
//! The laws read the text. Each undo step records, from the frame it takes
//! and the state before it, the region it must write (`ur`, 0 after a
//! commit), the version that region must then hold (`uv`), the version the
//! other region held (`kv`), and whether the frame was blocked (`ub`); a
//! region holding 0 is text that is no longer two regions of one version
//! each. The mutants change transitions only: `Buggy = 1` skips the rebase
//! (an out-of-order undo of a B frame under a later A commit writes at stale
//! coordinates), 2 lets `UndoAgent` take a blocked frame, 3 restores the
//! whole text from before the frame, and 4 leaves the frames an undo
//! unblocks blocked.

use aterm_spec::derive::Model;

/// Returns the `AmacsUndo` formal model (`amacs` `editor::history`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn amacs_undo_model() -> Model {
    aterm_spec::ty_model! {
        AmacsUndo {
            const Buggy = 0;
            const MaxVer = 5;
            var ta = 1;
            var tb = 2;
            var ver = 2;
            var n = 0;
            var r1 = 0;
            var o1 = 0;
            var old1 = 0;
            var b1 = 0;
            var r2 = 0;
            var o2 = 0;
            var old2 = 0;
            var b2 = 0;
            var r3 = 0;
            var o3 = 0;
            var old3 = 0;
            var b3 = 0;
            var ur = 0;
            var uv = 0;
            var kv = 0;
            var ub = 0;
            action CommitKeyA when (n <= 2 && ver <= MaxVer - 1) {
                r1 = if n == 0 { 1 } else { r1 };
                o1 = if n == 0 { 1 } else { o1 };
                old1 = if n == 0 { ta } else { old1 };
                b1 = if n > 0 && r1 == 1 && b1 == 0 { n + 1 } else { b1 };
                r2 = if n == 1 { 1 } else { r2 };
                o2 = if n == 1 { 1 } else { o2 };
                old2 = if n == 1 { ta } else { old2 };
                b2 = if n > 1 && r2 == 1 && b2 == 0 { n + 1 } else { b2 };
                r3 = if n == 2 { 1 } else { r3 };
                o3 = if n == 2 { 1 } else { o3 };
                old3 = if n == 2 { ta } else { old3 };
                ta = ver + 1;
                ver = ver + 1;
                n = n + 1;
                ur = 0;
                uv = 0;
                kv = 0;
                ub = 0;
            }
            action CommitKeyB when (n <= 2 && ver <= MaxVer - 1) {
                r1 = if n == 0 { 2 } else { r1 };
                o1 = if n == 0 { 1 } else { o1 };
                old1 = if n == 0 { tb } else { old1 };
                b1 = if n > 0 && r1 == 2 && b1 == 0 { n + 1 } else { b1 };
                r2 = if n == 1 { 2 } else { r2 };
                o2 = if n == 1 { 1 } else { o2 };
                old2 = if n == 1 { tb } else { old2 };
                b2 = if n > 1 && r2 == 2 && b2 == 0 { n + 1 } else { b2 };
                r3 = if n == 2 { 2 } else { r3 };
                o3 = if n == 2 { 1 } else { o3 };
                old3 = if n == 2 { tb } else { old3 };
                tb = ver + 1;
                ver = ver + 1;
                n = n + 1;
                ur = 0;
                uv = 0;
                kv = 0;
                ub = 0;
            }
            action CommitAgentA when (n <= 2 && ver <= MaxVer - 1) {
                r1 = if n == 0 { 1 } else { r1 };
                o1 = if n == 0 { 2 } else { o1 };
                old1 = if n == 0 { ta } else { old1 };
                b1 = if n > 0 && r1 == 1 && b1 == 0 { n + 1 } else { b1 };
                r2 = if n == 1 { 1 } else { r2 };
                o2 = if n == 1 { 2 } else { o2 };
                old2 = if n == 1 { ta } else { old2 };
                b2 = if n > 1 && r2 == 1 && b2 == 0 { n + 1 } else { b2 };
                r3 = if n == 2 { 1 } else { r3 };
                o3 = if n == 2 { 2 } else { o3 };
                old3 = if n == 2 { ta } else { old3 };
                ta = ver + 1;
                ver = ver + 1;
                n = n + 1;
                ur = 0;
                uv = 0;
                kv = 0;
                ub = 0;
            }
            action CommitAgentB when (n <= 2 && ver <= MaxVer - 1) {
                r1 = if n == 0 { 2 } else { r1 };
                o1 = if n == 0 { 2 } else { o1 };
                old1 = if n == 0 { tb } else { old1 };
                b1 = if n > 0 && r1 == 2 && b1 == 0 { n + 1 } else { b1 };
                r2 = if n == 1 { 2 } else { r2 };
                o2 = if n == 1 { 2 } else { o2 };
                old2 = if n == 1 { tb } else { old2 };
                b2 = if n > 1 && r2 == 2 && b2 == 0 { n + 1 } else { b2 };
                r3 = if n == 2 { 2 } else { r3 };
                o3 = if n == 2 { 2 } else { o3 };
                old3 = if n == 2 { tb } else { old3 };
                tb = ver + 1;
                ver = ver + 1;
                n = n + 1;
                ur = 0;
                uv = 0;
                kv = 0;
                ub = 0;
            }
            action CommitDiskA when (n <= 2 && ver <= MaxVer - 1) {
                r1 = if n == 0 { 1 } else { r1 };
                o1 = if n == 0 { 3 } else { o1 };
                old1 = if n == 0 { ta } else { old1 };
                b1 = if n > 0 && r1 == 1 && b1 == 0 { n + 1 } else { b1 };
                r2 = if n == 1 { 1 } else { r2 };
                o2 = if n == 1 { 3 } else { o2 };
                old2 = if n == 1 { ta } else { old2 };
                b2 = if n > 1 && r2 == 1 && b2 == 0 { n + 1 } else { b2 };
                r3 = if n == 2 { 1 } else { r3 };
                o3 = if n == 2 { 3 } else { o3 };
                old3 = if n == 2 { ta } else { old3 };
                ta = ver + 1;
                ver = ver + 1;
                n = n + 1;
                ur = 0;
                uv = 0;
                kv = 0;
                ub = 0;
            }
            action CommitDiskB when (n <= 2 && ver <= MaxVer - 1) {
                r1 = if n == 0 { 2 } else { r1 };
                o1 = if n == 0 { 3 } else { o1 };
                old1 = if n == 0 { tb } else { old1 };
                b1 = if n > 0 && r1 == 2 && b1 == 0 { n + 1 } else { b1 };
                r2 = if n == 1 { 2 } else { r2 };
                o2 = if n == 1 { 3 } else { o2 };
                old2 = if n == 1 { tb } else { old2 };
                b2 = if n > 1 && r2 == 2 && b2 == 0 { n + 1 } else { b2 };
                r3 = if n == 2 { 2 } else { r3 };
                o3 = if n == 2 { 3 } else { o3 };
                old3 = if n == 2 { tb } else { old3 };
                tb = ver + 1;
                ver = ver + 1;
                n = n + 1;
                ur = 0;
                uv = 0;
                kv = 0;
                ub = 0;
            }
            action UndoAt1 when (n == 1 && b1 == 0) {
                ta = if r1 == 1 { old1 } else { ta };
                tb = if r1 == 2 { old1 } else { tb };
                ur = r1;
                uv = old1;
                kv = if r1 == 1 { tb } else { ta };
                ub = if b1 == 0 { 0 } else { 1 };
                r1 = 0;
                o1 = 0;
                old1 = 0;
                n = 0;
            }
            action UndoAt2 when (n == 2 && b2 == 0) {
                ta = if r2 == 1 { old2 } else { ta };
                tb = if r2 == 2 { old2 } else { tb };
                ur = r2;
                uv = old2;
                kv = if r2 == 1 { tb } else { ta };
                ub = if b2 == 0 { 0 } else { 1 };
                b1 = if b1 == 2 && Buggy == 4 { b1 } else if b1 == 2 { 0 } else { b1 };
                r2 = 0;
                o2 = 0;
                old2 = 0;
                n = 1;
            }
            action UndoAt3 when (n == 3 && b3 == 0) {
                ta = if r3 == 1 { old3 } else { ta };
                tb = if r3 == 2 { old3 } else { tb };
                ur = r3;
                uv = old3;
                kv = if r3 == 1 { tb } else { ta };
                ub = if b3 == 0 { 0 } else { 1 };
                b1 = if b1 == 3 && Buggy == 4 { b1 } else if b1 == 3 { 0 } else { b1 };
                b2 = if b2 == 3 && Buggy == 4 { b2 } else if b2 == 3 { 0 } else { b2 };
                r3 = 0;
                o3 = 0;
                old3 = 0;
                n = 2;
            }
            action UndoAgentAt1 when (
                o1 == 2
                    && (n == 1 || (n == 2 && (o2 == 1 || o2 == 3) && (b1 == 0 || Buggy == 2)))
            ) {
                r2 = if n == 1 { r1 } else { r2 };
                o2 = if n == 1 { 4 } else { o2 };
                old2 = if n == 1 { if r1 == 1 { ta } else { tb } } else { old2 };
                r3 = if n == 2 { r1 } else { r3 };
                o3 = if n == 2 { 4 } else { o3 };
                old3 = if n == 2 { if r1 == 1 { ta } else { tb } } else { old3 };
                b1 = n + 1;
                ta = if Buggy == 1 && r1 == 2 && n == 2 && r2 == 1 {
                    0
                } else if r1 == 1 {
                    old1
                } else if Buggy == 3 && n == 2 {
                    old2
                } else {
                    ta
                };
                tb = if Buggy == 1 && r1 == 2 && n == 2 && r2 == 1 {
                    0
                } else if r1 == 2 {
                    old1
                } else if Buggy == 3 && n == 2 {
                    old2
                } else {
                    tb
                };
                ur = r1;
                uv = old1;
                kv = if r1 == 1 { tb } else { ta };
                ub = if b1 == 0 { 0 } else { 1 };
                n = n + 1;
            }
            action UndoAgentAt2 when (n == 2 && o2 == 2 && b2 == 0) {
                r3 = r2;
                o3 = 4;
                old3 = if r2 == 1 { ta } else { tb };
                b2 = 3;
                ta = if r2 == 1 { old2 } else { ta };
                tb = if r2 == 2 { old2 } else { tb };
                ur = r2;
                uv = old2;
                kv = if r2 == 1 { tb } else { ta };
                ub = if b2 == 0 { 0 } else { 1 };
                n = 3;
            }
            invariant UndoInvertsLatestUnblocked:
                ur == 0 || (ur == 1 && ta == uv) || (ur == 2 && tb == uv);
            invariant NoSilentDrop: ur == 0 || (ur == 1 && tb == kv) || (ur == 2 && ta == kv);
            invariant BlockedFrameNeverApplied: ub == 0;
            invariant RebaseKeepsDisjointFrames: ta > 0 && tb > 0;
            invariant BlockersAreLater: (b1 == 0 || (b1 > 1 && b1 <= n))
                && (b2 == 0 || (b2 > 2 && b2 <= n))
                && b3 == 0;
            bound VersionsBounded: ver <= MaxVer;
            bound StackBounded: n <= 3;
        }
    }
}
