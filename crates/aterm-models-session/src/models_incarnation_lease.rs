// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE INCARNATION LEASE (`aterm-link`'s `state.rs`, "The incarnation lease";
//! `StateDir::take_incarnation`): how an attach takes the incarnation its will and
//! every sequence of it run under, with the durable write of `inc` done once per
//! lease of `Span` incarnations instead of once per attach, and the fence still
//! holding across a `kill -9` at every step and a power loss at every step.
//!
//! Three files, each with a PAGE-CACHE value (`c_`, what any reader in this boot
//! reads) and a MEDIA value (`m_`, what a power loss leaves). `inc` (`c_inc`,
//! `m_inc`): the lease's top. `inc-lease` (`c_used`, `c_top`, `c_lb`): the
//! incarnation the last attach took, the top it took it under, and the boot that
//! wrote it (`c_lb = 0`: no lease file). `seq` (`c_seq`): the incarnation of the
//! newest reserved sequence. The OS writes any file back at any moment
//! (`WritebackInc`, `WritebackLease`, `WritebackSeq`), so after `PowerLoss` each
//! file holds some version it had — the newest one written back — and the boot
//! moves on. A flushed write moves `inc`'s media value itself.
//!
//! One take is `Plan` (read the three files, decide: `tr` whether the lease is
//! trusted, `pn` the incarnation taken), then exactly one of `Take` (no flush: a
//! trusted lease with room), `TakeFlushed` (lease afresh: `inc` flushed, then
//! `inc-lease` written), `KillRenamed` (killed after the flush's rename, before its
//! directory flush: the new top in the page cache only) or `KillFlushed` (killed
//! after the flush returned, before `inc-lease`), then `Present` (the will goes out:
//! `hi`, the highest incarnation ever presented, moves) or `Kill` (the process dies
//! between the take and the will). `OldAttach` is an older build's attach on the
//! same dir: `inc + 1`, flushed, presented.
//!
//! Invariants. `LeaseDurable`: a lease this boot wrote names a top the media holds.
//! `Durable`: every presented incarnation is at or below the media's `inc`.
//! `Fence`: no attach ever presents an incarnation that was presented before
//! (`dup`), the property the will fence and the sequence space rest on.
//!
//! `Buggy`, one defect each, each the real code's mutant of the same number
//! (`IncSeam::mutant`) except 4: 1 a lease trusted across boots · 2 a lease trusted
//! whose top is not `inc`'s · 3 `inc-lease` written before its top is flushed ·
//! 4 the will sent while the flush is in flight (the overlap the cost study proposed:
//! `inc`'s media value does not move with the take).
//!
//! The lease also names the `inc` FILE its top was flushed into (`IncFile`), so a
//! copy of the state dir, whose `inc` nobody flushed, trusts no lease. This model is
//! one dir, whose `inc` is always the file every lease was flushed into, so it leaves
//! the file out; the copy is `state.rs`'s own test
//! (`a_copied_state_dir_flushes_its_own_inc_before_trusting_a_lease`), as is the
//! doctor's incarnation, which takes none and never names a lease's top
//! (`the_doctor_never_answers_a_lost_leases_unused_top`).
//!
//! Tier-0 is `tests/derived_incarnation_lease.rs`; Tier-1
//! (`aterm-link/src/state.rs`, `the_incarnation_lease_refines_its_model`) runs the
//! real `take_incarnation` from every planned state this model reaches, killed at
//! each step, and requires the files it leaves to be the model's successor.

use aterm_spec::derive::Model;

