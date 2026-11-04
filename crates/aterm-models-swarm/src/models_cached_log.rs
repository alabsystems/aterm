// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A configured-log replay needs bound local custody, useful work and an eligible reader.
/// Pending activation itself is work, even when its stored record digest already matches.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn configured_log_model() -> Model {
    aterm_spec::ty_model! {
        VaultConfiguredLogReplay {
            const Buggy = 0;
            var pure = 1; var empty = 1; var bound = 1; var log = 1; var current = 1;
            var work = 0; var active = 1; var eligible = 1;
            var phase = 0; var pick = 0;
            action Envelope when (phase == 0 && pure == 1) { pure = 0; }
            action HaveCache when (phase == 0 && empty == 1) { empty = 0; }
            action Unbind when (phase == 0 && bound == 1) { bound = 0; }
            action LoseLog when (phase == 0 && log == 1) { log = 0; }
            action Replace when (phase == 0 && current == 1) { current = 0; }
            action NeedWork when (phase == 0 && work == 0) { work = 1; }
            action Pending when (phase == 0 && active == 1) {
                active = 0; eligible = 0; work = 1;
            }
            action Admit when (phase == 0 && active == 0 && eligible == 0) { eligible = 1; }
            action Select when (phase == 0) {
                phase = 1;
                pick = if Buggy == 7 {0}
                    else if (pure == 1 || Buggy == 1) && (empty == 1 || Buggy == 8)
                        && (bound == 1 || Buggy == 2)
                        && (log == 1 || Buggy == 3) && (current == 1 || Buggy == 4)
                        && (work == 1 || Buggy == 5)
                        && (active == 1 || eligible == 1 || Buggy == 6) {1}
                    else {0};
            }
            invariant LogOnly: pick == 0 || pure == 1;
            invariant CacheAbsent: pick == 0 || empty == 1;
            invariant SourceBound: pick == 0 || bound == 1;
            invariant Custody: pick == 0 || log == 1;
            invariant CurrentPolicy: pick == 0 || current == 1;
            invariant Useful: pick == 0 || work == 1;
            invariant Eligible: pick == 0 || active == 1 || eligible == 1;
            invariant Progress: phase == 0 || pure == 0 || empty == 0 || bound == 0 || log == 0
                || current == 0 || work == 0 || (active == 0 && eligible == 0) || pick == 1;
            bound ReplayBox: pure <= 1 && empty <= 1 && bound <= 1 && log <= 1 && current <= 1
                && work <= 1 && active <= 1 && eligible <= 1 && phase <= 1 && pick <= 1;
        }
    }
}

/// Cached log replay requires a present changed log and an exact verified envelope.
/// Losing an applied log cannot make an envelope replace its materialization.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cached_log_model() -> Model {
    aterm_spec::ty_model! {
        VaultCachedLogReplay {
            const Buggy = 0;
            var log = 1; var changed = 0; var cache = 1;
            var applied = 1; var replaced = 0;
            var active = 1; var eligible = 1; var behind = 0;
            var phase = 0; var pick = 0; var network = 0; var wait = 0;
            action ChangeLog when (phase == 0 && changed == 0) { changed = 1; }
            action LoseLog when (phase == 0 && log == 1) { log = 0; }
            action LoseCache when (phase == 0 && cache == 1) { cache = 0; }
            action NoAppliedLog when (phase == 0 && applied == 1) { applied = 0; changed = 1; }
            action ReplaceLineage when (phase == 0 && replaced == 0) { replaced = 1; }
            action BehindStamp when (phase == 0 && active == 1 && behind == 0) {
                behind = 1; changed = 1;
            }
            action Pending when (phase == 0 && active == 1 && behind == 0) {
                active = 0; eligible = 0; changed = 1;
            }
            action Admit when (phase == 0 && active == 0 && eligible == 0) { eligible = 1; }
            action Select when (phase == 0) {
                phase = 1;
                pick = if Buggy == 4 {0}
                    else if (log == 1 || Buggy == 1)
                        && (cache == 1 || Buggy == 2)
                        && (changed == 1 || Buggy == 3)
                        && (behind == 0 || Buggy == 8)
                        && (eligible == 1 || Buggy == 9) {1}
                    else {0};
                network = if Buggy == 5 && log == 1 && cache == 1 && changed == 1
                    && behind == 0 && eligible == 1 {1} else {0};
            }
            action Guard when (phase == 1) {
                phase = 2;
                wait = if Buggy == 6 {0}
                    else if Buggy == 7 || (applied == 1 && log == 0 && replaced == 0) {1}
                    else {0};
            }
            invariant ReplayAuthority: pick == 0 || (log == 1 && cache == 1);
            invariant ChangedOnly: pick == 0 || changed == 1;
            invariant CoverRequired: pick == 0 || behind == 0;
            invariant EligibleReaderOnly: pick == 0 || eligible == 1;
            invariant ReplayProgress: phase == 0 || log == 0 || cache == 0 || changed == 0
                || behind == 1 || eligible == 0 || pick == 1;
            invariant NoNetworkCredit: network == 0;
            invariant KeepAppliedLog: phase <= 1 || applied == 0 || log == 1 || replaced == 1 || wait == 1;
            invariant NoSpuriousWait: wait == 0 || (applied == 1 && log == 0 && replaced == 0);
            bound ReplayBox: log <= 1 && changed <= 1 && cache <= 1 && applied <= 1
                && replaced <= 1 && active <= 1 && eligible <= 1 && behind <= 1
                && phase <= 2 && pick <= 1 && network <= 1 && wait <= 1;
        }
    }
}

