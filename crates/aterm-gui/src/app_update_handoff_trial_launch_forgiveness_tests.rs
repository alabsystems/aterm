// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::forgives_the_counted_trial_launch;
use crate::ChildDeathEvidence as Death;

const CURRENT: u64 = 1_787_699_398;
const TARGET: u64 = 1_787_699_399;

fn forgives(death: Death) -> bool {
    forgives_the_counted_trial_launch(death, CURRENT, TARGET)
}

#[test]
fn only_a_death_the_bytes_answer_for_keeps_its_counted_trial_launch() {
    for (death, keeps_its_count, why) in [
        (
            Death::Exited { code: 0 },
            true,
            "the image reached an `exit` instruction: it ran, it decided, and \
             a launch that ended that way is the sentinel's to keep",
        ),
        (
            Death::Exited { code: 74 },
            true,
            "same reading; the code is recorded, not judged",
        ),
        (
            Death::Signalled {
                signal: libc::SIGSEGV,
            },
            true,
            "a fault is the image executing itself into a wall",
        ),
        (
            Death::Signalled {
                signal: libc::SIGKILL,
            },
            false,
            "THE FIELD CASE: macOS jetsam reclaiming memory says nothing about \
             the bytes, so the launch goes back — otherwise three busy \
             afternoons revert a healthy build",
        ),
        (
            Death::Unobserved,
            false,
            "and an unattributed death least of all: this is the majority \
             answer on the shipping lane, and it is retried six times",
        ),
        (
            Death::EndedHere,
            false,
            "nor a launch this process ended itself",
        ),
    ] {
        assert_eq!(forgives(death), !keeps_its_count, "{death:?}: {why}");
    }
}

#[test]
fn an_attempt_that_authorizes_no_newer_build_has_no_launch_to_forgive() {
    for death in [
        Death::Unobserved,
        Death::Exited { code: 0 },
        Death::Signalled {
            signal: libc::SIGKILL,
        },
    ] {
        assert!(
            !forgives_the_counted_trial_launch(death, CURRENT, CURRENT),
            "{death:?}: an installed activation is its own target, and the QA \
             seam authorizes none at all"
        );
        assert!(
            !forgives_the_counted_trial_launch(death, CURRENT, CURRENT - 1),
            "{death:?}: and an older target is never applied"
        );
    }
}