/// The incarnation lease across kills, power losses, write-backs and an older
/// build's attach (module doc). `Span` is the lease a flush covers (`INC_LEASE` is
/// 64; 2 runs a lease out inside the bound), `MaxTop` bounds the incarnations,
/// `MaxBoot` the power losses.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn incarnation_lease_model() -> Model {
    aterm_spec::ty_model! {
        IncarnationLease {
            const Buggy = 0;
            const Span = 2;
            const MaxTop = 5;
            const MaxBoot = 3;
            var boot = 1;
            var c_inc = 0;
            var m_inc = 0;
            var c_used = 0;
            var c_top = 0;
            var c_lb = 0;
            var m_used = 0;
            var m_top = 0;
            var m_lb = 0;
            var c_seq = 0;
            var m_seq = 0;
            var pc = 0;
            var tr = 0;
            var pn = 0;
            var hi = 0;
            var dup = 0;

            action Plan when (pc == 0) {
                tr = if (c_lb == boot || (Buggy == 1 && c_lb > 0))
                    && (c_top == c_inc || Buggy == 2) { 1 } else { 0 };
                pn = (if (c_lb == boot || (Buggy == 1 && c_lb > 0))
                    && (c_top == c_inc || Buggy == 2) {
                    if c_seq <= c_used { c_used } else { c_seq }
                } else {
                    if c_seq <= c_inc { c_inc } else { c_seq }
                }) + 1;
                pc = 1;
            }
            action Take when (pc == 1 && tr == 1 && pn <= c_inc) {
                c_used = pn;
                c_top = c_inc;
                c_lb = boot;
                pc = 2;
            }
            action TakeFlushed when (pc == 1 && (tr == 0 || pn > c_inc)
                && pn + Span - 1 <= MaxTop) {
                c_inc = pn + Span - 1;
                m_inc = if Buggy == 4 { m_inc } else { pn + Span - 1 };
                c_used = pn;
                c_top = pn + Span - 1;
                c_lb = boot;
                pc = 2;
            }
            action KillRenamed when (pc == 1 && (tr == 0 || pn > c_inc)
                && pn + Span - 1 <= MaxTop) {
                c_inc = pn + Span - 1;
                c_used = if Buggy == 3 { pn } else { c_used };
                c_top = if Buggy == 3 { pn + Span - 1 } else { c_top };
                c_lb = if Buggy == 3 { boot } else { c_lb };
                pc = 0;
            }
            action KillFlushed when (pc == 1 && (tr == 0 || pn > c_inc)
                && pn + Span - 1 <= MaxTop) {
                c_inc = pn + Span - 1;
                m_inc = pn + Span - 1;
                c_used = if Buggy == 3 { pn } else { c_used };
                c_top = if Buggy == 3 { pn + Span - 1 } else { c_top };
                c_lb = if Buggy == 3 { boot } else { c_lb };
                pc = 0;
            }
            action Present when (pc == 2) {
                dup = if pn <= hi { 1 } else { dup };
                hi = if pn > hi { pn } else { hi };
                c_seq = pn;
                pc = 0;
            }
            action Kill when (pc > 0) {
                pc = 0;
            }
            action WritebackInc when (c_inc > m_inc) {
                m_inc = c_inc;
            }
            action WritebackLease when (c_used > m_used || m_used > c_used || c_top > m_top
                || m_top > c_top || c_lb > m_lb || m_lb > c_lb) {
                m_used = c_used;
                m_top = c_top;
                m_lb = c_lb;
            }
            action WritebackSeq when (c_seq > m_seq || m_seq > c_seq) {
                m_seq = c_seq;
            }
            action PowerLoss when (boot + 1 <= MaxBoot) {
                boot = boot + 1;
                c_inc = m_inc;
                c_used = m_used;
                c_top = m_top;
                c_lb = m_lb;
                c_seq = m_seq;
                pc = 0;
            }
            action OldAttach when (pc == 0 && c_inc + 1 <= MaxTop) {
                c_inc = c_inc + 1;
                m_inc = c_inc + 1;
                dup = if c_inc + 1 <= hi { 1 } else { dup };
                hi = if c_inc + 1 > hi { c_inc + 1 } else { hi };
                c_seq = c_inc + 1;
            }

            invariant LeaseDurable: boot > c_lb || c_top <= m_inc;
            invariant Durable: hi <= m_inc;
            invariant Fence: dup == 0;
            bound BootBounded: boot <= MaxBoot;
            bound IncBounded: c_inc <= MaxTop;
            bound PresentedBounded: hi <= MaxTop;
        }
    }
}
