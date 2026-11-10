// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_spec::derive::Model;

/// One shared operation ID: a legacy recipe and a complete-receipt reservation
/// contend under the existing journal lock. No action grants execution.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn recipient_reservation_model() -> Model {
    aterm_spec::ty_model! {
        RecipientReservation {
            const Buggy = 0;
            var owner = 0; var closed = 0; var available = 1; var domain = 1;
            var room = 1; var reserved_ack = 0; var legacy_ack = 0;
            var reserved_ever = 0; var legacy_ever = 0;
            action Reserve {
                reserved_ack = if (available == 1 || Buggy == 6)
                    && (domain == 1 || Buggy == 5)
                    && (owner == 2 || (owner == 0 && (room == 1 || Buggy == 4))
                        || (Buggy == 1 && owner == 1)) {1} else {0};
                legacy_ack = 0;
                owner = if (available == 1 || Buggy == 6)
                    && (domain == 1 || Buggy == 5)
                    && ((owner == 0 && (room == 1 || Buggy == 4))
                        || (Buggy == 1 && owner == 1))
                    {if Buggy == 7 {owner} else {2}} else {owner};
                reserved_ever = if (available == 1 || Buggy == 6)
                    && (domain == 1 || Buggy == 5)
                    && (owner == 2 || (owner == 0 && (room == 1 || Buggy == 4))
                        || (Buggy == 1 && owner == 1))
                    {if Buggy == 7 {reserved_ever} else {1}} else {reserved_ever};
            }
            action Legacy {
                legacy_ack = if available == 1 && domain == 1 && closed == 0
                    && ((owner == 0 && room == 1) || owner == 1 || (Buggy == 2 && owner == 2)) {1} else {0};
                reserved_ack = 0;
                owner = if available == 1 && domain == 1 && closed == 0
                    && ((owner == 0 && room == 1) || (Buggy == 2 && owner == 2)) {1} else {owner};
                legacy_ever = if available == 1 && domain == 1 && closed == 0
                    && ((owner == 0 && room == 1) || owner == 1 || (Buggy == 2 && owner == 2)) {1} else {legacy_ever};
            }
            action Close when (owner == 1 && closed == 0 && available == 1 && domain == 1) {
                closed = 1; reserved_ack = 0; legacy_ack = 0;
            }
            action Crash { reserved_ack = 0; legacy_ack = 0; }
            action Lose when (available == 1 && owner > 0) {
                available = 0; reserved_ack = 0; legacy_ack = 0;
                owner = if Buggy == 3 {0} else {owner};
            }
            action WrongDomain when (domain == 1) {
                domain = 0; reserved_ack = 0; legacy_ack = 0;
            }
            action Tight when (owner == 0 && room == 1) { room = 0; }
            invariant ReservationOwns: reserved_ever == 0 || owner == 2;
            invariant LegacyOwns: legacy_ever == 0 || owner == 1;
            invariant AckOwns: reserved_ack == 0 || owner == 2;
            invariant Capacity: owner == 0 || room == 1;
            invariant Domain: reserved_ack == 0 || domain == 1;
            invariant Custody: reserved_ack == 0 || available == 1;
            bound Finite: 0 <= owner && owner <= 2 && 0 <= closed && closed <= 1
                && 0 <= available && available <= 1 && 0 <= domain && domain <= 1
                && 0 <= room && room <= 1 && 0 <= reserved_ack && reserved_ack <= 1
                && 0 <= legacy_ack && legacy_ack <= 1
                && 0 <= reserved_ever && reserved_ever <= 1 && 0 <= legacy_ever && legacy_ever <= 1;
        }
    }
}
