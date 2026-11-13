// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two consecutive two-record checkpoint groups and their independent custody.
/// A journal checkpoint identifies either a complete group or the previous prefix;
/// exact receipt hashes and application authentication are separate obligations.
/// The next permit's temporary file is flushed in full on the custody device before
/// its rename replaces the previous permit. That flush also covers the previous
/// Ordered custody witness, including when custody and the journal are apart.
/// A permit's directory flush precedes publication of any record in its group.
/// Witnesses are published only after the entire group is durable; their directory
/// flush then finishes before acknowledgement. A reader can see the rename before
/// that flush. Witnesses may remain Ordered on the journal's device. A crash can
/// lose an unflushed rename or witness.
/// A Strict journal flushes its checkpoint in full and finishes its witness in full.
/// Counts distinguish exact endpoints here; code must compare their hashes as well.
/// Flushing the directory alone does not flush a temporary file's unissued data:
/// a surviving permit entry can still lose its contents if its file flush was omitted.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn journal_group_custody_model() -> Model {
    aterm_spec::ty_model! {
        JournalGroupCustody {
            const Buggy = 0;
            var generation = 0; var phase = 0; var apart = 0; var strict = 0; var crashed = 0;
            var checkpoint = 0; var durable_checkpoint = 0;
            var witness = 0; var durable_witness = 0; var acknowledged = 0;
            var permit_start = 0; var permit_end = 0; var entry_durable = 1;
            var temp_durable = 1; var permit_data = 1;
            var old_start = 0; var old_end = 0; var old_data = 1;
            var kept_checkpoint = 0; var kept_witness = 0;
            var kept_start = 0; var kept_end = 0; var kept_data = 1; var kept_floor = 0;
            action Latch when (crashed == 0 && generation == 0 && phase == 0 && strict == 0) {
                strict = 1;
            }
            action Apart when (crashed == 0 && generation == 0 && phase == 0 && apart == 0) {
                apart = 1;
            }
            action Prepare when (crashed == 0 && phase == 0) { temp_durable = 0; phase = 1; }
            action FlushTemporary when (crashed == 0 && phase == 1) {
                temp_durable = if Buggy == 1 {0} else {1};
                durable_witness = if Buggy == 1 {durable_witness} else {witness};
                phase = 2;
            }
            action RenamePermit when (crashed == 0 && phase == 2) {
                old_start = permit_start;
                old_end = permit_end;
                old_data = permit_data;
                permit_start = witness;
                permit_end = if generation == 0 {2} else {4};
                permit_data = temp_durable;
                entry_durable = 0;
                phase = 3;
            }
            action FlushPermitDirectory when (crashed == 0 && phase == 3) {
                entry_durable = if Buggy == 2 {0} else {1};
                durable_witness = if Buggy == 2 {durable_witness} else {witness};
                phase = 4;
            }
            action Checkpoint when (crashed == 0 && phase == 4) {
                checkpoint = if generation == 0 {2} else {4};
                durable_checkpoint = if strict == 1 {
                    if generation == 0 {2} else {4}
                } else {durable_checkpoint};
                phase = 5;
            }
            action Commit when (crashed == 0 && phase == 5) {
                durable_checkpoint = if Buggy == 4 {durable_checkpoint} else {checkpoint};
                phase = 6;
            }
            action Witness when (crashed == 0 && (phase == 6 || (Buggy == 3 && phase == 5))) {
                witness = checkpoint;
                phase = 7;
            }
            action FinishWitness when (crashed == 0 && phase == 7) {
                durable_witness = if Buggy == 3 {durable_witness} else {
                    if apart == 1 || strict == 1 {checkpoint} else {durable_witness}
                };
                phase = 8;
            }
            action Ack when (crashed == 0 && phase == 8) {
                acknowledged = checkpoint;
                phase = 9;
            }
            action NextGroup when (crashed == 0 && phase == 9 && generation == 0) {
                generation = 1;
                phase = 0;
            }
            action Crash when (crashed == 0) {
                crashed = 1;
                kept_checkpoint = checkpoint;
                kept_witness = witness;
                kept_start = permit_start;
                kept_end = permit_end;
                kept_data = permit_data;
                kept_floor = permit_start;
            }
            action DropCheckpoint when (crashed == 1 && kept_checkpoint > durable_checkpoint
                && (kept_witness <= durable_checkpoint || apart == 1)) {
                kept_checkpoint = durable_checkpoint;
            }
            action DropWitness when (crashed == 1 && kept_witness > durable_witness
                && kept_floor <= durable_witness) {
                kept_witness = durable_witness;
            }
            action DropPermit when (crashed == 1 && entry_durable == 0
                && (kept_start > old_start || kept_end > old_end)) {
                kept_start = old_start;
                kept_end = old_end;
                kept_data = old_data;
                kept_floor = old_start;
            }
            action DropPermitData when (crashed == 1 && kept_data == 0 && kept_end > 0) {
                kept_start = 0;
                kept_end = 0;
            }
            invariant WitnessShowsDurable: witness <= durable_checkpoint;
            invariant ExactIndependentRecovery: crashed == 0 || kept_checkpoint == kept_witness
                || (kept_start == kept_witness && kept_end == kept_checkpoint);
            invariant AcknowledgedSurvives: crashed == 0 || acknowledged <= kept_checkpoint;
            invariant CustodyNeverAhead: crashed == 0 || kept_witness <= kept_checkpoint;
            bound GroupBox: generation <= 1 && phase <= 9 && apart <= 1 && strict <= 1 && crashed <= 1
                && checkpoint <= 4 && durable_checkpoint <= 4 && witness <= 4
                && durable_witness <= 4 && acknowledged <= 4 && permit_start <= 2
                && permit_end <= 4 && entry_durable <= 1 && temp_durable <= 1 && permit_data <= 1
                && old_start <= 2 && old_end <= 4 && old_data <= 1 && kept_data <= 1 && kept_floor <= 2
                && kept_checkpoint <= 4 && kept_witness <= 4 && kept_start <= 2 && kept_end <= 4;
        }
    }
}
