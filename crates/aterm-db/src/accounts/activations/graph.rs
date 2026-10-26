// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// State of all locally retained root decisions for one account. None of these
/// states establishes current Commons membership or provider authentication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Empty,
    Pending,
    Invalid,
    Conflict(Vec<Digest>),
    Stopped(Digest),
    Ready(Digest),
}

pub(super) struct History {
    pub records: BTreeMap<Digest, Record>,
    pub requests: BTreeMap<(Actor, [u8; 16]), Vec<Digest>>,
    pub unavailable: usize,
    pub ignored: usize,
}

impl History {
    pub fn read(session: &Session<'_>, store: &Store, trust: Trust) -> Result<Self> {
        let mut history = Self {
            records: BTreeMap::new(),
            requests: BTreeMap::new(),
            unavailable: 0,
            ignored: 0,
        };
        for evidence in store.metadata_evidence(session)? {
            let Some(record) = evidence.record else {
                history.unavailable += 1;
                continue;
            };
            let payload = record.payload();
            let decoded = (|| {
                if payload.len() < 21 || &payload[..5] != b"AAOP1" {
                    return None;
                }
                let request: [u8; 16] = payload[5..21].try_into().ok()?;
                if request == [0; 16] {
                    return None;
                }
                let decision = Record::verify(&payload[21..], trust).ok()?;
                Some((request, decision))
            })();
            let Some((request, decision)) = decoded else {
                history.ignored += 1;
                continue;
            };
            history
                .requests
                .entry((record.header().actor, request))
                .or_default()
                .push(decision.id());
            history.records.insert(decision.id(), decision);
        }
        Ok(history)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountActivation",
            action = "Root",
            project = "aterm_db::accounts::activations::tests::activation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountActivation",
            action = "Stop",
            project = "aterm_db::accounts::activations::tests::activation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountActivation",
            action = "Resume",
            project = "aterm_db::accounts::activations::tests::activation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountActivation",
            action = "Fork",
            project = "aterm_db::accounts::activations::tests::activation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountActivation",
            action = "Merge",
            project = "aterm_db::accounts::activations::tests::activation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbAccountActivation",
            action = "Replay",
            project = "aterm_db::accounts::activations::tests::activation_transition"
        )
    )]
    pub fn status(&self, account: Digest) -> State {
        if self.unavailable > 0 {
            return State::Pending;
        }
        let nodes: BTreeMap<_, _> = self
            .records
            .iter()
            .filter(|(_, r)| r.request().account == account)
            .collect();
        if nodes.is_empty() {
            return State::Empty;
        }
        let mut heads: BTreeSet<_> = nodes.keys().map(|id| **id).collect();
        for record in nodes.values() {
            let request = record.request();
            let mut maximum = 0u64;
            for parent in &request.parents {
                let Some(previous) = self.records.get(parent) else {
                    return State::Pending;
                };
                if previous.request().account != account {
                    return State::Invalid;
                }
                maximum = maximum.max(previous.request().generation);
                heads.remove(parent);
            }
            if maximum.checked_add(1) != Some(request.generation) {
                return State::Invalid;
            }
        }
        if heads.len() != 1 {
            return State::Conflict(heads.into_iter().collect());
        }
        let head = *heads.first().unwrap();
        if matches!(self.records[&head].request().action, Action::Stop) {
            State::Stopped(head)
        } else {
            State::Ready(head)
        }
    }

    /// Explicit parents must be current heads. Joining a bounded subset permits
    /// a root to reduce more than 64 competing heads in several steps; no grant
    /// is usable until the complete history again has one valid head.
    pub fn check_next(&self, request: &Request) -> Result<()> {
        let heads: BTreeSet<_> = match self.status(request.account) {
            State::Empty => BTreeSet::new(),
            State::Ready(id) | State::Stopped(id) => BTreeSet::from([id]),
            State::Conflict(heads) => heads.into_iter().collect(),
            State::Pending | State::Invalid => {
                return Err(Error::new(
                    "account decision history is incomplete or invalid",
                ));
            }
        };
        let parents: BTreeSet<_> = request.parents.iter().copied().collect();
        if parents.is_empty() != heads.is_empty() || !parents.is_subset(&heads) {
            return Err(Error::new(
                "account decision must extend explicit current heads",
            ));
        }
        let maximum = parents
            .iter()
            .map(|id| self.records[id].request().generation)
            .max()
            .unwrap_or(0);
        if maximum.checked_add(1) != Some(request.generation) {
            return Err(Error::new(
                "account decision generation does not follow its parents",
            ));
        }
        Ok(())
    }

    pub fn retry(
        &self,
        actor: Actor,
        request: [u8; 16],
        wanted: &Request,
    ) -> Result<Option<&Record>> {
        let Some(ids) = self.requests.get(&(actor, request)) else {
            return Ok(None);
        };
        let first = &self.records[&ids[0]];
        if first.request() != wanted || ids.iter().any(|id| *id != first.id()) {
            return Err(Error::new(
                "account request was reused with another decision",
            ));
        }
        Ok(Some(first))
    }
}