/// A received removal stays visible to authorization while blocking cached publication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cached_publication_model() -> Model {
    aterm_spec::ty_model! {
        VaultCachedPublication {
            const Buggy = 0;
            var revoked = 0; var phase = 0; var denied = 0; var copy = 0;
            action Revoke when (phase == 0 && revoked == 0) { revoked = 1; }
            action Read when (phase == 0) {
                phase = 1;
                denied = if Buggy == 1 {0} else {revoked};
            }
            action Publish when (phase == 1) {
                phase = 2;
                copy = if revoked == 0 || Buggy == 2 {1} else {0};
            }
            invariant DenialVisible: phase == 0 || revoked == 0 || denied == 1;
            invariant PublicationDenied: copy == 0 || revoked == 0;
            bound PublicationBox: revoked <= 1 && phase <= 2 && denied <= 1 && copy <= 1;
        }
    }
}

/// A cold reader honors the persisted applied-log floor and resumes when the log returns.
/// The model assumes its input log has already passed authentication and lineage checks.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cold_read_model() -> Model {
    aterm_spec::ty_model! {
        VaultColdRead {
            const Buggy = 0;
            var applied = 0; var log = 1; var phase = 0; var visible = 0;
            action Apply when (phase == 0 && log == 1 && applied == 0) { applied = 1; }
            action LoseLog when (phase == 0 && log == 1) { log = 0; }
            action RestoreLog when (phase == 0 && log == 0) { log = 1; }
            action Read when (phase == 0) {
                phase = 1;
                visible = if Buggy == 1 {1}
                    else if Buggy == 2 {0}
                    else if applied == 0 || log == 1 {1} else {0};
            }
            invariant NoFallback: phase == 0 || applied == 0 || log == 1 || visible == 0;
            invariant Availability: phase == 0 || (applied == 1 && log == 0) || visible == 1;
            bound ReadBox: applied <= 1 && log <= 1 && phase <= 1 && visible <= 1;
        }
    }
}

/// A caller covers retained authenticated heads independently of reader membership.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn caller_custody_model() -> Model {
    aterm_spec::ty_model! {
        VaultCallerCustody {
            const Buggy = 0;
            var current = 1; var behind = 0; var phase = 0; var admitted = 0;
            action Advance when (phase == 0 && behind == 0) { behind = 1; }
            action Noncurrent when (phase == 0 && current == 1) { current = 0; }
            action Read when (phase == 0) {
                phase = 1;
                admitted = if Buggy == 1 && current == 0 {1}
                    else if Buggy == 2 && current == 0 {0}
                    else if behind == 0 {1} else {0};
            }
            invariant RetainedHeads: phase == 0 || behind == 0 || admitted == 0;
            invariant CoveredAvailable: phase == 0 || behind == 1 || admitted == 1;
            bound CustodyBox: current <= 1 && behind <= 1 && phase <= 1 && admitted <= 1;
        }
    }
}
