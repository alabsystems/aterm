// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of one join phrase in a member's book (`docs/DESIGN-fluid-swarm-2026-10-28.md`
//! §3.3.4 "Limits on phrases", F9): a phrase admits at most once, a failed confirmation burns
//! it, and an expired phrase admits nothing.

use aterm_spec::derive::*;

/// `JoinPhrase`: one phrase a member shows, and the CPace runs joiners start with it.
///
/// `phase` is the phrase's state in the book: 0 not yet shown, 1 live, 2 running (one run
/// started with it), 3 admitted, 4 burned. A run's confirmation is a guess at the phrase: the
/// member checks the joiner's tag (`guesses`), and a right tag from a live phrase admits
/// (`admits`). The clock may pass the phrase's expiry at any time (`expired`); a confirmation
/// after that burns it, and `late_admits` counts admissions that came after expiry. A live
/// phrase is cancelled before any run (its row re-allowed, ignored or closed, or its sheet never
/// opened): it burns (`Cancel`).
///
/// Mutant dials: a wrong confirmation that returns the phrase to the book instead of burning it
/// (1), an admitted phrase that is shown again (2), a confirmation that admits after expiry (3).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn join_phrase_model() -> Model {
    aterm_spec::ty_model! {
        JoinPhrase {
            const Buggy = 0;

            var phase = 0;
            var expired = 0;
            var guesses = 0;
            var admits = 0;
            var late_admits = 0;

            action Issue when (phase == 0) {
                phase = 1;
            }
            action Begin when (phase == 1 && expired == 0) {
                phase = 2;
            }
            action ConfirmRight when (phase == 2 && expired == 0) {
                phase = 3;
                guesses = guesses + 1;
                admits = admits + 1;
            }
            action ConfirmWrong when (phase == 2) {
                phase = 4;
                guesses = guesses + 1;
            }
            action ConfirmLate when (phase == 2 && expired == 1) {
                phase = 4;
                guesses = guesses + 1;
            }
            action Abort when (phase == 2) {
                phase = 4;
            }
            action Cancel when (phase == 1) {
                phase = 4;
            }
            action Expire when (phase > 0 && expired == 0) {
                expired = 1;
            }

            // Mutant dials (one defect per law):
            action WrongKeepsPhrase when (Buggy == 1 && phase == 2 && guesses <= 1) {
                phase = 1;
                guesses = guesses + 1;
            }
            action AdmittedShownAgain when (Buggy == 2 && phase == 3 && admits <= 1) {
                phase = 1;
            }
            action LateConfirmAdmits when (Buggy == 3 && phase == 2 && expired == 1 &&
                admits <= 1) {
                phase = 3;
                guesses = guesses + 1;
                admits = admits + 1;
                late_admits = late_admits + 1;
            }

            invariant AtMostOneGuess: guesses <= 1;
            invariant AtMostOneAdmit: admits <= 1;
            invariant ExpiredAdmitsNothing: late_admits == 0;
            bound GuessBox: guesses <= 3 && admits <= 2 && late_admits <= 1;
        }
    }
}
