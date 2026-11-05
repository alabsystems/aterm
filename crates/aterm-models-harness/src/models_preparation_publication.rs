// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Acknowledged publication outlives a writer attempt but grants no stale admission.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn preparation_publication_model() -> Model {
    aterm_spec::ty_model! {
        PreparationPublication {
            const Buggy = 0;
            var serial = 1; var active = 1; var durable = 0; var recorded = 0;
            var seen_ack = 0; var valid = 1; var admitted = 0;
            var stale = 0; var published = 0; var collision = 0;
            action Begin when (active == 0 && serial <= 1) {
                serial = serial + 1;
                active = 1;
                recorded = if Buggy == 6 { 0 } else { recorded };
            }
            action Collide when (active == 1) {
                collision = if Buggy == 4 { 1 } else { collision };
            }
            action Ack when (active == 1) {
                durable = 1; recorded = 1; seen_ack = 1;
            }
            action LostAck when (active == 1) { durable = 1; }
            action StaleAck when (active == 1) {
                recorded = if Buggy == 5 { 1 } else { recorded };
            }
            action Release when (active == 1) { active = 0; }
            action Invalidate { valid = 0; }
            action Admit when (active == 1 && admitted == 0) {
                admitted = if (recorded == 1 || Buggy == 1)
                    && (valid == 1 || Buggy == 2) { 1 } else { 0 };
                stale = if (recorded == 1 || Buggy == 1)
                    && (valid == 1 || Buggy == 2) && valid == 0 { 1 } else { stale };
            }
            action Publish {
                published = if recorded == 1 || Buggy == 3 { 1 } else { published };
            }
            invariant AdmissionNeedsAcknowledgement: admitted == 0 || recorded == 1;
            invariant NoStaleAdmission: stale == 0;
            invariant TerminalNeedsAcknowledgement: published == 0 || recorded == 1;
            invariant SingleWriter: collision == 0;
            invariant AcknowledgementNeedsDurability: recorded == 0 || durable == 1;
            invariant AcknowledgementIsRetained: seen_ack == 0 || recorded == 1;
            bound Finite: 1 <= serial && serial <= 2
                && 0 <= active && active <= 1 && 0 <= durable && durable <= 1
                && 0 <= recorded && recorded <= 1 && 0 <= seen_ack && seen_ack <= 1
                && 0 <= valid && valid <= 1 && 0 <= admitted && admitted <= 1
                && 0 <= stale && stale <= 1 && 0 <= published && published <= 1
                && 0 <= collision && collision <= 1;
        }
    }
}
