// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A retired row returns after observing a replacement; carries preserve the new tenure.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn alab_claim_revival_model() -> Model {
    aterm_spec::ty_model! {
        AlabClaimRevival {
            const Buggy = 0; const Mode = 0;
            scenario Mode = 1;
            scenario Mode = 2;
            var mode = 0; var read = 0; var shown = 0; var tenure = 0;
            action Judge when (read == 0) {
                mode = Mode; read = 1;
                shown = if Buggy == 1 {1}
                    else {if Mode > 0 && Buggy == 2 {0} else {2}};
                tenure = if Buggy == 1 {10}
                    else {if Mode > 0 && Buggy == 2 {0} else {5}};
            }
            invariant ReplacementKeepsClaim: read == 0 || shown == 2;
            invariant RevivalStartsFreshTenure: read == 0 || tenure == 5;
            bound Modes: 0 <= mode && mode <= 2;
        }
    }
}

/// Undo restores a value after withdrawal; carrying its beforeimage never revives its tenure.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn alab_claim_undo_model() -> Model {
    aterm_spec::ty_model! {
        AlabClaimUndo {
            const Buggy = 0; const Mode = 0;
            scenario Mode = 1;
            scenario Mode = 2;
            var mode = 0; var read = 0; var shown = 0; var tenure = 0;
            action Judge when (read == 0) {
                mode = Mode; read = 1;
                shown = if Mode > 0 && Buggy == 1 {1}
                    else {if Mode == 2 && Buggy == 2 {0} else {2}};
                tenure = if Mode > 0 && Buggy == 1 {10}
                    else {if Mode == 2 && Buggy == 2 {0} else {5}};
            }
            invariant IncumbentKeepsClaim: read == 0 || shown == 2;
            invariant UndoStartsFreshTenure: read == 0 || tenure == 5;
            bound Modes: 0 <= mode && mode <= 2;
        }
    }
}

// Graph, digest order and read mode are constant environment inputs, not source transitions.
macro_rules! claim_model {
    ($(($ba:literal, $ca:literal, $cb:literal, $order:literal, $mode:literal)),* $(,)?) => {
        aterm_spec::ty_model! {
        AlabIdentityClaims {
            const Buggy = 0;
            const BA = 0; const CA = 0; const CB = 0; const Order = 0; const Mode = 0;
            $(scenario BA = $ba, CA = $ca, CB = $cb, Order = $order, Mode = $mode;)*
            var ba = 0; var ca = 0; var cb = 0;
            var order = 0; var mode = 0; var read = 0;
            var shown = 0; var expected = 0; var tenure = 0; var accepted = 0; var holders = 0;

            action Judge when (read == 0) {
                ba = BA; ca = CA; cb = CB; order = Order; mode = Mode;
                read = 1;
                accepted = if Mode == 5 {if Buggy == 5 {1} else {0}}
                    else {if Mode == 6 {if Buggy == 7 {1} else {0}} else {1}};
                holders = if Mode > 4 || (Mode == 4 && (4 > Buggy || Buggy > 4)) || (Mode == 1 && Buggy == 1) {0}
                    else {if Buggy == 6 {3} else {1}};
                tenure = if Mode == 3 {5} else {
                    if Mode == 2 && Buggy == 2 {40} else {
                        if Mode > 3 {0} else {
                            if Order <= 1 {10} else {if Order == 2 || Order == 4 {20} else {30}}
                        }
                    }
                };
                expected = if Mode == 3 {
                    if CB == 1 || Order == 0 || Order == 2 || Order == 3 {2} else {3}
                } else {
                    if Order <= 1 {1} else {
                        if Order == 2 {if BA == 0 {2} else {1}} else {
                            if Order == 3 {if BA == 0 {2} else {if CA == 0 && CB == 0 {3} else {1}}} else {
                                if Order == 4 {if CA == 0 && CB == 0 {3} else {1}} else {
                                    if CA == 0 && CB == 0 {3} else {if BA == 0 {2} else {1}}
                                }
                            }
                        }
                    }
                };
                shown = if Mode > 4 || (Mode == 4 && (4 > Buggy || Buggy > 4)) || (Mode == 1 && Buggy == 1) {0} else {
                    if Buggy == 3 || (Mode == 4 && Buggy == 4) {
                        if Order <= 1 {1} else {if Order == 2 || Order == 3 {2} else {3}}
                    } else {
                        if Mode == 3 {
                            if CB == 1 || Order == 0 || Order == 2 || Order == 3 {2} else {3}
                        } else {
                            if Order <= 1 {1} else {
                                if Order == 2 {if BA == 0 {2} else {1}} else {
                                    if Order == 3 {if BA == 0 {2} else {if CA == 0 && CB == 0 {3} else {1}}} else {
                                        if Order == 4 {if CA == 0 && CB == 0 {3} else {1}} else {
                                            if CA == 0 && CB == 0 {3} else {if BA == 0 {2} else {1}}
                                        }
                                    }
                                }
                            }
                        }
                    }
                };
            }

            invariant CausalWinner: read == 0 || mode > 3 || shown == expected;
            invariant UniqueExposure: holders <= 1;
            invariant SameValueKeepsTenure: read == 0 || 2 > mode || mode > 2
                || tenure == (if order <= 1 {10} else {if order == 2 || order == 4 {20} else {30}});
            invariant MissingHistoryWithholds: read == 0 || 4 > mode || mode > 4 || shown == 0;
            invariant RetirementHasUsableSource: read == 0 || 5 > mode || accepted == 0;
            bound Cases: 0 <= order && order <= 5 && 0 <= mode && mode <= 6;
        }
    }
    };
}

