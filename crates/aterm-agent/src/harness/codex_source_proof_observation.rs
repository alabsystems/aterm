// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Metadata-only observations for the explicit joined protocol fixture. No
//! observation can construct a source, close a use, or authorize publication.
use super::*;

pub const SOURCE_PROOF_OBSERVATION_LIMIT: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SourceProofObservation {
    pub issued: usize,
    pub member_highwater: u64,
    pub member_epoch: String,
    pub members: usize,
    pub managers_ready: usize,
    pub readers_ready: usize,
    pub loaded_readers: Vec<String>,
    pub unresolved: usize,
    pub unsettled_custody: usize,
    pub active_uses: usize,
    pub pending_uses: usize,
    pub ending_uses: usize,
    pub maximum_credential_generation: u64,
    pub ready_members: Vec<SourceProofMember>,
    pub uses: Vec<SourceProofUse>,
    pub derived: Vec<SourceProofDerived>,
}

impl SourceProofObservation {
    pub fn closed(&self) -> bool {
        self.unresolved == 0
            && self.unsettled_custody == 0
            && self.issued == 0
            && self.members == 0
            && self.active_uses == 0
            && self.pending_uses == 0
            && self.ending_uses == 0
    }
}

impl Registry {
    /// Observe the actual same kernel actor and every native member/use, not a
    /// child-supplied member list. Process and gate probes run outside State.
    pub fn observe_source_proof(
        &self,
        caller: &slot::Caller,
    ) -> Result<SourceProofObservation, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Stale)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        let (keys, issued, state_version, uses, mut result) = {
            let state = lock(&self.state)?;
            let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
            if actor.process != process {
                return Err(Error::Stale);
            }
            let keys = state
                .members
                .iter()
                .filter(|(_, member)| &member.caller == caller)
                .map(|(key, member)| (key.clone(), MemberSnapshot::capture(member)))
                .collect::<Vec<_>>();
            let mut result = SourceProofObservation {
                issued: actor.issued.len(),
                member_highwater: actor.highwater,
                member_epoch: actor.epoch.clone(),
                members: keys.len(),
                managers_ready: 0,
                readers_ready: 0,
                loaded_readers: Vec::new(),
                unresolved: actor
                    .issued
                    .keys()
                    .filter(|member| {
                        member_key(caller, member)
                            .map_or(true, |key| !state.members.contains_key(&key))
                    })
                    .count(),
                active_uses: 0,
                pending_uses: 0,
                ending_uses: 0,
                unsettled_custody: unsettled(&state, caller),
                maximum_credential_generation: 0,
                ready_members: Vec::new(),
                uses: Vec::new(),
                derived: Vec::new(),
            };
            (result.active_uses, result.pending_uses, result.ending_uses) =
                reservation::ProofUses::counts(&state, caller);
            (
                keys,
                actor.issued.keys().cloned().collect::<Vec<_>>(),
                state.version,
                reservation::ProofUses::capture(&state, caller)?,
                result,
            )
        };
        if !uses.observed() {
            return Err(Error::Stale);
        }
        for (key, _) in &keys {
            let member = key.rsplit(':').next().ok_or(Error::Stale)?;
            if let Ok(proof) = self.ready_source(caller, member)
                && proof.current()
            {
                result.managers_ready += 1;
                result.maximum_credential_generation = result
                    .maximum_credential_generation
                    .max(proof.source.credential_generation());
            } else if let Ok(proof) = self.reader_source(caller, member)
                && proof.current()
            {
                result.readers_ready += 1;
                result.loaded_readers.push(member.to_owned());
                result.maximum_credential_generation = result
                    .maximum_credential_generation
                    .max(proof.source.credential_generation());
            } else {
                result.unresolved += 1;
            }
        }
        if Process::observe(caller.pid).as_ref() != Some(&process) {
            return Err(Error::Stale);
        }
        let state = lock(&self.state)?;
        let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
        if state.version != state_version
            || actor.highwater != result.member_highwater
            || actor.epoch != result.member_epoch
            || actor.process != process
            || !actor.issued.keys().eq(issued.iter())
            || keys
                .iter()
                .any(|(key, version)| state.members.get(key).is_none_or(|m| !version.matches(m)))
            || state
                .members
                .values()
                .filter(|member| &member.caller == caller)
                .count()
                != keys.len()
        {
            return Err(Error::Busy);
        }
        let totals = reservation::ProofUses::counts(&state, caller);
        if totals != (result.active_uses, result.pending_uses, result.ending_uses) {
            return Err(Error::Busy);
        }
        if !uses.matches(&state, caller) {
            return Err(Error::Busy);
        }
        (result.uses, result.derived) = uses.metadata();
        for (key, snapshot) in &keys {
            if !snapshot.unknown
                && snapshot.live
                && let Some(ReadyRole::Manager(report)) = snapshot.role
            {
                result.ready_members.push(SourceProofMember {
                    operation: snapshot.operation.clone(),
                    preload_sequence: state
                        .preload_sequences
                        .get(key)
                        .and_then(|ledger| {
                            ledger.proof_sequence(
                                &snapshot.operation,
                                &snapshot.registration,
                                snapshot.role,
                            )
                        })
                        .ok_or(Error::Stale)?,
                    member: key.rsplit(':').next().ok_or(Error::Stale)?.to_owned(),
                    connection: snapshot.connection.id,
                    registration: snapshot.registration.clone(),
                    credential_generation: snapshot.source.credential_generation(),
                    report: [report.generation, report.owner, report.source, report.login],
                });
            }
        }
        if unsettled(&state, caller) != result.unsettled_custody {
            return Err(Error::Busy);
        }
        Ok(result)
    }
}

