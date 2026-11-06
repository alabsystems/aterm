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
mod intents;
pub use configuration::Configuration;
use graph::History;
pub use graph::State;

pub struct Activations {
    paths: Paths,
    deadline: Option<std::time::Instant>,
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
        Self {
            paths,
            deadline: None,
        }
    }

    /// Share one deadline across discovery, proposal checks, and durable issue.
    pub fn at_until(paths: Paths, deadline: std::time::Instant) -> Self {
        Self {
            paths,
            deadline: Some(deadline),
        }
    }

    fn proposals(&self) -> super::proposals::Proposals {
        match self.deadline {
            Some(deadline) => super::proposals::Proposals::at_until(self.paths.clone(), deadline),
            None => super::proposals::Proposals::at(self.paths.clone()),
        }
    }

    pub fn initialize(&self) -> Result<Scope> {
        self.with_store(true, |_, _, scope| Ok(scope))
    }

    pub fn state(&self, account: Digest) -> Result<State> {
        self.with_store(false, |session, store, scope| {
            Ok(History::read(session, store, scope.trust())?.status(account))
        })
    }

    /// Read the current root decision and its exact proposal closure. This
    /// carries no credential or permission to execute; use `prepare` for an
    /// addressed credential and recheck authority before every effect.
    pub fn current_configuration(&self, account: Digest) -> Result<(Record, Configuration)> {
        self.current_configuration_after(account, || {})
    }

    /// Observe one current root configuration and its currently admitted
    /// prepared devices with two shared history passes and one configuration
    /// capture, independent of fleet size.
    /// This is metadata only; consumers still call `prepare` before key use.
    pub fn current_configuration_members(
        &self,
        account: Digest,
    ) -> Result<(
        Record,
        Configuration,
        Vec<aterm_secrets::commons::binding::Binding>,
    )> {
        self.current_configuration_members_after(account, || {})
    }

    fn current_configuration_members_after(
        &self,
        account: Digest,
        after: impl FnOnce(),
    ) -> Result<(
        Record,
        Configuration,
        Vec<aterm_secrets::commons::binding::Binding>,
    )> {
        let (record, members, configuration) = self.observe_store(|session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let statuses = history.statuses();
            let Some(State::Ready(head)) = statuses.get(&account) else {
                return Err(Error::new("account has no current decision"));
            };
            let record = history.records[head].clone();
            intents::current(&history, &record)?;
            let members = prepared_members(session, &history, &statuses, &record)?;
            let configuration = Configuration::capture_in_session(
                session,
                &record,
                self.deadline.unwrap_or_else(crate::admission::deadline),
            )?;
            Ok((record, members, configuration))
        })?;
        after();
        self.observe_store(|session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let statuses = history.statuses();
            if statuses.get(&account) != Some(&State::Ready(record.id())) {
                return Err(Error::new("account changed during member observation"));
            }
            intents::current(&history, &record)?;
            if prepared_members(session, &history, &statuses, &record)? != members {
                return Err(Error::new("prepared members changed during observation"));
            }
            Ok(())
        })?;
        Ok((record, configuration, members.into_keys().collect()))
    }

    fn current_configuration_after(
        &self,
        account: Digest,
        after_capture: impl FnOnce(),
    ) -> Result<(Record, Configuration)> {
        let (record, configuration) = self.observe_store(|session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let State::Ready(head) = history.status(account) else {
                return Err(Error::new("account has no unique current root decision"));
            };
            let record = history.records[&head].clone();
            intents::current(&history, &record)?;
            let configuration = Configuration::capture_in_session(
                session,
                &record,
                self.deadline.unwrap_or_else(crate::admission::deadline),
            )?;
            Ok((record, configuration))
        })?;
        after_capture();
        self.observe_store(|session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            if history.status(account) != State::Ready(record.id()) {
                return Err(Error::new("account decision changed during observation"));
            }
            intents::current(&history, &record)?;
            Ok(())
        })?;
        Ok((record, configuration))
    }

    /// Current unambiguous preparations addressed to this installation. This
    /// supplies discovery, not reusable permission: consumers must call
    /// `prepare` again before each effect and retain their consumption floor.
    /// An observation first asks whether any current decision names this
    /// device; where none does, including where no Mac declared the table,
    /// discovery is empty without the control writer each permit requires.
    pub fn local_preparations(&self) -> Result<Vec<(Digest, Digest)>> {
        let named = self.observe_store_if_declared(|session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            Ok(!addressed(&history, session.actor()).is_empty())
        })?;
        if named != Some(true) {
            return Ok(Vec::new());
        }
        self.with_store(false, |session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            Ok(addressed(&history, session.actor())
                .into_iter()
                .filter(|(_, decision)| session.permit_account(&history.records[decision]).is_ok())
                .collect())
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
        let snapshot = self.proposals().snapshot()?;
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
                    intent: None,
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
        self.observe_store(
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
                "kind":match &r.request().action { Action::Stop=>"stop", Action::Intent(_)=>"intent", Action::Prepare(_)=>"prepare" },"control":hex(&r.control()),
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
        self.issue_in(admin, request, wanted, false)
    }

    fn issue_in(
        &self,
        admin: &Admin,
        request: [u8; 16],
        wanted: Request,
        automatic: bool,
    ) -> Result<(Record, Receipt)> {
        if request == [0; 16] {
            return Err(Error::new("account publication needs a nonzero request"));
        }
        if matches!(&wanted.action, Action::Intent(_))
            || matches!(&wanted.action, Action::Prepare(p) if p.intent.is_some())
        {
            Configuration::capture_request_until(&self.paths, &wanted, self.deadline)?;
        }
        self.with_store(true, |session, store, scope| {
            session.writer()?;
            let history = History::read(session, store, scope.trust())?;
            if history.unavailable > 0 {
                return Err(Error::new("account history is not fully available"));
            }
            intents::namespace(&history, &wanted)?;
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
            let decision = match intents::current_request(&history, &wanted)? {
                Some(intent) if automatic && !intents::direct_issuer(session, intent)? => session
                    .issue_account_with_recovered_issuer(
                    &self.paths,
                    admin,
                    wanted,
                    intent,
                    self.deadline.unwrap_or_else(crate::admission::deadline),
                )?,
                Some(intent) => session.issue_account_with_intent(admin, wanted, intent)?,
                None => session.issue_account(admin, wanted)?,
            };
            let mut payload = Vec::with_capacity(21 + decision.bytes().len());
            payload.extend_from_slice(b"AAOP1");
            payload.extend_from_slice(&request);
            payload.extend_from_slice(decision.bytes());
            let receipt = crate::documents::application::append(session, store, &payload)?;
            Ok((decision, receipt))
        })
    }

    /// Read fresh membership and the full retained root history, then return
    /// the exact bound credential bytes from the active writer's recipient.
    /// A successor still requires the caller's original configured source.
    /// No CLI login or provider request runs.
    pub fn prepare(
        &self,
        account: Digest,
        expected: &aterm_secrets::client::Config,
    ) -> Result<Preparation> {
        let (observed, paths, configuration) =
            self.with_store(false, |session, store, scope| {
                let history = History::read(session, store, scope.trust())?;
                let State::Ready(id) = history.status(account) else {
                    return Err(Error::new("account has no complete preparation decision"));
                };
                intents::current(&history, &history.records[&id])?;
                session.permit_account(&history.records[&id])?;
                let observed = history.records[&id].clone();
                let configuration = Configuration::capture_in_session(
                    session,
                    &observed,
                    self.deadline.unwrap_or_else(crate::admission::deadline),
                )?;
                Ok((observed, session.paths().clone(), configuration))
            })?;
        let recovered = paths != self.paths;
        let caller_current = || -> Result<()> {
            if aterm_secrets::client::config(&self.paths)
                .map_err(crate::table::failure)?
                .as_ref()
                != Some(expected)
            {
                return Err(Error::new(
                    "configured vault changed during account preparation",
                ));
            }
            Ok(())
        };
        let credential_config = if recovered
            && matches!(&observed.request().action, Action::Prepare(p) if p.secret.is_some())
        {
            caller_current()?;
            aterm_secrets::client::config(&paths)
                .map_err(crate::table::failure)?
                .ok_or_else(|| Error::new("account credential vault is not configured"))?
        } else {
            expected.clone()
        };
        self.with_store(false, |session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let State::Ready(id) = history.status(account) else {
                return Err(Error::new(
                    "account has no unique complete preparation decision",
                ));
            };
            let decision = history.records[&id].clone();
            intents::current(&history, &decision)?;
            if id != observed.id() {
                return Err(Error::new("account decision changed during preparation"));
            }
            session.permit_account(&decision)?;
            let credential = match &decision.request().action {
                Action::Prepare(p) if p.secret.is_some() => {
                    if recovered {
                        caller_current()?;
                    }
                    let entry = aterm_secrets::client::read_account_bound_entry(
                        &paths,
                        &credential_config,
                        &decision,
                    )
                    .map_err(crate::table::failure)?;
                    if recovered {
                        caller_current()?;
                    }
                    Some(entry.ok_or_else(|| {
                        Error::new("account credential does not match its signed binding")
                    })?)
                }
                Action::Prepare(_) => None,
                Action::Stop | Action::Intent(_) => {
                    return Err(Error::new("account is not a device preparation"));
                }
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
        match self.deadline {
            Some(deadline) => crate::documents::application::with_store_until(
                &self.paths,
                create,
                descriptor,
                deadline,
                action,
            ),
            None => {
                crate::documents::application::with_store(&self.paths, create, descriptor, action)
            }
        }
    }

    fn observe_store<T>(
        &self,
        action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
    ) -> Result<T> {
        match self.deadline {
            Some(deadline) => crate::documents::application::observe_store_until(
                &self.paths,
                descriptor,
                deadline,
                action,
            ),
            None => crate::documents::application::observe_store(&self.paths, descriptor, action),
        }
    }

    /// [`Self::observe_store`], `None` where no Mac declared the table.
    fn observe_store_if_declared<T>(
        &self,
        action: impl FnOnce(&Session<'_>, &Store, Scope) -> Result<T>,
    ) -> Result<Option<T>> {
        match self.deadline {
            Some(deadline) => crate::documents::application::observe_store_if_declared_until(
                &self.paths,
                descriptor,
                deadline,
                action,
            ),
            None => crate::documents::application::observe_store_if_declared(
                &self.paths,
                descriptor,
                action,
            ),
        }
    }
}

fn descriptor(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), "accounts", "activations", 1)
}

/// Each account whose unique current decision prepares the device `actor`, with
/// that decision: the most a permit can admit, since it requires the same device.
fn addressed(history: &History, actor: Actor) -> Vec<(Digest, Digest)> {
    let statuses = history.statuses();
    statuses
        .iter()
        .filter_map(|(&account, state)| {
            let &State::Ready(decision) = state else {
                return None;
            };
            let record = &history.records[&decision];
            let Action::Prepare(terms) = &record.request().action else {
                return None;
            };
            if terms.device.actor() != actor {
                return None;
            }
            intents::current_with_statuses(history, &statuses, record).ok()?;
            Some((account, decision))
        })
        .collect()
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

fn prepared_members(
    session: &Session<'_>,
    history: &History,
    statuses: &BTreeMap<Digest, State>,
    record: &Record,
) -> Result<BTreeMap<aterm_secrets::commons::binding::Binding, Digest>> {
    let incompatible = if matches!(record.request().action, Action::Intent(_)) {
        intents::invalid_device_namespaces(history, record.request().account)
    } else {
        BTreeSet::new()
    };
    let current = session.observed_account_targets()?;
    let mut counts = BTreeMap::new();
    for member in &current {
        *counts.entry(*member.machine()).or_insert(0usize) += 1;
    }
    let mut result = BTreeMap::new();
    for member in current {
        if counts[member.machine()] != 1 {
            continue;
        }
        match &record.request().action {
            Action::Prepare(terms) if terms.device == member => {
                result.insert(member, record.id());
            }
            Action::Intent(intent) => {
                let reference = aterm_secrets::commons::accounts::IntentRef {
                    account: record.request().account,
                    decision: record.id(),
                };
                let account = reference.device_account(&member);
                if incompatible.contains(&account) {
                    continue;
                }
                let Some(State::Ready(head)) = statuses.get(&account) else {
                    continue;
                };
                let child = &history.records[head];
                if matches!(&child.request().action, Action::Prepare(terms) if terms.intent == Some(reference) && terms.device == member && intent.matches(terms))
                {
                    result.insert(member, *head);
                }
            }
            _ => {}
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod frontier_tests;
