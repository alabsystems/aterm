// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Device requests for a new preparation after an uncertain login attempt.
//! These signed application records carry no authority or credential commitment.
//! The issuer must independently validate the current intent and exact device.

use crate::{
    Error, Result,
    catalog::{Directory, ring::RECOVERIES},
    database::{Change, Command},
    documents::log::Log,
    table::{Session, store::Store},
};
use astream_wire::sync::{Actor, Digest};
use aterm_digest::Sha256;
use aterm_json::{Value, json};
use aterm_secrets::{
    client::Paths,
    commons::{binding::Binding, bootstrap::Bootstrap},
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

const COLLECTION: &str = "request";
const MAX_PAYLOAD: usize = 2048;
const MAX_RECORDS: usize = 7680;
const MAX_WRITER_RECORDS: usize = 960;
const DOMAIN: &[u8] = b"aterm/accounts/recovery-request/1\0";

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct DecisionKey {
    writer: Actor,
    account: Digest,
    decision: Digest,
}

/// Exact consumed attempt named by its own authenticated writer. Construction
/// is private so callers cannot substitute a claimed requesting device.
#[derive(Clone, Debug)]
pub struct Request {
    account: Digest,
    decision: Digest,
    attempt: [u8; 16],
    requester: Binding,
}

impl Request {
    pub fn account(&self) -> Digest {
        self.account
    }
    pub fn decision(&self) -> Digest {
        self.decision
    }
    pub fn attempt(&self) -> [u8; 16] {
        self.attempt
    }
    pub fn requester(&self) -> &Binding {
        &self.requester
    }

    /// Stable across day-table retransmission and issuer restarts. A changed
    /// local nonce cannot reuse the same root decision's single-use authority.
    pub fn issue_nonce(&self) -> [u8; 16] {
        let mut sha = Sha256::new();
        sha.update(DOMAIN);
        sha.update(self.requester.actor());
        sha.update(self.account);
        sha.update(self.decision);
        sha.update(self.attempt);
        sha.finalize()[..16].try_into().expect("fixed digest width")
    }
}

/// Short-lived requests authenticated by their actual writer. The latest
/// attempt per writer, account and decision survives delayed requests for older
/// decisions; only the issuer's current root view decides which can advance.
pub struct Recovery {
    paths: Paths,
}

impl Recovery {
    pub fn at(paths: Paths) -> Self {
        Self { paths }
    }

    /// Advertise the retained attempt. Repeating it in the same day appends
    /// nothing. A later day's copy permits recovery after an offline interval.
    pub fn request(&self, account: Digest, decision: Digest, attempt: [u8; 16]) -> Result<()> {
        self.request_at(account, decision, attempt, now()?)
    }

    fn request_at(
        &self,
        account: Digest,
        decision: Digest,
        attempt: [u8; 16],
        now: u64,
    ) -> Result<()> {
        self.request_with_caps_at(
            account,
            decision,
            attempt,
            now,
            MAX_RECORDS,
            MAX_WRITER_RECORDS,
        )
    }

    fn request_with_caps_at(
        &self,
        account: Digest,
        decision: Digest,
        attempt: [u8; 16],
        now: u64,
        max_records: usize,
        max_writer_records: usize,
    ) -> Result<()> {
        if account == [0; 32] || decision == [0; 32] || attempt == [0; 16] {
            return Err(Error::new(
                "recovery needs exact nonzero attempt identifiers",
            ));
        }
        let paths = crate::generation::selected(&self.paths)?;
        let deadline = Instant::now() + PASS;
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, deadline)
            .map_err(crate::table::failure)?;
        let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
        session.writer()?;
        session.peer_writer(session.actor())?;
        let directory = Directory::new(&paths, bootstrap.trust());
        let state = directory.retire(&session, &RECOVERIES, now)?;
        if RECOVERIES.epoch_of(now) < state.floor {
            return Err(Error::new(
                "recovery publication waits for the retained day floor",
            ));
        }
        let actor_key = Sha256::digest(session.actor());
        for period in state.current(&RECOVERIES, now)..=*state.window(&RECOVERIES, now).end() {
            require_time(deadline)?;
            let command = command_key(actor_key, account, decision, attempt, period);
            let scope = RECOVERIES.scope(directory.trust(), period)?;
            let mut room = max_records > 0 && max_writer_records > 0;
            if let Some(store) = Store::observe_if_exists(&session, &directory.path(scope)?, scope)?
            {
                let log = Log::read_with(&session, &store, scope, decode)?;
                log.writable(session.actor())?;
                if log.retry(session.actor(), &command)?.is_some() {
                    return Ok(());
                }
                let (records, writer_records) = log.retained_counts(session.actor());
                let needed = log.append_records_bound();
                room = needed <= max_records.saturating_sub(records)
                    && needed <= max_writer_records.saturating_sub(writer_records);
            }
            if !room {
                continue;
            }
            let mut store = directory.enroll_ring(&session, &RECOVERIES, period, now)?;
            require_time(deadline)?;
            crate::documents::application::append(&session, &mut store, &encode(&command)?)?;
            return Ok(());
        }
        Err(Error::new(
            "recovery publication resumes in the next day window",
        ))
    }

    /// Current signed requests, without creating tables or acquiring a writer.
    /// Their body is still untrusted input, never permission to issue a grant.
    pub fn pending(&self) -> Result<Vec<Request>> {
        self.pending_until(Instant::now() + PASS)
    }

    pub fn pending_until(&self, deadline: Instant) -> Result<Vec<Request>> {
        self.pending_at_until(now()?, deadline)
    }

    #[cfg(test)]
    fn pending_at(&self, now: u64) -> Result<Vec<Request>> {
        self.pending_at_until(now, Instant::now() + PASS)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryDecisionIsolation",
            action = "Current",
            project = "aterm_db::accounts::recovery::tests::decision::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryDecisionIsolation",
            action = "Old",
            project = "aterm_db::accounts::recovery::tests::decision::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryDecisionIsolation",
            action = "Retry",
            project = "aterm_db::accounts::recovery::tests::decision::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryDecisionIsolation",
            action = "NextDay",
            project = "aterm_db::accounts::recovery::tests::decision::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryDecisionIsolation",
            action = "Repair",
            project = "aterm_db::accounts::recovery::tests::decision::transition"
        )
    )]
    fn pending_at_until(&self, now: u64, deadline: Instant) -> Result<Vec<Request>> {
        require_time(deadline)?;
        let paths = crate::generation::selected(&self.paths)?;
        let bootstrap =
            Bootstrap::required_until(&paths, deadline).map_err(crate::table::failure)?;
        let session = Session::observe_until(&bootstrap, deadline)?;
        let directory = Directory::new(&paths, bootstrap.trust());
        let state = directory.ring_view(&session, &RECOVERIES);
        let mut latest: BTreeMap<DecisionKey, ((u64, u64), Request)> = BTreeMap::new();
        let mut writers: BTreeMap<Actor, (Digest, Option<Binding>)> = BTreeMap::new();
        for period in state.window(&RECOVERIES, now) {
            require_time(deadline)?;
            let scope = RECOVERIES.scope(directory.trust(), period)?;
            let Some(store) = Store::observe_if_exists(&session, &directory.path(scope)?, scope)?
            else {
                continue;
            };
            let log = Log::read_with(&session, &store, scope, decode)?;
            require_time(deadline)?;
            for (operation, command) in log.eligible_iter() {
                let actor = operation.header.actor;
                let Ok((key, account, decision, attempt, record_period)) = body(command) else {
                    continue;
                };
                if record_period != period {
                    continue;
                }
                let rank = (period, operation.header.sequence);
                let decision_key = DecisionKey {
                    writer: actor,
                    account,
                    decision,
                };
                if latest
                    .get(&decision_key)
                    .is_some_and(|(old, _)| *old >= rank)
                {
                    continue;
                }
                let (actor_key, requester) = writers
                    .entry(actor)
                    .or_insert_with(|| (Sha256::digest(actor), session.peer_writer(actor).ok()));
                if key != *actor_key {
                    continue;
                }
                let Some(requester) = requester.clone() else {
                    continue;
                };
                latest.insert(
                    decision_key,
                    (
                        rank,
                        Request {
                            account,
                            decision,
                            attempt,
                            requester,
                        },
                    ),
                );
            }
        }
        Ok(latest.into_values().map(|(_, request)| request).collect())
    }
}