/// Every dependency subset, digest ordering and native/carried/tenure/source-read mode.
/// Only judging the held claims is a source transition; scenarios exhaust the input product.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn alab_identity_claims_model() -> Model {
    claim_model! {
        (0, 0, 0, 0, 1),
        (0, 0, 0, 0, 2),
        (0, 0, 0, 0, 3),
        (0, 0, 0, 0, 4),
        (0, 0, 0, 0, 5),
        (0, 0, 0, 0, 6),
        (0, 0, 0, 1, 0),
        (0, 0, 0, 1, 1),
        (0, 0, 0, 1, 2),
        (0, 0, 0, 1, 3),
        (0, 0, 0, 1, 4),
        (0, 0, 0, 1, 5),
        (0, 0, 0, 1, 6),
        (0, 0, 0, 2, 0),
        (0, 0, 0, 2, 1),
        (0, 0, 0, 2, 2),
        (0, 0, 0, 2, 3),
        (0, 0, 0, 2, 4),
        (0, 0, 0, 2, 5),
        (0, 0, 0, 2, 6),
        (0, 0, 0, 3, 0),
        (0, 0, 0, 3, 1),
        (0, 0, 0, 3, 2),
        (0, 0, 0, 3, 3),
        (0, 0, 0, 3, 4),
        (0, 0, 0, 3, 5),
        (0, 0, 0, 3, 6),
        (0, 0, 0, 4, 0),
        (0, 0, 0, 4, 1),
        (0, 0, 0, 4, 2),
        (0, 0, 0, 4, 3),
        (0, 0, 0, 4, 4),
        (0, 0, 0, 4, 5),
        (0, 0, 0, 4, 6),
        (0, 0, 0, 5, 0),
        (0, 0, 0, 5, 1),
        (0, 0, 0, 5, 2),
        (0, 0, 0, 5, 3),
        (0, 0, 0, 5, 4),
        (0, 0, 0, 5, 5),
        (0, 0, 0, 5, 6),
        (0, 0, 1, 0, 0),
        (0, 0, 1, 0, 1),
        (0, 0, 1, 0, 2),
        (0, 0, 1, 0, 3),
        (0, 0, 1, 0, 4),
        (0, 0, 1, 0, 5),
        (0, 0, 1, 0, 6),
        (0, 0, 1, 1, 0),
        (0, 0, 1, 1, 1),
        (0, 0, 1, 1, 2),
        (0, 0, 1, 1, 3),
        (0, 0, 1, 1, 4),
        (0, 0, 1, 1, 5),
        (0, 0, 1, 1, 6),
        (0, 0, 1, 2, 0),
        (0, 0, 1, 2, 1),
        (0, 0, 1, 2, 2),
        (0, 0, 1, 2, 3),
        (0, 0, 1, 2, 4),
        (0, 0, 1, 2, 5),
        (0, 0, 1, 2, 6),
        (0, 0, 1, 3, 0),
        (0, 0, 1, 3, 1),
        (0, 0, 1, 3, 2),
        (0, 0, 1, 3, 3),
        (0, 0, 1, 3, 4),
        (0, 0, 1, 3, 5),
        (0, 0, 1, 3, 6),
        (0, 0, 1, 4, 0),
        (0, 0, 1, 4, 1),
        (0, 0, 1, 4, 2),
        (0, 0, 1, 4, 3),
        (0, 0, 1, 4, 4),
        (0, 0, 1, 4, 5),
        (0, 0, 1, 4, 6),
        (0, 0, 1, 5, 0),
        (0, 0, 1, 5, 1),
        (0, 0, 1, 5, 2),
        (0, 0, 1, 5, 3),
        (0, 0, 1, 5, 4),
        (0, 0, 1, 5, 5),
        (0, 0, 1, 5, 6),
        (0, 1, 0, 0, 0),
        (0, 1, 0, 0, 1),
        (0, 1, 0, 0, 2),
        (0, 1, 0, 0, 3),
        (0, 1, 0, 0, 4),
        (0, 1, 0, 0, 5),
        (0, 1, 0, 0, 6),
        (0, 1, 0, 1, 0),
        (0, 1, 0, 1, 1),
        (0, 1, 0, 1, 2),
        (0, 1, 0, 1, 3),
        (0, 1, 0, 1, 4),
        (0, 1, 0, 1, 5),
        (0, 1, 0, 1, 6),
        (0, 1, 0, 2, 0),
        (0, 1, 0, 2, 1),
        (0, 1, 0, 2, 2),
        (0, 1, 0, 2, 3),
        (0, 1, 0, 2, 4),
        (0, 1, 0, 2, 5),
        (0, 1, 0, 2, 6),
        (0, 1, 0, 3, 0),
        (0, 1, 0, 3, 1),
        (0, 1, 0, 3, 2),
        (0, 1, 0, 3, 3),
        (0, 1, 0, 3, 4),
        (0, 1, 0, 3, 5),
        (0, 1, 0, 3, 6),
        (0, 1, 0, 4, 0),
        (0, 1, 0, 4, 1),
        (0, 1, 0, 4, 2),
        (0, 1, 0, 4, 3),
        (0, 1, 0, 4, 4),
        (0, 1, 0, 4, 5),
        (0, 1, 0, 4, 6),
        (0, 1, 0, 5, 0),
        (0, 1, 0, 5, 1),
        (0, 1, 0, 5, 2),
        (0, 1, 0, 5, 3),
        (0, 1, 0, 5, 4),
        (0, 1, 0, 5, 5),
        (0, 1, 0, 5, 6),
        (0, 1, 1, 0, 0),
        (0, 1, 1, 0, 1),
        (0, 1, 1, 0, 2),
        (0, 1, 1, 0, 3),
        (0, 1, 1, 0, 4),
        (0, 1, 1, 0, 5),
        (0, 1, 1, 0, 6),
        (0, 1, 1, 1, 0),
        (0, 1, 1, 1, 1),
        (0, 1, 1, 1, 2),
        (0, 1, 1, 1, 3),
        (0, 1, 1, 1, 4),
        (0, 1, 1, 1, 5),
        (0, 1, 1, 1, 6),
        (0, 1, 1, 2, 0),
        (0, 1, 1, 2, 1),
        (0, 1, 1, 2, 2),
        (0, 1, 1, 2, 3),
        (0, 1, 1, 2, 4),
        (0, 1, 1, 2, 5),
        (0, 1, 1, 2, 6),
        (0, 1, 1, 3, 0),
        (0, 1, 1, 3, 1),
        (0, 1, 1, 3, 2),
        (0, 1, 1, 3, 3),
        (0, 1, 1, 3, 4),
        (0, 1, 1, 3, 5),
        (0, 1, 1, 3, 6),
        (0, 1, 1, 4, 0),
        (0, 1, 1, 4, 1),
        (0, 1, 1, 4, 2),
        (0, 1, 1, 4, 3),
        (0, 1, 1, 4, 4),
        (0, 1, 1, 4, 5),
        (0, 1, 1, 4, 6),
        (0, 1, 1, 5, 0),
        (0, 1, 1, 5, 1),
        (0, 1, 1, 5, 2),
        (0, 1, 1, 5, 3),
        (0, 1, 1, 5, 4),
        (0, 1, 1, 5, 5),
        (0, 1, 1, 5, 6),
        (1, 0, 0, 0, 0),
        (1, 0, 0, 0, 1),
        (1, 0, 0, 0, 2),
        (1, 0, 0, 0, 3),
        (1, 0, 0, 0, 4),
        (1, 0, 0, 0, 5),
        (1, 0, 0, 0, 6),
        (1, 0, 0, 1, 0),
        (1, 0, 0, 1, 1),
        (1, 0, 0, 1, 2),
        (1, 0, 0, 1, 3),
        (1, 0, 0, 1, 4),
        (1, 0, 0, 1, 5),
        (1, 0, 0, 1, 6),
        (1, 0, 0, 2, 0),
        (1, 0, 0, 2, 1),
        (1, 0, 0, 2, 2),
        (1, 0, 0, 2, 3),
        (1, 0, 0, 2, 4),
        (1, 0, 0, 2, 5),
        (1, 0, 0, 2, 6),
        (1, 0, 0, 3, 0),
        (1, 0, 0, 3, 1),
        (1, 0, 0, 3, 2),
        (1, 0, 0, 3, 3),
        (1, 0, 0, 3, 4),
        (1, 0, 0, 3, 5),
        (1, 0, 0, 3, 6),
        (1, 0, 0, 4, 0),
        (1, 0, 0, 4, 1),
        (1, 0, 0, 4, 2),
        (1, 0, 0, 4, 3),
        (1, 0, 0, 4, 4),
        (1, 0, 0, 4, 5),
        (1, 0, 0, 4, 6),
        (1, 0, 0, 5, 0),
        (1, 0, 0, 5, 1),
        (1, 0, 0, 5, 2),
        (1, 0, 0, 5, 3),
        (1, 0, 0, 5, 4),
        (1, 0, 0, 5, 5),
        (1, 0, 0, 5, 6),
        (1, 0, 1, 0, 0),
        (1, 0, 1, 0, 1),
        (1, 0, 1, 0, 2),
        (1, 0, 1, 0, 3),
        (1, 0, 1, 0, 4),
        (1, 0, 1, 0, 5),
        (1, 0, 1, 0, 6),
        (1, 0, 1, 1, 0),
        (1, 0, 1, 1, 1),
        (1, 0, 1, 1, 2),
        (1, 0, 1, 1, 3),
        (1, 0, 1, 1, 4),
        (1, 0, 1, 1, 5),
        (1, 0, 1, 1, 6),
        (1, 0, 1, 2, 0),
        (1, 0, 1, 2, 1),
        (1, 0, 1, 2, 2),
        (1, 0, 1, 2, 3),
        (1, 0, 1, 2, 4),
        (1, 0, 1, 2, 5),
        (1, 0, 1, 2, 6),
        (1, 0, 1, 3, 0),
        (1, 0, 1, 3, 1),
        (1, 0, 1, 3, 2),
        (1, 0, 1, 3, 3),
        (1, 0, 1, 3, 4),
        (1, 0, 1, 3, 5),
        (1, 0, 1, 3, 6),
        (1, 0, 1, 4, 0),
        (1, 0, 1, 4, 1),
        (1, 0, 1, 4, 2),
        (1, 0, 1, 4, 3),
        (1, 0, 1, 4, 4),
        (1, 0, 1, 4, 5),
        (1, 0, 1, 4, 6),
        (1, 0, 1, 5, 0),
        (1, 0, 1, 5, 1),
        (1, 0, 1, 5, 2),
        (1, 0, 1, 5, 3),
        (1, 0, 1, 5, 4),
        (1, 0, 1, 5, 5),
        (1, 0, 1, 5, 6),
        (1, 1, 0, 0, 0),
        (1, 1, 0, 0, 1),
        (1, 1, 0, 0, 2),
        (1, 1, 0, 0, 3),
        (1, 1, 0, 0, 4),
        (1, 1, 0, 0, 5),
        (1, 1, 0, 0, 6),
        (1, 1, 0, 1, 0),
        (1, 1, 0, 1, 1),
        (1, 1, 0, 1, 2),
        (1, 1, 0, 1, 3),
        (1, 1, 0, 1, 4),
        (1, 1, 0, 1, 5),
        (1, 1, 0, 1, 6),
        (1, 1, 0, 2, 0),
        (1, 1, 0, 2, 1),
        (1, 1, 0, 2, 2),
        (1, 1, 0, 2, 3),
        (1, 1, 0, 2, 4),
        (1, 1, 0, 2, 5),
        (1, 1, 0, 2, 6),
        (1, 1, 0, 3, 0),
        (1, 1, 0, 3, 1),
        (1, 1, 0, 3, 2),
        (1, 1, 0, 3, 3),
        (1, 1, 0, 3, 4),
        (1, 1, 0, 3, 5),
        (1, 1, 0, 3, 6),
        (1, 1, 0, 4, 0),
        (1, 1, 0, 4, 1),
        (1, 1, 0, 4, 2),
        (1, 1, 0, 4, 3),
        (1, 1, 0, 4, 4),
        (1, 1, 0, 4, 5),
        (1, 1, 0, 4, 6),
        (1, 1, 0, 5, 0),
        (1, 1, 0, 5, 1),
        (1, 1, 0, 5, 2),
        (1, 1, 0, 5, 3),
        (1, 1, 0, 5, 4),
        (1, 1, 0, 5, 5),
        (1, 1, 0, 5, 6),
        (1, 1, 1, 0, 0),
        (1, 1, 1, 0, 1),
        (1, 1, 1, 0, 2),
        (1, 1, 1, 0, 3),
        (1, 1, 1, 0, 4),
        (1, 1, 1, 0, 5),
        (1, 1, 1, 0, 6),
        (1, 1, 1, 1, 0),
        (1, 1, 1, 1, 1),
        (1, 1, 1, 1, 2),
        (1, 1, 1, 1, 3),
        (1, 1, 1, 1, 4),
        (1, 1, 1, 1, 5),
        (1, 1, 1, 1, 6),
        (1, 1, 1, 2, 0),
        (1, 1, 1, 2, 1),
        (1, 1, 1, 2, 2),
        (1, 1, 1, 2, 3),
        (1, 1, 1, 2, 4),
        (1, 1, 1, 2, 5),
        (1, 1, 1, 2, 6),
        (1, 1, 1, 3, 0),
        (1, 1, 1, 3, 1),
        (1, 1, 1, 3, 2),
        (1, 1, 1, 3, 3),
        (1, 1, 1, 3, 4),
        (1, 1, 1, 3, 5),
        (1, 1, 1, 3, 6),
        (1, 1, 1, 4, 0),
        (1, 1, 1, 4, 1),
        (1, 1, 1, 4, 2),
        (1, 1, 1, 4, 3),
        (1, 1, 1, 4, 4),
        (1, 1, 1, 4, 5),
        (1, 1, 1, 4, 6),
        (1, 1, 1, 5, 0),
        (1, 1, 1, 5, 1),
        (1, 1, 1, 5, 2),
        (1, 1, 1, 5, 3),
        (1, 1, 1, 5, 4),
        (1, 1, 1, 5, 5),
        (1, 1, 1, 5, 6),
    }
}
