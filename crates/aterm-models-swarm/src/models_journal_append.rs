// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One acknowledged journal append with a dependent write (`aterm-journal`'s
/// `Log::append_then`; a Commons table's witness is the dependent write), the previous
/// append's dependent write (`prior`), and what a power loss may keep of them. Each write is
/// `0` unwritten or in the page cache only, `1` written and issued to the device behind a
/// barrier, or `2` on stable media. A barrier orders a write issued after it behind one issued
/// before it, on one device, so what a power loss takes there is a suffix of what was issued
/// behind barriers; a full flush on a device makes every write issued there before it
/// durable, and the log's own full flush its page cache too. The record and the checkpoint
/// share the log's directory; the witnesses are on that device unless `apart`, where the
/// previous one was flushed in full. `strict`: astream serves the log Strict, each record
/// flushed in full. A reader that takes no lock trusts a witness once it is written
/// (`witnessed`), so a witness shows only a durable record; the custody that reads the
/// witnesses back takes back at most one record past them. Signatures, hashing and the
/// journal's private-file checks are outside this bounded model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn journal_append_model() -> Model {
    aterm_spec::ty_model! {
        JournalAppend {
            const Buggy = 0;
            var strict = 0; var apart = 0;
            var published = 0; var checkpointed = 0; var committed = 0; var witnessed = 0;
            var acked = 0;
            var prior = 1; var record = 0; var checkpoint = 0; var witness = 0;
            var crashed = 0; var kept_prior = 0; var kept_record = 0; var kept_checkpoint = 0;
            var kept_witness = 0;
            action Latch when (published == 0 && strict == 0) { strict = 1; }
            action Apart when (published == 0 && apart == 0) {
                apart = 1;
                prior = if Buggy == 4 {1} else {2};
            }
            action Publish when (crashed == 0 && published == 0) {
                published = 1;
                record = if Buggy == 1 {0} else {if strict == 1 {2} else {1}};
                prior = if Buggy == 1 {prior} else {if strict == 1 && apart == 0 {2} else {prior}};
            }
            action Checkpoint when (crashed == 0 && published == 1 && checkpointed == 0) {
                checkpointed = 1;
                checkpoint = if Buggy == 2 {1} else {if strict == 1 {2} else {1}};
            }
            action Commit when (crashed == 0 && checkpointed == 1 && committed == 0) {
                committed = 1;
                checkpoint = if record == 2 {checkpoint} else {if checkpoint == 1 {2} else {checkpoint}};
                witness = if record == 2 || apart == 1 {witness} else {if witness == 1 {2} else {witness}};
                prior = if record == 2 || apart == 1 {prior} else {if prior == 1 {2} else {prior}};
                record = 2;
            }
            action Witness when (crashed == 0 && witnessed == 0
                && (committed == 1 || (Buggy == 3 && checkpointed == 1))) {
                witnessed = 1;
                witness = if Buggy == 4 {1} else {if strict == 1 || apart == 1 {2} else {1}};
                checkpoint = if Buggy == 4 {checkpoint} else {if strict == 1 && apart == 0 && checkpoint == 1 {2} else {checkpoint}};
                prior = if Buggy == 4 {prior} else {if strict == 1 && apart == 0 && prior == 1 {2} else {prior}};
            }
            action Ack when (crashed == 0 && committed == 1 && witnessed == 1 && acked == 0) {
                acked = 1;
            }
            action Crash when (crashed == 0) {
                crashed = 1;
                kept_prior = 1;
                kept_record = published;
                kept_checkpoint = checkpointed;
                kept_witness = witnessed;
            }
            action DropWitness when (crashed == 1 && kept_witness == 1 && witness <= 1) {
                kept_witness = 0;
            }
            action DropCheckpoint when (crashed == 1 && kept_checkpoint == 1 && checkpoint <= 1
                && (kept_witness == 0 || apart == 1)) {
                kept_checkpoint = 0;
            }
            action DropRecord when (crashed == 1 && kept_record == 1 && record <= 1
                && (kept_checkpoint == 0 || record == 0)) {
                kept_record = 0;
            }
            action DropPrior when (crashed == 1 && kept_prior == 1 && prior <= 1
                && (apart == 1 || (kept_record == 0 && kept_checkpoint == 0
                    && kept_witness == 0))) {
                kept_prior = 0;
            }
            invariant RecordBeforeCheckpoint: kept_checkpoint == 0 || kept_record == 1;
            invariant CheckpointBeforeWitness: kept_witness == 0 || kept_checkpoint == 1;
            invariant WitnessShowsDurable: witnessed == 0 || (record == 2 && checkpoint == 2);
            invariant AcknowledgedSurvives: crashed == 0 || acked == 0
                || (kept_record == 1 && kept_checkpoint == 1);
            invariant OneRecordPastTheWitnesses: kept_record == 0 || kept_checkpoint == 0
                || kept_witness == 1 || kept_prior == 1;
            bound AppendBox: strict <= 1 && apart <= 1 && published <= 1 && checkpointed <= 1
                && committed <= 1 && witnessed <= 1 && acked <= 1 && prior <= 2 && record <= 2
                && checkpoint <= 2 && witness <= 2 && crashed <= 1 && kept_prior <= 1
                && kept_record <= 1 && kept_checkpoint <= 1 && kept_witness <= 1;
        }
    }
}
