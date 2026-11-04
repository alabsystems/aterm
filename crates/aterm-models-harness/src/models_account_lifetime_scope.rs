// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Independent lifetime observations survive non-credential edits, but never follow a
/// replaced login merely because a current authentication word was observed there.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_lifetime_scope_model() -> Model {
    aterm_spec::ty_model! {
        AccountLifetimeScope {
            const Buggy = 0;
            var phase = 0; var transform = 1; var has_auth = 0; var read = 0;
            var forecast = 0; var served = 0; var ended = 0; var deadline = 0;
            var due = 0; var ok = 0; var renew = 0; var closing = 0;
            var history = 1; var read_only = 1;
            var accepted = 0; var preserved = 1;
            action AnotherTransform when (phase == 0 && 4 > transform) {
                transform = transform + 1;
            }
            action WithAuth when (phase == 0 && has_auth == 0) { has_auth = 1; }
            action Seed when (phase == 0) { phase = 1; read = 0; }
            action Transform when (phase == 1 && read == 1) {
                phase = 2; read = 0;
                history = if Buggy == 9 {0} else {1};
            }
            action AuthOnly when (phase == 2 && read == 1) { phase = 3; read = 0; }
            action Served when (phase == 3 && read == 1) { phase = 4; read = 0; }
            action Notice when (phase == 3 && read == 1) { phase = 5; read = 0; }
            action Renewed when (phase == 3 && read == 1) { phase = 6; read = 0; }
            action StaleServed when (phase == 3 && read == 1) {
                phase = 7; read = 0;
                accepted = if Buggy == 19 {1} else {0};
                preserved = if Buggy == 20 {0} else {1};
            }
            action Read when (phase > 0 && phase <= 6 && read == 0) {
                read = 1;
                forecast = if Buggy == 13 {0}
                    else if (Buggy == 1 && transform <= 2 && phase == 2)
                        || (Buggy == 4 && transform <= 2 && phase == 3)
                        || (Buggy == 6 && transform <= 2 && phase == 4) {1}
                    else if (Buggy == 11 && transform == 3 && phase > 1 && 4 > phase)
                        || (Buggy == 12 && transform == 4 && phase > 1 && 4 > phase)
                        || (Buggy == 14 && phase == 5) || (Buggy == 16 && phase == 6) {0}
                    else if phase == 1 || transform > 2 || phase > 4 {1} else {0};
                served = if Buggy == 13 {0}
                    else if (Buggy == 2 && transform <= 2 && phase == 2)
                        || (Buggy == 5 && transform <= 2 && phase == 3)
                        || (Buggy == 8 && phase == 6) {1}
                    else if (Buggy == 11 && transform == 3 && phase > 1 && 4 > phase)
                        || (Buggy == 12 && transform == 4 && phase > 1 && 4 > phase)
                        || (Buggy == 15 && phase == 4) {0}
                    else if phase == 4 || (6 > phase && (phase == 1 || transform > 2)) {1} else {0};
                ended = if Buggy == 13 {0}
                    else if Buggy == 3 && transform <= 2 && phase > 1 && 6 > phase {1}
                    else if phase == 1 || (transform > 2 && 6 > phase) {1} else {0};
                deadline = if phase == 6 {if Buggy == 16 {0} else {3}}
                    else if phase == 5 && transform <= 2 {if Buggy == 7 {1} else {2}}
                    else if phase == 1 || transform > 2 {1} else {0};
                due = if 6 > phase && (phase == 1 || transform > 2 || phase == 5) {1} else {0};
                ok = if phase == 4 && transform <= 2 {1} else {0};
                renew = if Buggy == 18 && transform <= 2 && phase == 3 {1}
                    else if 6 > phase && (phase == 1 || transform > 2 || phase == 5) {1} else {0};
                closing = if Buggy == 17 && transform <= 2 && phase == 3 {1}
                    else if 6 > phase && (phase == 1 || transform > 2 || phase == 5) {1} else {0};
                read_only = if Buggy == 10 {0} else {1};
            }
            invariant OldForecastRetired: read == 0 || transform > 2 || phase <= 1 || phase > 4 || forecast == 0;
            invariant OldServedRetired: read == 0 || transform > 2 || phase <= 1 || phase > 3 || served == 0;
            invariant OldEndRetired: read == 0 || transform > 2 || phase <= 1 || phase == 6 || ended == 0;
            invariant AuthDoesNotEstablishClocks: read == 0 || phase <= 2 || phase > 3 || transform > 2
                || (forecast == 0 && served == 0 && ended == 0);
            invariant InitialEvidenceAvailable: read == 0 || phase > 1 || (forecast == 1 && served == 1 && ended == 1 && due == 1);
            invariant NonCredentialEditsKeepEvidence: read == 0 || transform <= 2 || phase <= 1 || phase > 3
                || (forecast == 1 && served == 1 && ended == 1 && due == 1);
            invariant FreshServedAvailable: read == 0 || 4 > phase || phase > 4 || served == 1;
            invariant FreshServedDoesNotRenew: read == 0 || 4 > phase || phase > 4 || transform > 2 || forecast == 0;
            invariant FreshNoticeAvailable: read == 0 || 5 > phase || phase > 5 || forecast == 1;
            invariant NoticeUsesCurrentDeadline: read == 0 || 5 > phase || phase > 5 || deadline == if transform <= 2 {2} else {1};
            invariant RenewalAvailable: read == 0 || 6 > phase || (forecast == 1 && deadline == 3);
            invariant RenewalDoesNotInventServed: read == 0 || 6 > phase || served == 0;
            invariant RenewalQueueUsesCurrentEvidence: read == 0 || renew == due;
            invariant RunwayUsesCurrentEvidence: read == 0 || closing == due;
            invariant DueUsesForecast: read == 0 || due == if forecast == 1 && 6 > phase {1} else {0};
            invariant ServedFollowsForecastPriority: read == 0 || ok == if served == 1 && due == 0 {1} else {0};
            invariant ReadsDoNotWrite: read_only == 1;
            invariant ReplacementRetainsRawHistory: history == 1;
            invariant StaleClockPublicationRefused: phase <= 6 || accepted == 0;
            invariant RefusalPreservesPublicationImages: preserved == 1;
        }
    }
}
