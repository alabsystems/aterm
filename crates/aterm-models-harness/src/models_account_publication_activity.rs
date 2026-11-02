// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A committed success remains observable after implicit journal replay. A new
/// retry baseline or replaced owner excludes it; unrelated and unknown holds stay.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_publication_activity_model() -> Model {
    aterm_spec::ty_model! {
        AccountPublicationActivity {
            const Buggy = 0;
            var provider = 1; var case = 0; var phase = 0;
            var activity = 1; var ladder = 1; var foreign = 1;
            action Provider when (phase == 0 && 4 > provider) { provider = provider + 1; }
            action Case when (phase == 0 && 22 > case) { case = case + 1; }
            action Observe when (phase == 0) {
                phase = 1;
                activity = if Buggy == 1 {1} else if Buggy == 3 {0}
                    else if (Buggy == 5 && (case == 3 || case == 12)) || (Buggy == 7 && case == 4)
                        || (Buggy == 9 && case == 8) || (Buggy == 10 && case == 7) || (Buggy == 12 && case == 21) || (Buggy == 13 && case == 22) {0}
                    else if (Buggy == 6 && case == 2) || (Buggy == 8 && case == 11) || (Buggy == 11 && (case == 19 || case == 20)) {1}
                    else if case == 0 || case == 2 || case == 10 || case == 11 || case == 13 || case == 15 || case == 18 || case == 19 || case == 20 {0} else {1};
                ladder = if Buggy == 2 {1} else if Buggy == 3 {0}
                    else if (Buggy == 5 && (case == 3 || case == 12)) || (Buggy == 7 && case == 4)
                        || (Buggy == 9 && case == 8) || (Buggy == 10 && case == 7) || (Buggy == 12 && case == 21) || (Buggy == 13 && case == 22) {0}
                    else if (Buggy == 6 && case == 2) || (Buggy == 8 && case == 11) || (Buggy == 11 && (case == 19 || case == 20)) {1}
                    else if case == 0 || case == 2 || case == 10 || case == 11 || case == 14 || case == 15 || case == 18 || case == 19 || case == 20 {0} else {1};
                foreign = if Buggy == 4 {0} else {1};
            }
            invariant ActivityTracksSuccess: phase == 0 || activity ==
                if case == 0 || case == 2 || case == 10 || case == 11 || case == 13 || case == 15 || case == 18 || case == 19 || case == 20 {0} else {1};
            invariant LadderTracksSuccess: phase == 0 || ladder ==
                if case == 0 || case == 2 || case == 10 || case == 11 || case == 14 || case == 15 || case == 18 || case == 19 || case == 20 {0} else {1};
            invariant ForeignSurvives: foreign == 1;
        }
    }
}
