// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Incarnation lease model across kills, power losses, and write-backs.

use aterm_spec::derive::Model;

/// Build the incarnation lease model across kills, power losses, and write-backs.
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
