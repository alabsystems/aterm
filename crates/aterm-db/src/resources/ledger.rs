// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use astream_wire::sync::Actor;
use std::collections::BTreeMap;

/// Fixed shares and conservative retained usage, not provider balance or a refund source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub allocation: Digest,
    pub used: BTreeMap<Actor, u64>,
    pub retained: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Debit {
    pub allocation: Digest,
    pub request: [u8; 16],
    pub cost: u64,
    pub cumulative: u64,
}
impl Debit {
    pub fn encode(&self) -> Vec<u8> {
        self.body(b"ARDB1")
    }
    pub fn encode_attempt(&self, operation: Digest) -> Vec<u8> {
        let mut bytes = self.body(b"ARDA1");
        bytes.extend_from_slice(&operation);
        bytes
    }
    fn body(&self, tag: &[u8]) -> Vec<u8> {
        let mut bytes = tag.to_vec();
        bytes.extend_from_slice(&self.allocation);
        bytes.extend_from_slice(&self.request);
        bytes.extend_from_slice(&self.cost.to_le_bytes());
        bytes.extend_from_slice(&self.cumulative.to_le_bytes());
        bytes
    }
    fn decode(bytes: &[u8]) -> Result<(Self, Option<Digest>)> {
        let mut read = crate::crypto::Reader::new(bytes);
        let attempt = match read.take(5)? {
            b"ARDB1" => false,
            b"ARDA1" => true,
            _ => return Err(Error::new("unknown resource debit format")),
        };
        let debit = Self {
            allocation: read.array()?,
            request: read.array()?,
            cost: u64::from_le_bytes(read.array()?),
            cumulative: u64::from_le_bytes(read.array()?),
        };
        let operation = if attempt { Some(read.array()?) } else { None };
        read.end()?;
        if debit.allocation == [0; 32]
            || debit.request == [0; 16]
            || debit.cost == 0
            || debit.cumulative == 0
            || operation == Some([0; 32])
        {
            return Err(Error::new("invalid resource debit"));
        }
        Ok((debit, operation))
    }
}

pub(super) struct Completion {
    pub allocation: Digest,
    pub request: [u8; 16],
    pub claim: Digest,
    pub observation: Observation,
}
impl Completion {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = b"ARDO1".to_vec();
        bytes.extend_from_slice(&self.allocation);
        bytes.extend_from_slice(&self.request);
        bytes.extend_from_slice(&self.claim);
        bytes.push(match self.observation.outcome() {
            Outcome::Completed => 0,
            Outcome::Rejected => 1,
        });
        bytes.extend_from_slice(&self.observation.evidence());
        bytes
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        let mut read = crate::crypto::Reader::new(bytes);
        if read.take(5)? != b"ARDO1" {
            return Err(Error::new("unknown dispatch observation format"));
        }
        let allocation = read.array()?;
        let request = read.array()?;
        let claim = read.array()?;
        let outcome = match read.byte()? {
            0 => Outcome::Completed,
            1 => Outcome::Rejected,
            _ => return Err(Error::new("invalid dispatch outcome")),
        };
        let observation = Observation::new(outcome, read.array()?)?;
        read.end()?;
        if allocation == [0; 32] || request == [0; 16] || claim == [0; 32] {
            return Err(Error::new("invalid dispatch observation identity"));
        }
        Ok(Self {
            allocation,
            request,
            claim,
            observation,
        })
    }
}
pub(super) struct Recorded {
    pub cost: u64,
    pub id: Digest,
    pub cumulative: u64,
    pub operation: Option<Digest>,
    pub observation: Option<Observation>,
}
pub(super) struct Ledger {
    used: BTreeMap<Actor, u64>,
    requests: BTreeMap<(Actor, [u8; 16]), Recorded>,
    retained: usize,
}
impl Ledger {
    pub fn read(allocation: &Allocation, session: &Session<'_>, store: &Store) -> Result<Self> {
        let mut records = store.liability_evidence(session)?;
        records.sort_by_key(|record| {
            (
                record.header().actor,
                record.header().sequence,
                record.header().id,
            )
        });
        let mut ledger = Self {
            used: BTreeMap::new(),
            requests: BTreeMap::new(),
            retained: records.len(),
        };
        for record in records {
            let actor = record.header().actor;
            if record.payload().starts_with(b"ARDO1") {
                let completion = Completion::decode(record.payload())?;
                let old = ledger
                    .requests
                    .get_mut(&(actor, completion.request))
                    .ok_or_else(|| Error::new("dispatch observation has no prior attempt"))?;
                if completion.allocation != allocation.id()
                    || old.operation.is_none()
                    || old.id != completion.claim
                    || old.observation.is_some()
                {
                    return Err(Error::new(
                        "dispatch observation conflicts with retained attempt",
                    ));
                }
                old.observation = Some(completion.observation);
                continue;
            }
            let (debit, operation) = Debit::decode(record.payload())?;
            if debit.allocation != allocation.id() {
                return Err(Error::new(
                    "retained resource debit has a different activation pin",
                ));
            }
            if let Some(old) = ledger.requests.get(&(actor, debit.request)) {
                if old.cost != debit.cost
                    || old.cumulative != debit.cumulative
                    || ledger.used(&actor) != debit.cumulative
                    || old.operation.is_some()
                    || operation.is_some()
                {
                    return Err(Error::new(
                        "retained resource request has conflicting debits",
                    ));
                }
                continue;
            }
            let limit = allocation
                .share(&actor)
                .ok_or_else(|| Error::new("retained debit writer has no resource share"))?;
            let previous = ledger.used(&actor);
            if previous.checked_add(debit.cost) != Some(debit.cumulative)
                || !aterm_db_core::admissible(limit, previous, debit.cumulative)
            {
                return Err(Error::new(
                    "retained resource accounting is inconsistent; capacity stays reserved",
                ));
            }
            ledger.used.insert(actor, debit.cumulative);
            ledger.requests.insert(
                (actor, debit.request),
                Recorded {
                    cost: debit.cost,
                    cumulative: debit.cumulative,
                    id: record.header().id,
                    operation,
                    observation: None,
                },
            );
        }
        Ok(ledger)
    }
    pub fn used(&self, actor: &Actor) -> u64 {
        self.used.get(actor).copied().unwrap_or(0)
    }
    pub fn request(&self, actor: &Actor, request: &[u8; 16]) -> Option<&Recorded> {
        self.requests.get(&(*actor, *request))
    }
    pub fn snapshot(&self, allocation: &Allocation) -> Snapshot {
        Snapshot {
            allocation: allocation.id(),
            used: self.used.clone(),
            retained: self.retained,
        }
    }
}
