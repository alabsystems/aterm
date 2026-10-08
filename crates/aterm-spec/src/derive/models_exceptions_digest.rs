// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The exceptions digest never asks for a band row, never loses an entry, and
//! never lets a dismissal be undone by the same fact.

use super::Model;

/// `open` exceptions stand in the digest (`MessageCenter::digest`); `glass` is
/// what the band WANTS (`MessageCenter::wanted_rows`); `now_rows` is the ghost
/// of what a Working, Notice or Now message legitimately earns. The incident
/// that started this work was a band row that appeared and folded 0.7 ms apart
/// and re-gridded every tab of the window, so the first invariant is the one
/// with a death behind it: `LaterNeverWantsARow` (`glass == now_rows`).
/// `PostLater` files nothing on the band; `PostLaterAtCap` files the oldest
/// entry with its log line (`filed`, saturating: the check needs a finite space); `Cap` is `DIGEST_CAP`, so the cap loses nothing; `FileAll` is
/// "Dismiss all", which quiets the facts it filed until `Resurface` (their
/// span passing); `Suppress` is a dismissed fact raised again inside its span.
/// `Buggy=1` adds four mutants, one per invariant: a `Later` seated on the
/// band (the incident), a re-post that goes through a dismissal, a cap that
/// drops an entry without its line, and an over-cap post.
/// Tier-1 drives the shipping `MessageCenter` in lock step.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exceptions_digest_model() -> Model {
    crate::ty_model! {
        ExceptionsDigest {
            const Buggy = 0;
            const Cap = 12;
            var open = 0;
            var glass = 0;
            var now_rows = 0;
            var quiet = 0;
            var filed = 0;
            var lost = 0;

            action PostNow when (now_rows <= 2) {
                now_rows = now_rows + 1;
                glass = glass + 1;
            }
            action FoldNow when (now_rows > 0) {
                now_rows = now_rows - 1;
                glass = glass - 1;
            }
            action PostLater when (quiet == 0 && open <= Cap - 1) {
                open = open + 1;
            }
            action PostLaterAtCap when (quiet == 0 && open > Cap - 1) {
                filed = if filed <= Cap - 1 { filed + 1 } else { filed };
            }
            action Suppress when (quiet == 1) {
                filed = filed;
            }
            action FileAll when (open > 0) {
                filed = if filed + open <= Cap { filed + open } else { Cap };
                open = 0;
                quiet = 1;
            }
            action Resurface when (quiet == 1) {
                quiet = 0;
            }
            action PostLaterSeated when (Buggy == 1 && glass == now_rows && quiet == 0 && open <= Cap - 1 && now_rows <= 2) {
                open = open + 1;
                glass = glass + 1;
            }
            action PostThroughQuiet when (Buggy == 1 && quiet == 1 && open <= Cap - 1) {
                open = open + 1;
            }
            action CapDrops when (Buggy == 1 && quiet == 0 && open > Cap - 1 && lost == 0) {
                lost = lost + 1;
            }
            action OverCap when (Buggy == 1 && quiet == 0 && open <= Cap) {
                open = open + 1;
            }

            invariant LaterNeverWantsARow: glass == now_rows;
            invariant DismissalHolds: quiet == 0 || open == 0;
            invariant NothingIsLost: lost == 0;
            invariant OpenIsBounded: open <= Cap;
        }
    }
}
