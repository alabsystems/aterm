// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Root-authorized account history transported by the ordinary astream table.
//! Relays confer no authority. All root decisions, including old stops, survive
//! withdrawal of the relaying writer's current data privileges.

use crate::{
    Error, Result,
    catalog::Descriptor,
    table::{
        Scope, Session,
        store::{Receipt, Store},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_secrets::{
    client::{Admin, Paths},
    commons::{
        accounts::{Action, Preparation as Terms, Record, Request, SecretRef},
        bootstrap::Bootstrap,
        journal::Trust,
    },
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) mod command;
mod configuration;
mod graph;
pub use configuration::Configuration;
use graph::History;
pub use graph::State;

pub struct Activations {
    paths: Paths,
}

/// A point-in-time authorization. It does not promise unseen offline decisions,
/// future membership or a provider's acceptance. Obtain a new observation before
/// each effect; merely retaining this object is never a renewable lease.
pub struct Preparation {
    decision: Record,
    configuration: Configuration,
    credential: Option<aterm_secrets::client::VerifiedEntry>,
}
impl Preparation {
    pub fn decision(&self) -> &Record {
        &self.decision
    }
    /// The exact owned proposal closure validated for this observation. A
    /// consumer must use these values rather than resolve a mutable key again.
    pub fn configuration(&self) -> &Configuration {
        &self.configuration
    }
    pub fn credential(&self) -> Option<&aterm_secrets::client::VerifiedEntry> {
        self.credential.as_ref()
    }
}

impl Activations {
    pub fn at(paths: Paths) -> Self {
        Self { paths }
    }

    pub fn initialize(&self) -> Result<Scope> {
        self.with_store(true, |_, _, scope| Ok(scope))
    }

    pub fn state(&self, account: Digest) -> Result<State> {
        self.with_store(false, |session, store, scope| {
            Ok(History::read(session, store, scope.trust())?.status(account))
        })
    }

    /// Resolve an existing, complete proposal and explicit current parent heads
    /// for root review. This proposal factory grants nothing and signs nothing.
    pub fn configuration(
        &self,
        admin: &Admin,
        account: Digest,
        key: &str,
        actor: Actor,
        parents: Vec<Digest>,
    ) -> Result<Request> {
        let snapshot = super::proposals::Proposals::at(self.paths.clone()).snapshot()?;
        let plan = super::plans::inspect(&snapshot, key)?;
        if plan["complete"].as_bool() != Some(true) {
            return Err(Error::new("account proposal is not complete"));
        }
        let value = &snapshot.cells[&("accounts".into(), key.into())].candidates[0].value;
        let digest = |field: &str| {
            crate::database::decode_hex::<32>(
                plan[field]
                    .as_str()
                    .ok_or_else(|| Error::new("account plan has no digest"))?,
            )
        };
        let secret = match value.get("secret_ref").filter(|v| !v.is_null()) {
            None => None,
            Some(value) => Some(SecretRef {
                vault: value["vault"]
                    .as_str()
                    .ok_or_else(|| Error::new("invalid secret vault"))?
                    .into(),
                signer: admin
                    .config
                    .signer
                    .ok_or_else(|| Error::new("account activation needs a signed vault"))?,
                entry: value["entry"]
                    .as_str()
                    .ok_or_else(|| Error::new("invalid secret entry"))?
                    .into(),
            }),
        };
        self.with_store(true, |session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let generation = next_generation(&history, &parents)?;
            let request = Request {
                account,
                generation,
                parents,
                action: Action::Prepare(Box::new(Terms {
                    table: snapshot.scope.digest(),
                    key: key.into(),
                    proposal: digest("proposal_digest")?,
                    configuration: digest("configuration_digest")?,
                    provider: value["provider"].as_str().unwrap().into(),
                    method: value["method"].as_str().unwrap().into(),
                    device: session.peer(actor)?,
                    secret,
                })),
            };
            Ok(request)
        })
    }

    pub fn stop_request(&self, account: Digest, parents: Vec<Digest>) -> Result<Request> {
        self.with_store(false, |session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let request = Request {
                account,
                generation: next_generation(&history, &parents)?,
                parents,
                action: Action::Stop,
            };
            Ok(request)
        })
    }

    /// Root decisions and their transport provenance remain readable after a
    /// stop. Ciphertext and credential commitments are omitted from this view.
    pub fn inspect(&self, account: Digest) -> Result<aterm_json::Value> {
        use aterm_codec::hex::encode_lower as hex;
        use aterm_json::json;
        crate::documents::application::observe_store(
            &self.paths,
            descriptor,
            |session, store, scope| {
                let history = History::read(session, store, scope.trust())?;
                let state = history.status(account);
                let (status, heads) = match state {
                    State::Empty => ("empty", vec![]),
                    State::Pending => ("pending", vec![]),
                    State::Invalid => ("invalid", vec![]),
                    State::Conflict(heads) => ("conflict", heads),
                    State::Stopped(head) => ("stopped", vec![head]),
                    State::Ready(head) => ("ready", vec![head]),
                };
                let decisions:Vec<_>=history.records.values().filter(|r|r.request().account==account).map(|r|json!({
                "decision":hex(&r.id()),"generation":r.request().generation,"parents":r.request().parents.iter().map(|p|hex(p)).collect::<Vec<_>>(),
                "kind":if matches!(r.request().action,Action::Stop){"stop"}else{"prepare"},"control":hex(&r.control()),
                "authored_at_unix_ms":r.authored_ms(),"time_source":"root-signed-author-claim"
            })).collect();
                let transport:Vec<_>=store.metadata_evidence(session)?.into_iter().filter_map(|e| {
                let payload=e.record.as_ref()?.payload();
                if payload.get(..5)!=Some(b"AAOP1") || payload.get(5..21)==Some(&[0;16]) {return None}
                let r=Record::verify(payload.get(21..)?,scope.trust()).ok()?;
                if r.request().account!=account {return None}
                Some(json!({"decision":hex(&r.id()),"operation":hex(&e.header.id),"relay":hex(&e.header.actor),"received_at_unix_ms":e.receipt.received_ms,"local_journal_offset":e.receipt.offset}))
            }).collect();
                Ok(
                    json!({"kind":"account-decisions","account":hex(&account),"table":hex(&scope.digest()),"state":status,
                "heads":heads.iter().map(|p|hex(p)).collect::<Vec<_>>(),"decisions":decisions,"transport":transport,"unavailable":history.unavailable,"ignored":history.ignored}),
                )
            },
        )
    }

    /// Publish one explicit root decision. A retry returns the stored decision
    /// before signing or encrypting again. The prepared bytes cannot escape the
    /// store's durable commit, so a lost reply never requires another ciphertext.
    pub fn issue(
        &self,
        admin: &Admin,
        request: [u8; 16],
        wanted: Request,
    ) -> Result<(Record, Receipt)> {
        if request == [0; 16] {
            return Err(Error::new("account publication needs a nonzero request"));
        }
        self.with_store(true, |session, store, scope| {
            session.writer()?;
            let history = History::read(session, store, scope.trust())?;
            if history.unavailable > 0 {
                return Err(Error::new("account history is not fully available"));
            }
            if let Some(decision) = history.retry(session.actor(), request, &wanted)? {
                let receipt = store
                    .metadata_evidence(session)?
                    .into_iter()
                    .find(|e| {
                        e.header.actor == session.actor()
                            && e.record.as_ref().is_some_and(|r| {
                                r.payload().get(..5) == Some(b"AAOP1")
                                    && r.payload().get(5..21) == Some(request.as_slice())
                                    && r.payload().get(21..) == Some(decision.bytes())
                            })
                    })
                    .map(|e| e.receipt)
                    .ok_or_else(|| Error::new("account publication receipt is unavailable"))?;
                return Ok((decision.clone(), receipt));
            }
            history.check_next(&wanted)?;
            let decision = session.issue_account(admin, wanted)?;
            let mut payload = b"AAOP1".to_vec();
            payload.extend_from_slice(&request);
            payload.extend_from_slice(decision.bytes());
            let receipt = store.commit_catalog(session, &payload)?;
            Ok((decision, receipt))
        })
    }

    /// Read fresh membership and the full retained root history, then return
    /// the exact bound credential bytes. No CLI login or provider request runs.
    pub fn prepare(
        &self,
        account: Digest,
        expected: &aterm_secrets::client::Config,
    ) -> Result<Preparation> {
        let observed = self.with_store(false, |session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let State::Ready(id) = history.status(account) else {
                return Err(Error::new("account has no complete preparation decision"));
            };
            session.permit_account(&history.records[&id])?;
            Ok(history.records[&id].clone())
        })?;
        let configuration = Configuration::capture(&self.paths, &observed)?;
        self.with_store(false, |session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let State::Ready(id) = history.status(account) else {
                return Err(Error::new(
                    "account has no unique complete preparation decision",
                ));
            };
            let decision = history.records[&id].clone();
            if id != observed.id() {
                return Err(Error::new("account decision changed during preparation"));
            }
            session.permit_account(&decision)?;
            let credential = match &decision.request().action {
                Action::Prepare(p) if p.secret.is_some() => Some(
                    aterm_secrets::client::read_account_bound_entry(
                        &self.paths,
                        expected,
                        &decision,
                    )
                    .map_err(crate::table::failure)?
                    .ok_or_else(|| {
                        Error::new("account credential does not match its signed binding")
                    })?,
                ),
                Action::Prepare(_) => None,
                Action::Stop => return Err(Error::new("account is stopped")),
            };
            Ok(Preparation {
                decision,
                configuration,
                credential,
            })
        })
    }

    fn with_store<T>(
        &self,
        create: bool,
        action: impl FnOnce(&Session<'_>, &mut Store, Scope) -> Result<T>,
    ) -> Result<T> {
        crate::documents::application::with_store(&self.paths, create, descriptor, action)
    }
}

fn descriptor(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), "accounts", "activations", 1)
}

fn next_generation(history: &History, parents: &[Digest]) -> Result<u64> {
    let mut maximum = 0;
    for id in parents {
        maximum = maximum.max(
            history
                .records
                .get(id)
                .ok_or_else(|| Error::new("account decision parent is not retained"))?
                .request()
                .generation,
        );
    }
    maximum
        .checked_add(1)
        .ok_or_else(|| Error::new("account decision generation exhausted"))
}

#[cfg(test)]
mod tests;
