// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE READER-RESUME LAW (`aterm-link`'s `state.rs`, `LogWitness::continues` and
//! `StateDir::forget_log_offsets`; `crate::durability`'s module docs): where a reader
//! that kept offsets into the bus log resumes after the broker MOVED the log's
//! generation — a power loss under a weak tier, a torn batch cut, a repair's cut — so
//! that it never skips a record it did not read, across any number of moves.
//!
//! The broker (`g`, its generation; `h`, its head) takes `Write`s. A power loss is
//! `Crash`, then any number of `Lose` (the tail the device never held), then `Bump`:
//! the next open moves the generation by one and records the LINEAGE step
//! `(g + 1, h)` — offsets below the recovered head name the same records under both
//! generations. astream keeps the newest `W` steps, and adding one CLAMPS every older
//! step's common to the new one (`Lineage::step`): the log agrees with an older
//! generation at most as far as it agrees with the newest. Here `W = 2`, two slots:
//! `f1` the common of the step to `g`, `f2` the common of the step to `g - 1` (`Top`:
//! no such step).
//!
//! The reader holds `c` (the next offset it reads) under generation `rg`, and `Read`s
//! forward while its generation is the broker's. After a move it `Decide`s where to
//! resume (`r`) and `Resume`s there (`c = r`, `rg = g`). The lineage says where its
//! generation continues — the minimum common over the steps from `rg + 1` to `g`
//! (`LogIdentity::continues`), or nothing when one of those steps is outside the
//! window — and it resumes at the lower of its cursor and that point, or, told
//! nothing, re-bases at the head as it does for ANOTHER log.
//!
//! `k` is the TRUTH the harness keeps and no reader sees: the lowest offset any move
//! since the reader's last resume cut back to — every record below `k` is one the
//! reader's offsets already account for, every record at or above it may be one it
//! never read.
//!
//! Invariants, both read off the resume point `r` the reader's `Decide` chose (the
//! harness's `Resume` records a violation; `Buggy` appears in `Decide` alone, so no
//! invariant can hold or fail by naming a defect). `NoSkipInWindow`: a resume whose
//! generation is within the window never lands past `k` (`skip`), so no record posted
//! after a fork is skipped. `AnotherLogOutsideWindow`: a resume outside the window
//! re-bases at the head (`other` records one that did not) — the steps that would bound
//! a continuation are gone, so the reader treats the log as another one, exactly as
//! before lineage, and claims no cursor of its own.
//!
//! `Buggy`, one defect each: 1 resume at the head (the rule before lineage: every move
//! is another log, and the records posted between the restart and the resume are
//! skipped) · 2 resume at the NEWEST fork only — the clamp a hand-written reader gets
//! wrong across two moves, when the older one cut lower · 3 a continuation trusted
//! beyond the window (the oldest step it would need has fallen off).
//!
//! Tier-0 is the registry sweep (`aterm-models`' `non_vacuity`): the model holds at its
//! committed configuration and each law is broken by a member of its mutant family.
//! Tier-1 (`aterm-link/src/state.rs`, `the_reader_resume_refines_its_model`) builds
//! each reachable state's lineage through astream's own `Hello` parser, runs the real
//! `LogWitness::continues` and `StateDir::forget_log_offsets` from it, and requires the
//! cursor they leave to be the `r` of the model's `Decide` successor, applied by its
//! `Resume` — with the reader that re-bases at the head as the negative control the
//! model must reject. Tier-1's lineages have at most two steps (the model's window);
//! astream's own window of `astream_broker::Lineage::MAX` steps is run end to end on a
//! real broker log by `aterm-link`'s `durability.rs`,
//! `a_reader_older_than_the_lineage_window_forgets`.

use aterm_spec::derive::Model;

/// The reader-resume law across power losses and generation moves (module doc).
/// `MaxH` bounds the head, `MaxG` the moves, `W` is the lineage window (astream's
/// `Lineage::MAX` is 8; two slots reach every shape a longer window has: a step in
/// it, a step clamped by a newer one, a step fallen off), `Top` is "no fork".
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn fabric_reader_resume_model() -> Model {
    aterm_spec::ty_model! {
        FabricReaderResume {
            const Buggy = 0;
            const MaxH = 3;
            const MaxG = 3;
            const W = 2;
            const Top = 4;
            var g = 0;
            var h = 0;
            var f1 = 4;
            var f2 = 4;
            var c = 0;
            var rg = 0;
            var k = 4;
            var down = 0;
            var pc = 0;
            var r = 0;
            var skip = 0;
            var other = 0;

            action Write when (pc == 0 && down == 0 && h + 1 <= MaxH) {
                h = h + 1;
            }
            action Read when (pc == 0 && down == 0 && rg == g && c + 1 <= h) {
                c = c + 1;
            }
            action Crash when (pc == 0 && down == 0 && g + 1 <= MaxG) {
                down = 1;
            }
            action Lose when (down == 1 && h > 0) {
                h = h - 1;
            }
            action Bump when (down == 1) {
                g = g + 1;
                f1 = h;
                f2 = if f1 == Top { Top } else { if f1 <= h { f1 } else { h } };
                k = if k <= h { k } else { h };
                down = 0;
            }
            action Decide when (pc == 0 && down == 0 && rg + 1 <= g) {
                r = if Buggy == 1 {
                    h
                } else {
                    if g - rg <= W {
                        if Buggy == 2 || g - rg == 1 {
                            if c <= f1 { c } else { f1 }
                        } else {
                            if c <= f1 && c <= f2 { c } else { if f1 <= f2 { f1 } else { f2 } }
                        }
                    } else {
                        if Buggy == 3 {
                            if c <= f2 { c } else { f2 }
                        } else {
                            h
                        }
                    }
                };
                pc = 1;
            }
            action Resume when (pc == 1) {
                c = r;
                rg = g;
                k = Top;
                skip = if g - rg <= W && r > k { 1 } else { skip };
                other = if g - rg > W { if r == h { other } else { 1 } } else { other };
                pc = 0;
            }

            invariant NoSkipInWindow: skip == 0;
            invariant AnotherLogOutsideWindow: other == 0;
            bound GenerationBounded: g <= MaxG;
            bound HeadBounded: h <= MaxH;
            bound CursorBounded: c <= MaxH;
        }
    }
}
