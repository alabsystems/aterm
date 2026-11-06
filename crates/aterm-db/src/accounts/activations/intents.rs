// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One explicit root intent, independent preparation histories for each device.

use super::*;
use aterm_secrets::commons::accounts::{Intent, IntentRef};

#[derive(PartialEq, Eq)]
enum Namespace {
    Legacy,
    Intent,
    Device(Digest),
}

fn kind(action: &Action) -> Option<Namespace> {
    match action {
        Action::Stop => None,
        Action::Intent(_) => Some(Namespace::Intent),
        Action::Prepare(p) => Some(
            p.intent
                .map_or(Namespace::Legacy, |i| Namespace::Device(i.account)),
        ),
    }
}

/// Index incompatible device namespaces once for a whole fleet observation.
pub(super) fn invalid_device_namespaces(history: &History, parent: Digest) -> BTreeSet<Digest> {
    history
        .records
        .values()
        .filter_map(|record| {
            kind(&record.request().action)
                .filter(|found| *found != Namespace::Device(parent))
                .map(|_| record.request().account)
        })
        .collect()
}

/// Older readers ignore new action tags. Never put one in a history where
/// ignoring it could leave an older, legacy device authorization usable.
pub(super) fn namespace(history: &History, request: &Request) -> Result<()> {
    if let Some(expected) = kind(&request.action)
        && history
            .records
            .values()
            .filter(|record| record.request().account == request.account)
            .filter_map(|record| kind(&record.request().action))
            .any(|found| found != expected)
    {
        return Err(Error::new(
            "fleet intents and device approvals need distinct account histories",
        ));
    }
    Ok(())
}

pub(super) fn current<'a>(history: &'a History, decision: &Record) -> Result<Option<&'a Record>> {
    current_request(history, decision.request())
}

pub(super) fn current_with_statuses<'a>(
    history: &'a History,
    statuses: &BTreeMap<Digest, State>,
    decision: &Record,
) -> Result<Option<&'a Record>> {
    let request = decision.request();
    namespace(history, request)?;
    let Action::Prepare(preparation) = &request.action else {
        return Ok(None);
    };
    let Some(reference) = preparation.intent else {
        return Ok(None);
    };
    if statuses.get(&reference.account) != Some(&State::Ready(reference.decision)) {
        return Err(Error::new(
            "fleet account intent is stopped, changed or unavailable",
        ));
    }
    let intent = &history.records[&reference.decision];
    let Action::Intent(terms) = &intent.request().action else {
        return Err(Error::new("account parent is not a fleet intent"));
    };
    if reference.device_account(&preparation.device) != request.account
        || !terms.matches(preparation)
    {
        return Err(Error::new(
            "device preparation differs from its fleet intent",
        ));
    }
    Ok(Some(intent))
}

pub(super) fn current_request<'a>(
    history: &'a History,
    request: &Request,
) -> Result<Option<&'a Record>> {
    namespace(history, request)?;
    let Action::Prepare(preparation) = &request.action else {
        return Ok(None);
    };
    let Some(reference) = preparation.intent else {
        return Ok(None);
    };
    if history.status(reference.account) != State::Ready(reference.decision) {
        return Err(Error::new(
            "fleet account intent is stopped, changed or unavailable",
        ));
    }
    let intent = &history.records[&reference.decision];
    let Action::Intent(terms) = &intent.request().action else {
        return Err(Error::new("account parent is not a fleet intent"));
    };
    if reference.device_account(&preparation.device) != request.account
        || !terms.matches(preparation)
    {
        return Err(Error::new(
            "device preparation differs from its fleet intent",
        ));
    }
    Ok(Some(intent))
}

pub(super) fn direct_issuer(session: &Session<'_>, record: &Record) -> Result<bool> {
    let Action::Intent(terms) = &record.request().action else {
        return Ok(false);
    };
    let issuer = session.peer_writer(session.actor())?;
    let count = session
        .account_targets()?
        .iter()
        .filter(|target| target.machine() == issuer.machine())
        .count();
    Ok(terms.issued_by(&issuer, count))
}

fn automatic_issuer(
    record: &Record,
    issuer: &aterm_secrets::commons::binding::Binding,
    count: usize,
    lineage: &[aterm_secrets::commons::binding::Binding],
) -> bool {
    let Action::Intent(terms) = &record.request().action else {
        return false;
    };
    terms.issued_by(issuer, count)
        || (count == 1
            && lineage.last() == Some(issuer)
            && lineage[..lineage.len().saturating_sub(1)].contains(&terms.issuer))
}

