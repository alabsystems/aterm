// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cold owner recovery never reacquires work from identities. It can restore
//! only the original private CPU capabilities after authentic memory closure.
use super::*;
use aterm_capacity::{CohortClaimState, CohortClosureRef};

pub(super) fn context(
    binding: &Binding,
    memory: &CohortClosureRef,
    bytes: u64,
    width: u32,
) -> [u8; 32] {
    let record = aterm_json::json!({"domain":"aterm-resource-host-recovery-v3","session":binding.session,
        "attempt":aterm_codec::hex::encode_lower(&binding.attempt),"memory":memory.to_json(),"bytes":bytes,"width":width});
    aterm_digest::Sha256::digest(record.to_string().as_bytes())
}
impl Owner {
    pub(super) fn recover(&mut self) -> io::Result<()> {
        for record in self.journal.outstanding()? {
            let binding = journal::record_binding(&record)?;
            if self
                .jobs
                .contains_key(&(binding.session.clone(), binding.attempt))
                || self.failed.iter().any(|failed| failed.binding == binding)
                || self
                    .pending
                    .iter()
                    .any(|pending| pending.submission.request.binding == binding)
                || self.jobs.values().any(|job| {
                    job.binding.session == binding.session
                        && job.members.contains_key(&binding.attempt)
                })
            {
                continue;
            }
            let phase = record["phase"]
                .as_str()
                .ok_or_else(|| refused("missing original phase"))?;
            match phase {
                "queued" => {
                    // CPU publication and ARM are forbidden before the durable
                    // prepared record. An orphan unused description grants no work.
                    self.journal.settle(&binding)?;
                }
                "settled" => self.journal.finish(&binding)?,
                "member_prepared" | "member_bound" | "member_running" | "member_terminal" => {
                    // Members borrow the original aggregate memory claim and
                    // ordinary kernel CPU rights. This row owns no fixed grant.
                    let memory = CohortClosureRef::from_json(&record["memory"])?;
                    if self.ledger.reap_cohort_claim(&memory)? == CohortClaimState::Closed {
                        self.journal.settle(&binding)?;
                    }
                }
                "prepared" | "bound" | "running" | "terminal" => {
                    let memory = CohortClosureRef::from_json(&record["memory"])?;
                    if self.ledger.reap_cohort_claim(&memory)? != CohortClaimState::Closed {
                        continue;
                    }
                    match record["cpu_mode"].as_str() {
                        Some("retained_fixed") => {
                            let bytes = record["memory_bytes"]
                                .as_u64()
                                .filter(|n| *n > 0)
                                .ok_or_else(|| refused("missing original memory declaration"))?;
                            let width = record["cpu_units"]
                                .as_u64()
                                .and_then(|n| u32::try_from(n).ok())
                                .filter(|n| *n > 0)
                                .ok_or_else(|| refused("missing original CPU declaration"))?;
                            let (parent, leaf) = self.journal.authority(&binding)?;
                            let original = self.debt.restore_recovery(&parent, &leaf)?;
                            if original.context() != context(&binding, &memory, bytes, width)
                                || original.width().get() != width
                            {
                                return Err(refused(
                                    "private original CPU authority differs from cohort binding",
                                ));
                            }
                            // Only the exact durable Closed result above permits
                            // this use of original secret-backed settlement custody.
                            original.settle()?;
                        }
                        Some("cooperative_shared") => {}
                        _ => return Err(refused("unknown original CPU mode")),
                    }
                    self.journal.settle(&binding)?;
                }
                _ => return Err(refused("unknown original host phase")),
            }
        }
        Ok(())
    }
}
