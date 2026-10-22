// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! THE BUS MIRROR (design §5.5, phase 4): one session's bus-class records on
//! their way from the GUI's ring to the bus, through the bridge's pin, across
//! evictions, a broker that may hold another writer's record at a pinned
//! seq, and a seamless update. The laws are the READER'S VIEW:
//! a reader dedupes records by `(node, sid, ep, seq)`, so a body the bus
//! holds twice is shown once and exactly-once publication is no law; what
//! the reader must never be shown is an `ms` twice (a record and a cover of
//! it), an `ms` past a hole nobody said, or, once the mirror is quiet, a
//! record that is neither on the bus nor covered.

use aterm_spec::derive::Model;

/// THE LOG MIRROR.
///
/// One session of three bus-class records (`ms` 1..3; `ms` 1 the session's
/// open, which the ring never evicts). `head` records appended; `e2`/`e3`
/// evicted, in any order (eviction is the ring's, a set with a
/// pinned record, so a hole can sit between two live records); `landed` the
/// GUI's cursor. One batch in flight: `pin` the bridge's pin of one slot,
/// `[plo, phi]` the `ms` it covers, `pgap` whether it is a cover, `onbus`
/// whether the broker holds it at its pinned seq, `foreign` another writer's
/// record at that seq (a co-holder of the node cap). The reader's
/// view: `rec<i>` it holds `ms` i's record (deduped), `cov<i>` a cover of it
/// is on the bus. The update: `Export` fences the readers at `fh` (the `ms`
/// head) and draws the cursor `lc`; `Commit` hands the session to the
/// successor, which holds the carry (`head = fh`, the overlap's records
/// gone) and continues the drawn cursor; `succ`, and `adopted` until the
/// successor has registered the session (`AdoptLate`, until
/// then the GUI answers a take `ERR busy adopting=1` and a landing of the
/// session `adopting`, and the bridge keeps its pin).
///
/// `Close`: the session ends (fixer F1/F10), its close the last record
/// appended. Its log becomes a closed session's bus TAIL in the GUI
/// (`session_log_mirror`), taken and landed as the live log was — under the
/// export's fence too, which the update's stand-down lifts — and nothing
/// more is appended. A session that closes
/// while an update is in flight stands that update down (its `Commit` needs
/// every session it drew, `exact_sessions`): `Commit` needs `closed == 0`.
///
/// A take reads up to the visible head (`fh` while fenced): `TakeRecord` the
/// next record when it is live, `TakeGap` the maximal evicted run from
/// `landed + 1` up to the next live `ms` or the visible head. `Publish`
/// sends the slot at its pinned seq, unless another writer's record holds
/// it, which the checked dedup finds (`Repin`, a fresh seq). `Landed` moves
/// the cursor only unfenced and adopted, never past the head (a cursor past
/// it is refused and the slot dropped, `Unpin`).
///
/// `Buggy = 1` adds one dead mutant per law, each with the real call it
/// stands for (Tier-1, `aterm-link`'s `log_mirror_conforms_to_derived_model`):
/// - `DropUnadopted` (`ReaderViewAtMostOnce`): the bridge drops its pin on
///   `ERR no such session` from a successor that has not adopted the
///   session; the successor's carried cursor is behind the pin, and what its
///   carry shed of the pinned records it then covers: a record and a cover
///   of one `ms`.
/// - `TailPastFence` (`ReaderViewAtMostOnce`): a take under the fence whose
///   tail cover reaches the LIVE `ms` head, not the fence's: it covers `ms`
///   the successor numbers again.
/// - `SkipGap` (`MsDense`): a take of the next live record past a hole with
///   no cover before it.
/// - `AckBeforePublish` (`QuiescentMirrorComplete`): `log-landed` before the
///   slot's ack.
/// - `TrustDeduped` (`QuiescentMirrorComplete`): a deduped answer to the
///   pinned seq taken as ours without reading the record there.
/// - `LandUnderFence` (`NoLandUnderFence`): `log-landed` answered `OK` while
///   the export's cursor stands.
/// - `CloseDropsTail` (`QuiescentMirrorComplete`): a session that closes with
///   records the bus does not hold keeps no tail, so nothing more is owed of
///   it (`aterm-gui`'s Tier-1, `a_closed_sessions_tail_conforms_to_the_model`).
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