/// How long one recovery pass may take before it gives up.
#[cfg(not(test))]
const PASS: Duration = Duration::from_secs(2);
/// A test's pass is its hang detector: a loaded machine can keep a test's journal reads from
/// finishing within the product's bound.
#[cfg(test)]
const PASS: Duration = aterm_spec::wait::MUST_HAPPEN;

fn now() -> Result<u64> {
    aterm_time::unix_millis()
        .map(|ms| ms / 1000)
        .ok_or_else(|| Error::new("system clock cannot place recovery requests"))
}

fn require_time(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(Error::new("account recovery pass expired"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn command(
    actor: Actor,
    account: Digest,
    decision: Digest,
    attempt: [u8; 16],
    period: u64,
) -> Command {
    command_key(Sha256::digest(actor), account, decision, attempt, period)
}

fn command_key(
    key: Digest,
    account: Digest,
    decision: Digest,
    attempt: [u8; 16],
    period: u64,
) -> Command {
    let body = json!({"account":aterm_codec::hex::encode_lower(&account),
        "decision":aterm_codec::hex::encode_lower(&decision),
        "attempt":aterm_codec::hex::encode_lower(&attempt),"period":period});
    let mut sha = Sha256::new();
    sha.update(DOMAIN);
    sha.update(key);
    sha.update(aterm_json::to_vec(&body).expect("fixed scalar recovery body"));
    Command::Write {
        request: sha.finalize()[..16].try_into().expect("fixed digest width"),
        changes: vec![Change {
            collection: COLLECTION.into(),
            key: aterm_codec::hex::encode_lower(&key),
            value: Some(body),
        }],
    }
}

fn digest<const N: usize>(value: &Value) -> Result<[u8; N]> {
    let value = value
        .as_str()
        .ok_or_else(|| Error::new("invalid recovery identifier"))?;
    let digest = crate::database::decode_hex::<N>(value)?;
    if digest == [0; N] {
        return Err(Error::new("invalid recovery identifier"));
    }
    Ok(digest)
}

fn body(command: &Command) -> Result<(Digest, Digest, Digest, [u8; 16], u64)> {
    let Command::Write { changes, .. } = command else {
        return Err(Error::new("recovery request is not a write"));
    };
    let [change] = changes.as_slice() else {
        return Err(Error::new("recovery writes one own row"));
    };
    let value = change
        .value
        .as_ref()
        .filter(|_| change.collection == COLLECTION)
        .ok_or_else(|| Error::new("recovery writes one own row"))?;
    let object = value
        .as_object()
        .ok_or_else(|| Error::new("invalid recovery body"))?;
    let Some(period) = value["period"].as_u64() else {
        return Err(Error::new("invalid recovery body"));
    };
    if object.len() != 4
        || object
            .keys()
            .any(|key| !["account", "decision", "attempt", "period"].contains(&key.as_str()))
    {
        return Err(Error::new("invalid recovery body"));
    }
    let account = digest::<32>(&value["account"])?;
    let decision = digest::<32>(&value["decision"])?;
    let attempt = digest::<16>(&value["attempt"])?;
    let key = crate::database::decode_hex::<32>(&change.key)?;
    Ok((key, account, decision, attempt, period))
}

fn decode(payload: &[u8]) -> Result<Command> {
    if payload.len() > MAX_PAYLOAD {
        return Err(Error::new("recovery request exceeds its bound"));
    }
    let command = Command::decode(payload)?;
    if command.encode()? != payload {
        return Err(Error::new("recovery request is not canonical"));
    }
    let (key, account, decision, attempt, period) = body(&command)?;
    if command_key(key, account, decision, attempt, period) != command {
        return Err(Error::new("recovery retry identity differs from its body"));
    }
    Ok(command)
}

fn encode(command: &Command) -> Result<Vec<u8>> {
    let payload = command.encode()?;
    if decode(&payload)? != *command {
        return Err(Error::new("recovery encoding would change its meaning"));
    }
    Ok(payload)
}

/// Bounded monotonic cooldown for the automatic issuer. A restart waits a fresh
/// cooldown; a clock rollback neither authorizes a retry nor stops one forever.
pub struct Queue {
    due: BTreeMap<(Digest, Digest, Actor), Instant>,
    cooldown: Duration,
    capacity: usize,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            due: BTreeMap::new(),
            cooldown: Duration::from_secs(60),
            capacity: crate::database::MAX_RECORDS * 3,
        }
    }
}

impl Queue {
    /// The normal issuer's bounded upkeep pass. Materializing an explicit
    /// root intent and accepting a device's recovery request use the same
    /// fresh root checks; neither a proposal nor this queue grants authority.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryCurrentDispatch",
            action = "Issue",
            project = "aterm_db::accounts::recovery::tests::dispatch::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryCurrentDispatch",
            action = "Repeat",
            project = "aterm_db::accounts::recovery::tests::dispatch::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryCurrentDispatch",
            action = "Successor",
            project = "aterm_db::accounts::recovery::tests::dispatch::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryCurrentDispatch",
            action = "Quiet",
            project = "aterm_db::accounts::recovery::tests::dispatch::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryCurrentDispatch",
            action = "Flood",
            project = "aterm_db::accounts::recovery::tests::dispatch::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryCurrentDispatch",
            action = "Stop",
            project = "aterm_db::accounts::recovery::tests::dispatch::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryCurrentDispatch",
            action = "StopDevice",
            project = "aterm_db::accounts::recovery::tests::dispatch::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryCurrentDispatch",
            action = "Unknown",
            project = "aterm_db::accounts::recovery::tests::dispatch::transition"
        )
    )]
    pub fn step(
        &mut self,
        paths: &Paths,
        admin: &aterm_secrets::client::Admin,
        deadline: Instant,
    ) -> Result<usize> {
        require_time(deadline)?;
        let activations = super::activations::Activations::at_until(paths.clone(), deadline);
        let mut issued = 0;
        let mut materialization_error = None;
        for _ in 0..8 {
            require_time(deadline)?;
            if aterm_secrets::service::subscriber_turned_off(&paths.home) {
                return Ok(issued);
            }
            match activations.materialize_next_intent(admin) {
                Ok(Some(_)) => issued += 1,
                Ok(None) => break,
                Err(error) => {
                    materialization_error = Some(error);
                    break;
                }
            }
        }
        let requests = Recovery::at(paths.clone()).pending_until(deadline)?;
        let requests = activations.current_recoveries(&requests)?;
        let (recovered, stopped) = self.dispatch(
            requests,
            Instant::now(),
            4,
            || {
                require_time(deadline)?;
                Ok(
                    (!aterm_secrets::service::subscriber_turned_off(&paths.home))
                        .then(Instant::now),
                )
            },
            |request| {
                activations
                    .reissue_intent_preparation(
                        admin,
                        request.account,
                        request.decision,
                        &request.requester,
                        request.issue_nonce(),
                    )
                    .is_ok()
            },
        )?;
        issued += recovered;
        if stopped {
            return Ok(issued);
        }
        match materialization_error {
            Some(error) if issued == 0 => Err(error),
            _ => Ok(issued),
        }
    }

    /// Return at most `limit` mature requests without postponing them. Selection
    /// can be interrupted before dispatch, so only `dispatch` starts a cooldown.
    fn ready(&mut self, requests: Vec<Request>, now: Instant, limit: usize) -> Vec<Request> {
        let keys: std::collections::BTreeSet<_> = requests
            .iter()
            .map(|r| (r.account, r.decision, r.requester.actor()))
            .collect();
        self.due.retain(|key, _| keys.contains(key));
        let mut candidates = Vec::new();
        for request in requests {
            let key = (request.account, request.decision, request.requester.actor());
            if !self.due.contains_key(&key) && self.due.len() < self.capacity {
                self.due.insert(key, now + self.cooldown);
            }
            if let Some(due) = self.due.get(&key).filter(|due| **due <= now) {
                candidates.push((*due, key, request));
            }
        }
        candidates.sort_unstable_by_key(|(due, key, _)| (*due, *key));
        candidates
            .into_iter()
            .take(limit)
            .map(|(_, _, request)| request)
            .collect()
    }

    /// Check whether each attempt can begin, then charge its cooldown before
    /// calling the issuer, including when the issuer refuses it. A stop or error
    /// leaves the unattempted suffix mature. The boolean reports an explicit stop.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryAttemptCooldown",
            action = "Stop",
            project = "aterm_db::accounts::recovery::tests::cooldown::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryAttemptCooldown",
            action = "Deadline",
            project = "aterm_db::accounts::recovery::tests::cooldown::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryAttemptCooldown",
            action = "Partial",
            project = "aterm_db::accounts::recovery::tests::cooldown::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryAttemptCooldown",
            action = "Refused",
            project = "aterm_db::accounts::recovery::tests::cooldown::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryAttemptCooldown",
            action = "Delayed",
            project = "aterm_db::accounts::recovery::tests::cooldown::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryAttemptCooldown",
            action = "Complete",
            project = "aterm_db::accounts::recovery::tests::cooldown::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryAttemptCooldown",
            action = "Resume",
            project = "aterm_db::accounts::recovery::tests::cooldown::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryAttemptCooldown",
            action = "Rescan",
            project = "aterm_db::accounts::recovery::tests::cooldown::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryAttemptCooldown",
            action = "Boundary",
            project = "aterm_db::accounts::recovery::tests::cooldown::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRecoveryAttemptCooldown",
            action = "LateResume",
            project = "aterm_db::accounts::recovery::tests::cooldown::transition"
        )
    )]
    fn dispatch(
        &mut self,
        requests: Vec<Request>,
        now: Instant,
        limit: usize,
        mut before: impl FnMut() -> Result<Option<Instant>>,
        mut issue: impl FnMut(&Request) -> bool,
    ) -> Result<(usize, bool)> {
        let mut issued = 0;
        for request in self.ready(requests, now, limit) {
            let Some(started) = before()? else {
                return Ok((issued, true));
            };
            self.due.insert(
                (request.account, request.decision, request.requester.actor()),
                started + self.cooldown,
            );
            issued += usize::from(issue(&request));
        }
        Ok((issued, false))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod frontier_tests;
