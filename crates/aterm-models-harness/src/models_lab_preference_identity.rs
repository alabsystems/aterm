// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two distinct machine keys. Zero `me` means no live facts binding; zero `owner`
/// means no preference. Each display label is absent, m3 or m4, independently of
/// keys. `rank_in` is unranked, rank 1 or rank 9. An account can be joinable or held.
///
/// One snapshot projection only: keys decide preference, enrollment and its rank.
/// No claim about credential authority, replication fairness or provider effects.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn lab_preference_identity_model() -> Model {
    aterm_spec::ty_model! {
        LabPreferenceIdentity {
            const Buggy = 0;
            var me = 0; var owner = 0;
            var label_a = 0; var label_b = 0;
            var joinable = 0; var rank_in = 0;
            var matched = 0; var enrolled = 0; var rank_out = 0;
            var phase = 0;

            action Bind when (phase == 0 && 2 > me) { me = me + 1; }
            action Prefer when (phase == 0 && 2 > owner) { owner = owner + 1; }
            action LabelA when (phase == 0 && 2 > label_a) { label_a = label_a + 1; }
            action LabelB when (phase == 0 && 2 > label_b) { label_b = label_b + 1; }
            action Joinable when (phase == 0 && joinable == 0) { joinable = 1; }
            action Rank when (phase == 0 && 2 > rank_in) { rank_in = rank_in + 1; }

            action Project when (phase == 0) {
                matched = if Buggy == 1 {
                    if me > 0 && owner > 0 && (me == owner || label_a == label_b)
                        && ((me == 1 && label_a > 0) || (me == 2 && label_b > 0))
                        && ((owner == 1 && label_a > 0) || (owner == 2 && label_b > 0))
                        {1} else {0}
                } else if Buggy == 2 {
                    if me > 0 && me == owner
                        && ((me == 1 && label_a > 0) || (me == 2 && label_b > 0))
                        {1} else {0}
                } else if Buggy == 3 {
                    if me == owner {1} else {0}
                } else {
                    if me > 0 && me == owner {1} else {0}
                };
                enrolled = if Buggy == 4 {joinable}
                    else if joinable == 1 && me > 0 && me == owner {1} else {0};
                rank_out = if me > 0 && me == owner {
                    if Buggy == 5 {1} else {rank_in + 1}
                } else {0};
                phase = 1;
            }

            invariant KeysDecide: phase == 0
                || (me > 0 && me == owner && matched == 1)
                || ((me == 0 || me > owner || owner > me) && matched == 0);
            invariant Enrollment: phase == 0
                || (joinable == 1 && me > 0 && me == owner && enrolled == 1)
                || ((joinable == 0 || me == 0 || me > owner || owner > me) && enrolled == 0);
            invariant RankIdentity: phase == 0
                || (me > 0 && me == owner && rank_out == rank_in + 1)
                || ((me == 0 || me > owner || owner > me) && rank_out == 0);
            bound ProjectionBox: me <= 2 && owner <= 2 && label_a <= 2
                && label_b <= 2 && joinable <= 1 && rank_in <= 2
                && matched <= 1 && enrolled <= 1 && rank_out <= 3 && phase <= 1;
        }
    }
}
