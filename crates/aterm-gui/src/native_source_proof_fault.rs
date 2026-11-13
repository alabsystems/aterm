// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One negative debug-fixture fault after actual native Ready acceptance. The
//! caller supplies only a plan; the accepted op/sequence/registration/report
//! comes exclusively from the normal authenticated dispatch's real Outbox.
#[derive(serde::Serialize)]
pub(super) struct ReadyLoss {
    member: String,
    credential_generation: u64,
    previous_registration: String,
    fired: Option<aterm_json::Value>,
}
impl ReadyLoss {
    pub(super) fn arm(
        observation: &aterm_agent::harness::codex_source_registry::SourceProofObservation,
        member: &str,
        generation: u64,
    ) -> Result<Self, String> {
        let previous = observation
            .ready_members
            .iter()
            .find(|ready| ready.member == member)
            .ok_or("ready-loss member is not an actual ready manager")?;
        if generation <= previous.credential_generation {
            return Err("ready-loss requires a newer native read".into());
        }
        Ok(Self {
            member: member.into(),
            credential_generation: generation,
            previous_registration: previous.registration.clone(),
            fired: None,
        })
    }
    pub(super) fn same_plan(&self, member: &str, generation: u64) -> bool {
        self.member == member && self.credential_generation == generation
    }
    pub(super) fn candidate(&self, outcome: &aterm_json::Value) -> bool {
        self.fired.is_none()
            && outcome["action"].as_str() == Some("member-ready")
            && outcome["member"].as_str() == Some(self.member.as_str())
            && outcome["credential_generation"].as_u64() == Some(self.credential_generation)
            && outcome["registration"]
                .as_str()
                .is_some_and(|registration| registration != self.previous_registration)
            && outcome["connection"].as_u64().is_some()
    }
    pub(super) fn current(
        outcome: &aterm_json::Value,
        observation: &aterm_agent::harness::codex_source_registry::SourceProofObservation,
    ) -> bool {
        observation.unresolved == 0
            && observation.ready_members.iter().any(|ready| {
                outcome["member"].as_str() == Some(ready.member.as_str())
                    && aterm_json::to_value(ready)
                        .is_ok_and(|current| current_projection(outcome, &current))
            })
    }
    pub(super) fn accept(&mut self, outcome: &aterm_json::Value) -> bool {
        if !self.candidate(outcome) {
            return false;
        }
        self.fired = Some(outcome.clone());
        true
    }
}

// Pure correlation used only after the Registry supplied a fresh typed sample.
// Arbitrary JSON cannot enter the runtime producer or create any native grant.
fn current_projection(outcome: &aterm_json::Value, current: &aterm_json::Value) -> bool {
    [
        "member",
        "operation",
        "preload_sequence",
        "registration",
        "connection",
        "credential_generation",
        "report",
    ]
    .into_iter()
    .all(|field| !outcome[field].is_null() && outcome[field] == current[field])
}

#[cfg(test)]
#[path = "native_source_proof_fault_tests.rs"]
mod tests;
