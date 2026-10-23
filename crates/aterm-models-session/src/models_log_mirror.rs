// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! THE BUS MIRROR (design §5.5, phase 4).

use aterm_spec::derive::Model;

/// THE LOG MIRROR.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn log_mirror_model() -> Model {
    aterm_spec::ty_model! {
        LogMirror {
            const Buggy = 0;
            var head = 0;
            var e2 = 0;
            var e3 = 0;
            var landed = 0;
            var pin = 0;
            var plo = 0;
            var phi = 0;
            var pgap = 0;
            var onbus = 0;
            var foreign = 0;
            var rec1 = 0;
            var rec2 = 0;
            var rec3 = 0;
            var cov1 = 0;
            var cov2 = 0;
            var cov3 = 0;
            var fence = 0;
            var fh = 0;
            var lc = 0;
            var succ = 0;
            var adopted = 1;
            var closed = 0;

            action Append when (3 > head && closed == 0) {
                head = head + 1;
            }
            action Evict2 when (2 <= head && e2 == 0 && (fence == 0 || 2 > fh)) {
                e2 = 1;
            }
            action Evict3 when (3 <= head && e3 == 0 && (fence == 0 || 3 > fh)) {
                e3 = 1;
            }
            action TakeRecord when (pin == 0 && adopted == 1 && (if fence == 1 { fh } else { head }) > landed && (landed == 0 || (landed == 1 && e2 == 0) || (landed == 2 && e3 == 0))) {
                pin = 1;
                plo = landed + 1;
                phi = landed + 1;
                pgap = 0;
                onbus = 0;
            }
            action TakeGap when (pin == 0 && adopted == 1 && (if fence == 1 { fh } else { head }) > landed && ((landed == 1 && e2 == 1) || (landed == 2 && e3 == 1))) {
                pin = 1;
                plo = landed + 1;
                phi = if landed == 1 && e3 == 1 && 3 <= (if fence == 1 { fh } else { head }) { 3 } else { landed + 1 };
                pgap = 1;
                onbus = 0;
            }
            action Publish when (pin == 1 && onbus == 0 && foreign == 0) {
                onbus = 1;
                rec1 = if pgap == 0 && plo <= 1 && 1 <= phi { 1 } else { rec1 };
                rec2 = if pgap == 0 && plo <= 2 && 2 <= phi { 1 } else { rec2 };
                rec3 = if pgap == 0 && plo <= 3 && 3 <= phi { 1 } else { rec3 };
                cov1 = if pgap == 1 && plo <= 1 && 1 <= phi { 1 } else { cov1 };
                cov2 = if pgap == 1 && plo <= 2 && 2 <= phi { 1 } else { cov2 };
                cov3 = if pgap == 1 && plo <= 3 && 3 <= phi { 1 } else { cov3 };
            }
            action Foreign when (pin == 1 && onbus == 0 && foreign == 0) {
                foreign = 1;
            }
            action Repin when (foreign == 1) {
                foreign = 0;
            }
            action Landed when (pin == 1 && onbus == 1 && fence == 0 && adopted == 1) {
                landed = if phi > landed && phi <= head { phi } else { landed };
            }
            action Unpin when (pin == 1 && onbus == 1 && (phi <= landed || phi > head)) {
                pin = 0;
                onbus = 0;
            }
            action Export when (fence == 0 && succ == 0) {
                fence = 1;
                fh = head;
                lc = landed;
            }
            action Commit when (fence == 1 && closed == 0) {
                fence = 0;
                succ = 1;
                adopted = 0;
                head = fh;
                e2 = if 2 > fh { 0 } else { e2 };
                e3 = if 3 > fh { 0 } else { e3 };
                landed = lc;
            }
            action Rollback when (fence == 1) {
                fence = 0;
            }
            action AdoptLate when (adopted == 0) {
                adopted = 1;
            }
            action Close when (closed == 0 && adopted == 1 && 3 > head) {
                head = head + 1;
                closed = 1;
            }
            action DropUnadopted when (Buggy == 1 && adopted == 0 && pin == 1 && onbus == 1) {
                pin = 0;
                onbus = 0;
            }
            action TailPastFence when (Buggy == 1 && pin == 0 && fence == 1 && head > fh && landed == fh) {
                pin = 1;
                plo = landed + 1;
                phi = head;
                pgap = 1;
                onbus = 0;
            }
            action SkipGap when (Buggy == 1 && pin == 0 && adopted == 1 && landed == 1 && e2 == 1 && e3 == 0 && 3 <= (if fence == 1 { fh } else { head })) {
                pin = 1;
                plo = 3;
                phi = 3;
                pgap = 0;
                onbus = 0;
            }
            action AckBeforePublish when (Buggy == 1 && pin == 1 && onbus == 0 && fence == 0 && adopted == 1 && phi <= head) {
                landed = if phi > landed { phi } else { landed };
                onbus = 1;
            }
            action TrustDeduped when (Buggy == 1 && pin == 1 && onbus == 0 && foreign == 1) {
                onbus = 1;
                foreign = 0;
            }
            action LandUnderFence when (Buggy == 1 && fence == 1 && pin == 1 && onbus == 1) {
                landed = if phi > landed { phi } else { landed };
            }
            action CloseDropsTail when (Buggy == 1 && closed == 0 && adopted == 1 && 3 > head && pin == 0 && fence == 0) {
                landed = head + 1;
                head = head + 1;
                closed = 1;
            }

            invariant ReaderViewAtMostOnce: rec1 + cov1 <= 1 && rec2 + cov2 <= 1 && rec3 + cov3 <= 1;
            invariant MsDense: (rec2 + cov2 == 0 || 1 <= rec1 + cov1) && (rec3 + cov3 == 0 || 1 <= rec2 + cov2);
            invariant QuiescentMirrorComplete: pin == 1 || fence == 1 || adopted == 0 || head > landed || ((1 > head || 1 <= rec1 + cov1) && (2 > head || 1 <= rec2 + cov2) && (3 > head || 1 <= rec3 + cov3));
            invariant NoLandUnderFence: fence == 0 || landed == lc;
            bound StateBounded: head <= 3 && e2 <= 1 && e3 <= 1 && landed <= 3 && pin <= 1 && plo <= 3 && phi <= 3 && pgap <= 1 && onbus <= 1 && foreign <= 1 && rec1 <= 1 && rec2 <= 1 && rec3 <= 1 && cov1 <= 1 && cov2 <= 1 && cov3 <= 1 && fence <= 1 && fh <= 3 && lc <= 3 && succ <= 1 && adopted <= 1 && closed <= 1;
        }
    }
}