// These captured allocations cannot be relabeled by a work-rebind which keeps
// the credential version unchanged. Holding an Arc here is observation only.
struct MemberSnapshot {
    process: Process,
    operation: String,
    registration: String,
    connection: Connection,
    live: bool,
    version: u64,
    source: Arc<slot::SourceHandle>,
    unknown: bool,
    role: Option<ReadyRole>,
    reader_owned: bool,
    identity: crate::harness::codex_source_gate::SourceGateIdentity,
    gate: Option<ReaderGateSnapshot>,
}
enum ReaderGateSnapshot {
    Global(Arc<SourceWorkLease>),
    Target(Arc<crate::harness::codex_source_gate::TargetFamilyLease>),
}
impl MemberSnapshot {
    fn capture(m: &Member) -> Self {
        Self {
            process: m.process.clone(),
            operation: m.operation.clone(),
            registration: m.registration.clone(),
            connection: m.connection.clone(),
            live: m.connection.is_live(),
            version: m.version,
            source: Arc::clone(&m.source),
            unknown: m.unknown,
            role: m.readiness.role,
            reader_owned: m.readiness.reader_owned,
            identity: m.readiness.identity.clone(),
            gate: m.readiness.gate.as_ref().map(|gate| match gate {
                ReaderGate::Global(gate) => ReaderGateSnapshot::Global(Arc::clone(gate)),
                ReaderGate::Target(gate) => ReaderGateSnapshot::Target(Arc::clone(gate)),
            }),
        }
    }
    fn matches(&self, m: &Member) -> bool {
        self.process == m.process
            && self.operation == m.operation
            && self.registration == m.registration
            && self.connection.same(&m.connection)
            && self.live == m.connection.is_live()
            && self.version == m.version
            && Arc::ptr_eq(&self.source, &m.source)
            && self.unknown == m.unknown
            && self.role == m.readiness.role
            && self.reader_owned == m.readiness.reader_owned
            && self.identity == m.readiness.identity
            && match (&self.gate, &m.readiness.gate) {
                (None, None) => true,
                (Some(ReaderGateSnapshot::Global(a)), Some(ReaderGate::Global(b))) => {
                    Arc::ptr_eq(a, b)
                }
                (Some(ReaderGateSnapshot::Target(a)), Some(ReaderGate::Target(b))) => {
                    Arc::ptr_eq(a, b)
                }
                _ => false,
            }
    }
}
fn unsettled(state: &State, caller: &slot::Caller) -> usize {
    let prefix = format!("{}:", actor_key(caller));
    state
        .plain_retiring
        .keys()
        .filter(|key| key.starts_with(&prefix))
        .count()
        + state
            .preloads
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .count()
        + state
            .unrelated
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .count()
        + state
            .preload_sequences
            .iter()
            .filter(|(key, ledger)| key.starts_with(&prefix) && !ledger.proof_closed())
            .count()
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SourceProofMember {
    pub member: String,
    pub operation: String,
    pub preload_sequence: String,
    pub connection: u64,
    pub registration: String,
    pub credential_generation: u64,
    pub report: [u64; 4],
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SourceProofUse {
    pub member: String,
    pub nonce: String,
    pub manager: bool,
    pub reader: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SourceProofDerived {
    pub child: String,
    pub parent: String,
    pub nonce: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_counts_do_not_hide_unresolved_or_release_custody() {
        let mut sample = SourceProofObservation {
            issued: 0,
            member_highwater: 0,
            member_epoch: String::new(),
            members: 0,
            managers_ready: 0,
            readers_ready: 0,
            loaded_readers: Vec::new(),
            unresolved: 0,
            unsettled_custody: 0,
            active_uses: 0,
            pending_uses: 0,
            ending_uses: 0,
            maximum_credential_generation: 0,
            ready_members: Vec::new(),
            uses: Vec::new(),
            derived: Vec::new(),
        };
        assert!(sample.closed());
        sample.unresolved = 1;
        assert!(!sample.closed());
        sample.unresolved = 0;
        sample.unsettled_custody = 1;
        assert!(!sample.closed());
    }
    #[test]
    fn maximum_registry_metadata_and_receipts_fit_native_bound() {
        let word = "f".repeat(32);
        let sample = SourceProofObservation {
            issued: 128,
            member_highwater: u64::MAX,
            member_epoch: "f".repeat(16),
            members: 128,
            managers_ready: 128,
            readers_ready: 128,
            loaded_readers: vec![word.clone(); 128],
            unresolved: 128,
            unsettled_custody: 4096,
            active_uses: 512,
            pending_uses: 512,
            ending_uses: 512,
            maximum_credential_generation: u64::MAX,
            ready_members: (0..128)
                .map(|_| SourceProofMember {
                    member: word.clone(),
                    operation: word.clone(),
                    preload_sequence: word.clone(),
                    connection: u64::MAX,
                    registration: word.clone(),
                    credential_generation: u64::MAX,
                    report: [u64::MAX; 4],
                })
                .collect(),
            uses: (0..512)
                .map(|_| SourceProofUse {
                    member: word.clone(),
                    nonce: word.clone(),
                    manager: true,
                    reader: true,
                })
                .collect(),
            derived: (0..128)
                .map(|_| SourceProofDerived {
                    child: word.clone(),
                    parent: word.clone(),
                    nonce: word.clone(),
                })
                .collect(),
        };
        let event = aterm_json::json!({"action":"member-rebound-ready","request":word,"operation":word,
            "member":word,"registration":word,"preload_sequence":word,"credential_generation":u64::MAX,
            "report":vec![u64::MAX;4],"connection":u64::MAX,"sequence":u64::MAX});
        let encoded = aterm_json::to_vec(
            &aterm_json::json!({"schema":1,"request":word,"action":"observe",
            "observation":sample,"outcomes":vec![event;32],"outcomeSequence":u64::MAX,
            "readySeen":true,"closedSeen":false,"generation":u64::MAX,"outcome":"ok"}),
        )
        .unwrap();
        assert!(encoded.len() > 4096);
        assert!(encoded.len() <= SOURCE_PROOF_OBSERVATION_LIMIT);
    }
}
