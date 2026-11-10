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
mod tests {
    use super::*;
    #[test]
    fn historical_ack_on_cleanup_connection_cannot_fire_current_work_fault() {
        // Unattested metadata exercises refusal, not native Ready admission.
        let event = aterm_json::json!({"member":"a".repeat(32),"operation":"b".repeat(32),
            "preload_sequence":"c".repeat(32),"registration":"d".repeat(32),"connection":7,
            "credential_generation":4,"report":[4,1,2,1]});
        assert!(current_projection(&event, &event));
        for field in [
            "connection",
            "operation",
            "preload_sequence",
            "registration",
            "credential_generation",
            "report",
        ] {
            let mut current = event.clone();
            current
                .as_object_mut()
                .unwrap()
                .insert(field.into(), aterm_json::json!("foreign"));
            assert!(
                !current_projection(&event, &current),
                "historical {field} does not identify the current work occurrence"
            );
        }
        assert!(!current_projection(
            &aterm_json::json!({}),
            &aterm_json::json!({})
        ));
    }
    #[test]
    fn loss_is_one_shot_and_only_matches_the_new_native_manager_report() {
        // Correlation metadata only, never a manufactured native Ready grant.
        let mut loss = ReadyLoss {
            member: "a".repeat(32),
            credential_generation: 4,
            previous_registration: "b".repeat(32),
            fired: None,
        };
        let accepted = aterm_json::json!({"action":"member-ready","member":"a".repeat(32),
            "credential_generation":4,"registration":"c".repeat(32),"connection":7,
            "operation":"d".repeat(32),"preload_sequence":"e".repeat(32),"report":[4,1,2,1]});
        for (field, value) in [
            ("action", aterm_json::json!("member-waiting")),
            ("member", aterm_json::json!("f".repeat(32))),
            ("credential_generation", aterm_json::json!(3)),
            ("registration", aterm_json::json!("b".repeat(32))),
        ] {
            let mut wrong = accepted.clone();
            wrong.as_object_mut().unwrap().insert(field.into(), value);
            assert!(!loss.accept(&wrong));
        }
        assert!(loss.accept(&accepted));
        assert_eq!(loss.fired.as_ref(), Some(&accepted));
        assert!(!loss.accept(&accepted), "retry cannot drop another reply");
    }
}