impl Activations {
    /// Current addressed recovery candidates from one read-only root snapshot.
    /// This avoids scheduling obsolete notices; issuance still checks current
    /// authority, configuration and membership again before any mutation.
    pub(in crate::accounts) fn current_recoveries(
        &self,
        requests: &[crate::accounts::recovery::Request],
    ) -> Result<Vec<crate::accounts::recovery::Request>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        self.observe_store(|session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let statuses = history.statuses();
            Ok(requests
                .iter()
                .filter(|request| {
                    if statuses.get(&request.account()) != Some(&State::Ready(request.decision())) {
                        return false;
                    }
                    let record = &history.records[&request.decision()];
                    let Action::Prepare(preparation) = &record.request().action else {
                        return false;
                    };
                    &preparation.device == request.requester()
                        && session
                            .peer_writer(request.requester().actor())
                            .is_ok_and(|binding| &binding == request.requester())
                        && current_with_statuses(&history, &statuses, record)
                            .is_ok_and(|intent| intent.is_some())
                })
                .cloned()
                .collect())
        })
    }

    /// Materialize at most one missing device approval per automatic upkeep
    /// pass. An invalid target cannot prevent other admitted machines advancing.
    pub fn materialize_next_intent(&self, admin: &Admin) -> Result<Option<(Record, Receipt)>> {
        let mut first_error = None;
        for (intent, actor) in self.intent_targets_in(true)? {
            let next = self
                .intent_member_request_in(intent, actor, true)
                .and_then(|request| {
                    let Some(request) = request else {
                        return Ok(None);
                    };
                    let Action::Prepare(terms) = &request.action else {
                        unreachable!()
                    };
                    let reference = terms.intent.unwrap();
                    let mut hash = aterm_digest::Sha256::new();
                    hash.update(b"aterm/commons/intent-materialization/1\0");
                    hash.update(reference.account);
                    hash.update(reference.decision);
                    hash.update(request.account);
                    hash.update(request.generation.to_le_bytes());
                    hash.update(terms.device.actor());
                    hash.update(terms.device.recipient().0);
                    for parent in &request.parents {
                        hash.update(parent);
                    }
                    let digest = hash.finalize();
                    let nonce = digest[..16].try_into().unwrap();
                    self.issue_in(admin, nonce, request, true).map(Some)
                });
            match next {
                Ok(Some(issued)) => return Ok(Some(issued)),
                Ok(None) => {}
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    /// Resolve a complete proposal for an explicit root decision authorizing
    /// preparation on current and future admitted machines. This signs nothing.
    pub fn intent_configuration(
        &self,
        admin: &Admin,
        account: Digest,
        key: &str,
        parents: Vec<Digest>,
    ) -> Result<Request> {
        let actor = self.with_store(true, |session, _, _| Ok(session.actor()))?;
        let mut request = self.configuration(admin, account, key, actor, parents)?;
        let Action::Prepare(preparation) = request.action else {
            unreachable!()
        };
        request.action = Action::Intent(Box::new(Intent {
            table: preparation.table,
            key: preparation.key,
            proposal: preparation.proposal,
            configuration: preparation.configuration,
            provider: preparation.provider,
            method: preparation.method,
            issuer: preparation.device,
            secret: preparation.secret,
        }));
        Ok(request)
    }

    /// Current root intents and unique admitted writer targets. This metadata
    /// is only work discovery; issuing and using a child rechecks every fence.
    pub fn intent_targets(&self) -> Result<Vec<(Digest, Actor)>> {
        self.intent_targets_in(false)
    }

    fn intent_targets_in(&self, automatic: bool) -> Result<Vec<(Digest, Actor)>> {
        self.with_store(false, |session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let targets = session.account_targets()?;
            let mut by_machine = BTreeMap::new();
            for target in &targets {
                *by_machine.entry(*target.machine()).or_insert(0usize) += 1;
            }
            let issuer = session.peer_writer(session.actor())?;
            let issuer_count = by_machine.get(issuer.machine()).copied().unwrap_or(0);
            let lineage = if automatic {
                crate::generation::lineage_in(
                    &self.paths,
                    session,
                    self.deadline
                        .unwrap_or_else(crate::admission::deadline),
                )?
            } else {
                Vec::new()
            };
            let accounts: BTreeSet<_> = history
                .records
                .values()
                .filter_map(|record| {
                    matches!(record.request().action, Action::Intent(_))
                        .then_some(record.request().account)
                })
                .collect();
            let mut result = Vec::new();
            let statuses = history.statuses();
            let prepared: BTreeSet<_> = history.records.values().filter_map(|record| {
                let Action::Prepare(preparation) = &record.request().action else { return None };
                preparation.intent.map(|reference| (record.request().account, reference.account, reference.decision))
            }).collect();
            for account in accounts {
                let Some(State::Ready(head)) = statuses.get(&account) else {
                    continue;
                };
                if !automatic_issuer(&history.records[head], &issuer, issuer_count, &lineage) {
                    continue;
                }
                let reference = IntentRef { account, decision: *head };
                for target in &targets {
                    let child = reference.device_account(target);
                    let retained = match statuses.get(&child) {
                        Some(State::Ready(id)) => matches!(&history.records[id].request().action,
                            Action::Prepare(p) if p.intent == Some(reference) && p.device == *target),
                        Some(State::Stopped(_)) => prepared.contains(&(child, reference.account, reference.decision)),
                        _ => false,
                    };
                    if by_machine[target.machine()] == 1 && (!automatic || !retained) {
                        result.push((account, target.actor()));
                    }
                }
            }
            Ok(result)
        })
    }

    /// Build the missing device decision for one current intent. Existing
    /// decisions for this revision, including a later per-device Stop, stay put.
    pub fn intent_member_request(
        &self,
        intent_account: Digest,
        actor: Actor,
    ) -> Result<Option<Request>> {
        self.intent_member_request_in(intent_account, actor, false)
    }

    pub(super) fn intent_member_request_in(
        &self,
        intent_account: Digest,
        actor: Actor,
        automatic: bool,
    ) -> Result<Option<Request>> {
        self.with_store(false, |session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let State::Ready(head) = history.status(intent_account) else {
                return Err(Error::new("fleet account intent is not current"));
            };
            let Action::Intent(terms) = &history.records[&head].request().action else {
                return Err(Error::new("account is not a fleet intent"));
            };
            let targets = session.account_targets()?;
            let issuer = session.peer_writer(session.actor())?;
            let issuer_count = targets
                .iter()
                .filter(|target| target.machine() == issuer.machine())
                .count();
            let lineage = if automatic {
                crate::generation::lineage_in(
                    &self.paths,
                    session,
                    self.deadline.unwrap_or_else(crate::admission::deadline),
                )?
            } else {
                Vec::new()
            };
            if !automatic_issuer(&history.records[&head], &issuer, issuer_count, &lineage) {
                return Err(Error::new("fleet account has another automatic issuer"));
            }
            let device = session.peer_writer(actor)?;
            if targets
                .iter()
                .filter(|target| target.machine() == device.machine())
                .count()
                != 1
            {
                return Err(Error::new(
                    "fleet account target has ambiguous machine bindings",
                ));
            }
            let reference = IntentRef {
                account: intent_account,
                decision: head,
            };
            let account = reference.device_account(&device);
            let parents = match history.status(account) {
                State::Empty => vec![],
                State::Ready(previous) | State::Stopped(previous) => {
                    let prior = &history.records[&previous];
                    match &prior.request().action {
                        Action::Prepare(preparation)
                            if preparation.intent == Some(reference)
                                && preparation.device == device =>
                        {
                            return Ok(None);
                        }
                        Action::Stop => {
                            // A Stop is never undone by the same intent revision.
                            let same_intent = history.records.values().any(|record| {
                                record.request().account == account
                                    && matches!(&record.request().action,
                                    Action::Prepare(p) if p.intent == Some(reference))
                            });
                            if same_intent {
                                return Ok(None);
                            }
                        }
                        Action::Prepare(preparation)
                            if preparation
                                .intent
                                .is_some_and(|old| old.account == intent_account) => {}
                        _ => {
                            return Err(Error::new(
                                "device account belongs to another authority lane",
                            ));
                        }
                    }
                    vec![previous]
                }
                _ => return Err(Error::new("device preparation history is not unambiguous")),
            };
            Ok(Some(Request {
                account,
                generation: next_generation(&history, &parents)?,
                parents,
                action: Action::Prepare(Box::new(terms.for_device(reference, device))),
            }))
        })
    }

    /// Recover a consumed attempt using independently current root authority.
    /// The caller authenticates the requesting device; this method accepts no
    /// device-supplied configuration or credential commitment.
    pub fn reissue_intent_preparation(
        &self,
        admin: &Admin,
        account: Digest,
        expected_decision: Digest,
        requester: &aterm_secrets::commons::binding::Binding,
        request: [u8; 16],
    ) -> Result<(Record, Receipt)> {
        let observed = self.with_store(false, |session, store, scope| {
            let history = History::read(session, store, scope.trust())?;
            let previous = history
                .records
                .get(&expected_decision)
                .ok_or_else(|| Error::new("recovery decision is unavailable"))?;
            if previous.request().account != account || current(&history, previous)?.is_none() {
                return Err(Error::new("recovery has no current fleet intent"));
            }
            let Action::Prepare(preparation) = &previous.request().action else {
                unreachable!()
            };
            if requester != &preparation.device
                || session.peer_writer(requester.actor())? != *requester
            {
                return Err(Error::new("recovery target binding changed"));
            }
            session.account_binding_unchanged(admin, previous)?;
            let wanted = Request {
                account,
                generation: previous
                    .request()
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| Error::new("account decision generation exhausted"))?,
                parents: vec![expected_decision],
                action: previous.request().action.clone(),
            };
            // Replays of a durably committed request return its same decision.
            if history.retry(session.actor(), request, &wanted)?.is_none()
                && history.status(account) != State::Ready(expected_decision)
            {
                return Err(Error::new("recovery decision is no longer current"));
            }
            Ok((previous.clone(), wanted))
        })?;
        Configuration::capture_until(&self.paths, &observed.0, self.deadline)?;
        self.issue_in(admin, request, observed.1, true)
    }
}
