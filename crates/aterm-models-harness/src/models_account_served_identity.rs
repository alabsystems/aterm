// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Served observations and sign-in recovery use the identity at their writer.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_served_identity_model() -> Model {
    aterm_spec::ty_model! {
        AccountServedIdentity {
            const Buggy = 0;
            var named = 0; var before = 0; var credential = 0; var denied = 0;
            var before_auth = 0; var auth = 0; var auth_result = 0; var method = 0;
            var phase = 0; var polls = 0;
            var writes = 0; var proven = 0; var recovered = 0;
            var remaining = 1; var canary = 1; var preserved = 1; var quiet = 1;
            action Named when (phase == 0 && named == 0) { named = 1; }
            action Before when (phase == 0 && before <= 1) { before = before + 1; }
            action Credential when (phase == 0 && credential <= 1) { credential = credential + 1; }
            action BeforeAuth when (phase == 0 && before_auth <= 1) { before_auth = before_auth + 1; }
            action Auth when (phase == 0 && auth <= 1) { auth = auth + 1; auth_result = auth + 1; }
            action Method when (phase == 0 && method == 0) { method = 1; }
            action Observe when (phase == 0) {
                phase = 1;
                denied = credential;
                writes = if Buggy == 1 {
                    if (method == 0 || auth == 2) && (named == 0 || before == 1 || (before == 0 && auth <= 1 && method == 0)) {1} else {0}
                } else if Buggy == 2 {
                    if (method == 0 || auth == 2) && (named == 0 || credential == 1 || (credential == 0 && before_auth <= 1 && method == 0)) {1} else {0}
                } else if Buggy == 3 {
                    if method == 0 && (auth <= 1 || named == 0 || credential == 1) {1}
                    else if method == 1 && auth == 2 && (named == 0 || credential == 1) {1} else {0}
                } else if Buggy == 4 {
                    if method == 0 || auth == 2 {1} else {0}
                } else if Buggy == 5 {
                    if (method == 0 || auth == 2) && (named == 0 || credential == 1) {1} else {0}
                } else if Buggy == 6 {
                    if (method == 0 || auth == 2) && (named == 0 || credential <= 1) {1} else {0}
                } else if Buggy == 7 {
                    if (method == 0 || auth == 2) && named == 1 && (credential == 1 || (credential == 0 && auth <= 1 && method == 0)) {1} else {0}
                } else if (method == 0 || auth == 2) && (named == 0 || credential == 1 || (credential == 0 && auth <= 1 && method == 0)) {1} else {0};
                proven = if (Buggy == 8 && method == 0) || (Buggy == 16 && method == 1 && auth == 2 && (named == 0 || credential == 1)) {1}
                    else if method == 0 && (named == 0 || credential == 1 || (credential == 0 && auth <= 1)) {1} else {0};
                recovered = if Buggy == 9 {1}
                    else if auth == 2 && (named == 0 || credential == 1) {1} else {0};
                auth_result = if Buggy == 18 {auth}
                    else if (method == 0 || auth == 2) && (named == 0 || credential == 1 || (credential == 0 && auth <= 1 && method == 0)) {1} else {auth};
                remaining = if Buggy == 10 {0}
                    else if method == 0 && (named == 0 || credential == 1 || (credential == 0 && auth <= 1)) {0} else {1};
                canary = if Buggy == 11 {0}
                    else if method == 0 && (named == 0 || credential == 1 || (credential == 0 && auth <= 1)) {0} else {1};
                preserved = if Buggy == 12 {0} else {1};
                quiet = if Buggy == 13 {0}
                    else if (method == 0 || auth == 2) && (named == 0 || credential == 1 || (credential == 0 && auth <= 1 && method == 0)) {0} else {1};
            }
            action Poll when (phase == 1 && writes == 0 && (method == 0 || auth == 2) && polls == 0) {
                polls = 1;
                writes = if Buggy == 17 {1} else {0};
                quiet = if Buggy == 17 {0} else {1};
            }
            action Correct when (phase == 1 && writes == 0 && (method == 0 || auth == 2)) {
                credential = 1;
                phase = 2;
            }
            action Retry when (phase == 2) {
                phase = 3;
                writes = if Buggy == 15 {0} else {1};
                proven = if method == 0 {1} else {0};
                recovered = if auth == 2 {1} else {0};
                auth_result = 1;
                remaining = if method == 0 {0} else {1};
                canary = if method == 0 {0} else {1};
                quiet = 0;
            }
            action Replay when ((phase == 1 || phase == 3) && writes == 1) {
                phase = 4;
                writes = if Buggy == 14 {writes + 1} else {writes};
                quiet = if Buggy == 19 {0} else {1};
            }
            invariant Admission: phase == 0 || phase > 1 || writes == if (method == 0 || auth == 2) && (named == 0 || credential == 1 || (credential == 0 && auth <= 1 && method == 0)) {1} else {0};
            invariant Proven: proven == if writes > 0 && method == 0 {1} else {0};
            invariant Recovery: recovered == if writes > 0 && auth == 2 {1} else {0};
            invariant Authentication: auth_result == if writes > 0 {1} else {auth};
            invariant Move: remaining + proven == 1;
            invariant Canary: canary + proven == 1;
            invariant Evidence: preserved == 1;
            invariant Quiet: quiet == if (phase == 1 && writes == 1) || phase == 3 {0} else {1};
            invariant Progress: phase <= 2 || writes == 1;
            invariant Once: writes <= 1;
        }
    }
}
