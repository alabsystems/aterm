// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-local request capabilities derived from current signed vault grants.
//! This is the broker's authentication boundary, not a transport or launcher.
//! Only trusted local control may issue tokens; request clients may submit them.

use crate::{
    Error, Result,
    accounts::billing::{self, Billing},
    resources::{BoundRequest, Observation, RequestBinding, RequestKey},
    table::Session,
};
use astream_wire::sync::{Actor, Digest};
use aterm_secrets::{
    client::{self, Config, EntrySnapshot, EntryVersion, Paths},
    commons::bootstrap::Bootstrap,
};
use aterm_uds::secret::Secret;
use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

pub(crate) mod command;
pub mod google;
pub mod host;
mod policy;
mod refresh;
pub mod transport;
pub use policy::{ENTRY, Grant, Policy};
pub use refresh::Refresh;
const MAX_SESSIONS: usize = 64;

/// Trusted local selection, never a bearer capability. Account selection must
/// identify exactly one local-writer grant for the named adapter.
#[derive(Clone, Debug)]
pub enum Selection {
    Grant([u8; 16]),
    Account { account: Digest, adapter: String },
}

/// A local bearer credential, never a provider key. Keep its delivery private.
/// Dropping it scrubs this copy; revocation requires `Broker::revoke`.
#[derive(Debug)]
pub struct Token(Secret);

impl Token {
    pub fn bytes(&self) -> &[u8] {
        self.0.expose()
    }
}

#[derive(Clone)]
struct Lease {
    serial: u64,
    grant: Grant,
    paths: Paths,
    floor: EntrySnapshot,
    expires: Instant,
}

#[derive(Default)]
struct State {
    serial: u64,
    leases: BTreeMap<Digest, Lease>,
    // Original custody plus the foundation's bounded succession chain.
    floors: BTreeMap<std::path::PathBuf, EntryVersion>,
}

/// One trusted broker process. Leases are intentionally not serializable:
/// restarting it revokes every token while durable request history survives.
pub struct Broker {
    paths: Paths,
    expected: Config,
    key: Secret,
    state: Mutex<State>,
    #[cfg(test)]
    lease_clock: Option<Instant>,
}

/// Admission evidence plus actual adapter entry. `accounting.entered` describes
/// the broker wrapper; `entered` describes the external adapter below it.
pub struct Dispatch {
    pub accounting: billing::Dispatch,
    pub entered: bool,
    pub refusal: Option<String>,
}

impl Broker {
    pub fn at(paths: Paths, expected: Config) -> Result<Self> {
        let key = zeroize::Zeroizing::new(crate::crypto::random::<32>()?);
        Ok(Self {
            paths,
            expected,
            key: Secret::new(key.as_ref()),
            state: Mutex::new(State::default()),
            #[cfg(test)]
            lease_clock: None,
        })
    }

    /// Trusted local control only: a grant id is a selector, not authentication.
    /// This checks the exact current billing binding but initializes/spends no
    /// resource. The caller must securely deliver the returned token to its client.
    pub fn issue_until(
        &self,
        grant: [u8; 16],
        within: Duration,
        deadline: Instant,
    ) -> Result<Token> {
        self.issue_selected_until(&Selection::Grant(grant), within, deadline)
    }

    /// Select and issue atomically with respect to this broker's observations.
    /// Account selection is repeated after billing under the same policy floor;
    /// ambiguity never falls back to a grant that merely happens to validate.
    pub fn issue_selected_until(
        &self,
        selection: &Selection,
        within: Duration,
        deadline: Instant,
    ) -> Result<Token> {
        self.issue_selected_with(selection, within, deadline, || {})
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerSelection",
            action = "Unique",
            project = "aterm_db::accounts::billing::observation::tests::selection_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerSelection",
            action = "Absent",
            project = "aterm_db::accounts::billing::observation::tests::selection_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerSelection",
            action = "Ambiguous",
            project = "aterm_db::accounts::billing::observation::tests::selection_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerSelection",
            action = "ForeignActor",
            project = "aterm_db::accounts::billing::observation::tests::selection_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerSelection",
            action = "WrongAdapter",
            project = "aterm_db::accounts::billing::observation::tests::selection_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerSelection",
            action = "Stale",
            project = "aterm_db::accounts::billing::observation::tests::selection_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerSelection",
            action = "ChangedDuring",
            project = "aterm_db::accounts::billing::observation::tests::selection_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerSelection",
            action = "Retry",
            project = "aterm_db::accounts::billing::observation::tests::selection_transition"
        )
    )]
    fn issue_selected_with(
        &self,
        selection: &Selection,
        within: Duration,
        deadline: Instant,
        after_billing: impl FnOnce(),
    ) -> Result<Token> {
        if within.is_zero() || Instant::now() >= deadline {
            return Err(denied());
        }
        let (paths, grant, floor) = {
            let mut state = self.lock_until(deadline)?;
            self.read(&mut state, selection, None, deadline)?
        };
        if within > grant.lifetime {
            return Err(denied());
        }
        let prepared = Billing::at(
            self.paths.clone(),
            self.expected.clone(),
            grant.binding.account,
        )
        .prepare_until(deadline)?;
        if prepared.binding() != &grant.binding {
            return Err(denied());
        }
        after_billing();
        // Re-read after billing so a removed grant cannot be issued from a stale capture.
        let mut state = self.lock_until(deadline)?;
        let (_, current, floor) =
            self.read(&mut state, selection, Some((&paths, &floor)), deadline)?;
        if current != grant {
            return Err(denied());
        }
        let raw = zeroize::Zeroizing::new(crate::crypto::random::<32>()?);
        let digest = self.token_digest(raw.as_ref())?;
        let token = Token(Secret::new(raw.as_ref()));
        let now = Instant::now();
        if now >= deadline {
            return Err(denied());
        }
        #[cfg(test)]
        let now = self.lease_clock.unwrap_or(now);
        if state.leases.contains_key(&digest) {
            return Err(denied());
        }
        state
            .leases
            .retain(|_, lease| lease.expires > now && lease.grant.id != grant.id);
        if state.leases.len() >= MAX_SESSIONS {
            return Err(denied());
        }
        state.serial = state.serial.checked_add(1).ok_or_else(denied)?;
        let serial = state.serial;
        let expires = now.checked_add(within).ok_or_else(denied)?;
        state.leases.insert(
            digest,
            Lease {
                serial,
                grant,
                paths,
                floor,
                expires,
            },
        );
        Ok(token)
    }

    /// Revoke this process's token. Root grant removal is checked independently
    /// on every request; observing any failed refresh also burns that lease.
    pub fn revoke(&self, token: &[u8]) -> Result<bool> {
        self.revoke_with(token, || {})
    }

    fn revoke_with(&self, token: &[u8], waiting: impl FnOnce()) -> Result<bool> {
        let digest = self.token_digest(token)?;
        let mut state = self.lock_wait(None, true, waiting)?;
        Ok(held_lease(&state, digest).is_some() && state.leases.remove(&digest).is_some())
    }

    /// Trusted local control may revoke a launch without retaining its secret.
    pub fn revoke_grant(&self, grant: [u8; 16]) -> Result<bool> {
        let mut state = self.lock_wait(None, true, || {})?;
        let before = state.leases.len();
        state.leases.retain(|_, lease| lease.grant.id != grant);
        Ok(state.leases.len() != before)
    }

    pub fn dispatch_until(
        &self,
        adapter: &str,
        token: &[u8],
        logical: [u8; 16],
        payload: &[u8],
        deadline: Instant,
        run: impl FnOnce(&BoundRequest, &[u8]) -> Option<Observation>,
    ) -> Result<Dispatch> {
        self.dispatch_with_until(adapter, token, (logical, payload), deadline, || {}, run)
    }

    /// Authenticate before parsing provider-specific input, then retain that
    /// prepared value through durable admission. Preparation must be bounded,
    /// perform no provider effect and honor the supplied deadline. Its failure
    /// creates neither a logical request claim nor a resource charge.
    ///
    /// # Errors
    /// Authentication, preparation, expiry, changed authority or billing errors.
    pub fn dispatch_prepared_until<T>(
        &self,
        adapter: &str,
        token: &[u8],
        request: ([u8; 16], &[u8]),
        deadline: Instant,
        prepare: impl FnOnce(&[u8], Instant) -> Result<T>,
        run: impl FnOnce(T, &BoundRequest, &[u8]) -> Option<Observation>,
    ) -> Result<Dispatch> {
        self.dispatch_preparing_until(adapter, token, request, deadline, prepare, (|| {}, run))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "Fresh",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "WrongToken",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "OtherBroker",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "Revoked",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "Expired",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "PolicyChanged",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "RevokedAfter",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "ExpiredAfter",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "Rotated",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "Restart",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "WrongBilling",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "PolicyAfter",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "IssueRollback",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "AbsentRollback",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerCapability",
            action = "RevocationContended",
            project = "aterm_db::accounts::billing::observation::tests::broker_transition"
        )
    )]
    fn dispatch_with_until(
        &self,
        adapter: &str,
        token: &[u8],
        request: ([u8; 16], &[u8]),
        deadline: Instant,
        before_entry: impl FnOnce(),
        run: impl FnOnce(&BoundRequest, &[u8]) -> Option<Observation>,
    ) -> Result<Dispatch> {
        self.dispatch_preparing_until(
            adapter,
            token,
            request,
            deadline,
            |_, _| Ok(()),
            (before_entry, |(), request, credential| {
                run(request, credential)
            }),
        )
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPreparation",
            action = "Valid",
            project = "aterm_db::accounts::billing::observation::tests::preparation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPreparation",
            action = "Invalid",
            project = "aterm_db::accounts::billing::observation::tests::preparation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPreparation",
            action = "WrongToken",
            project = "aterm_db::accounts::billing::observation::tests::preparation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPreparation",
            action = "WrongAdapter",
            project = "aterm_db::accounts::billing::observation::tests::preparation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPreparation",
            action = "RevokedDuring",
            project = "aterm_db::accounts::billing::observation::tests::preparation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPreparation",
            action = "ExpiredDuring",
            project = "aterm_db::accounts::billing::observation::tests::preparation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPreparation",
            action = "Retry",
            project = "aterm_db::accounts::billing::observation::tests::preparation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBrokerPreparation",
            action = "Panic",
            project = "aterm_db::accounts::billing::observation::tests::preparation_transition"
        )
    )]
    fn dispatch_preparing_until<T>(
        &self,
        adapter: &str,
        token: &[u8],
        request: ([u8; 16], &[u8]),
        deadline: Instant,
        prepare: impl FnOnce(&[u8], Instant) -> Result<T>,
        callbacks: (
            impl FnOnce(),
            impl FnOnce(T, &BoundRequest, &[u8]) -> Option<Observation>,
        ),
    ) -> Result<Dispatch> {
        let (before_entry, run) = callbacks;
        let (logical, payload) = request;
        if logical == [0; 16] || payload.len() > 1024 * 1024 || Instant::now() >= deadline {
            return Err(denied());
        }
        let digest = self.token_digest(token)?;
        let lease = self.refresh(digest, None, deadline)?;
        if adapter != lease.grant.binding.adapter {
            return Err(denied());
        }
        let prepared = prepare(payload, deadline)?;
        // Parsing may have taken time or yielded to trusted control. Recheck
        // exact lease authority before any durable request claim or debit.
        self.refresh(digest, Some(lease.serial), deadline)?;
        let key = RequestKey::new(lease.grant.producer, logical)?;
        let billing = Billing::at(
            self.paths.clone(),
            self.expected.clone(),
            lease.grant.binding.account,
        );
        let mut entered = false;
        let mut refusal = None;
        let accounting = billing.dispatch_pinned_until(
            key,
            &lease.grant.binding,
            payload,
            deadline,
            |request, credential| {
                before_entry();
                if request.binding() != &lease.grant.binding {
                    refusal = Some(denied().to_string());
                    return None;
                }
                match self.refresh(digest, Some(lease.serial), deadline) {
                    Ok(_) => {
                        entered = true;
                        run(prepared, request, credential)
                    }
                    Err(error) => {
                        refusal = Some(error.to_string());
                        None
                    }
                }
            },
        )?;
        Ok(Dispatch {
            accounting,
            entered,
            refusal,
        })
    }

    /// Serialize floor advancement with revocation. No lease or storage lock
    /// survives this function or is held over the caller's adapter callback.
    fn refresh(&self, digest: Digest, serial: Option<u64>, deadline: Instant) -> Result<Lease> {
        let mut state = self.lock_until(deadline)?;
        let lease = held_lease(&state, digest).ok_or_else(denied)?;
        if serial.is_some_and(|serial| serial != lease.serial) {
            return Err(denied());
        }
        let refreshed = (|| {
            if self.lease_now() >= lease.expires {
                return Err(denied());
            }
            let (_, grant, floor) = self.read(
                &mut state,
                &Selection::Grant(lease.grant.id),
                Some((&lease.paths, &lease.floor)),
                deadline,
            )?;
            if grant != lease.grant || self.lease_now() >= lease.expires {
                return Err(denied());
            }
            Ok(Lease { floor, ..lease })
        })();
        match refreshed {
            Ok(lease) => {
                state.leases.insert(digest, lease.clone());
                Ok(lease)
            }
            Err(error) => {
                state.leases.remove(&digest);
                Err(error)
            }
        }
    }

    fn read(
        &self,
        state: &mut State,
        selection: &Selection,
        floor: Option<(&Paths, &EntrySnapshot)>,
        deadline: Instant,
    ) -> Result<(Paths, Grant, EntrySnapshot)> {
        let (paths, policy, actor, snapshot) = self.read_policy(state, floor, deadline)?;
        Ok((paths, policy.select(selection, actor)?.clone(), snapshot))
    }

    fn read_policy(
        &self,
        state: &mut State,
        floor: Option<(&Paths, &EntrySnapshot)>,
        deadline: Instant,
    ) -> Result<(Paths, Policy, Actor, EntrySnapshot)> {
        self.config(deadline)?;
        let paths = crate::application_paths_until(&self.paths, deadline)?;
        let config = client::config(&paths)
            .map_err(crate::table::failure)?
            .ok_or_else(denied)?;
        if config.name != self.expected.name || !client::pin_admitted(&config, self.expected.signer)
        {
            return Err(denied());
        }
        let observed = client::observe_verified_entry(&paths, &config, ENTRY)
            .map_err(crate::table::failure)?
            .ok_or_else(denied)?;
        if let Some(previous) = state.floors.get(&paths.dir) {
            if !observed.version.not_before(previous) {
                return Err(denied());
            }
        } else if state.floors.len() >= 9 {
            return Err(denied());
        }
        state.floors.insert(paths.dir.clone(), observed.version);
        let now = self.lease_now();
        state.leases.retain(|_, lease| {
            lease.expires > now
                && lease.paths == paths
                && observed
                    .entry
                    .as_ref()
                    .is_some_and(|entry| entry.snapshot().continues(&lease.floor))
        });
        let entry = observed.entry.ok_or_else(denied)?;
        if let Some((previous, floor)) = floor
            && (previous != &paths || !entry.snapshot().continues(floor))
        {
            return Err(denied());
        }
        let policy = Policy::parse(entry.value().as_slice())?;
        let bootstrap =
            Bootstrap::required_until(&paths, deadline).map_err(crate::table::failure)?;
        let session = Session::open_until(&bootstrap, deadline)?;
        session.writer()?;
        let actor = session.actor();
        drop(session);
        if client::config(&paths)
            .map_err(crate::table::failure)?
            .as_ref()
            != Some(&config)
            || crate::application_paths_until(&self.paths, deadline)? != paths
        {
            return Err(denied());
        }
        self.config(deadline)?;
        Ok((paths, policy, actor, entry.snapshot().clone()))
    }

    fn config(&self, deadline: Instant) -> Result<()> {
        if Instant::now() >= deadline
            || self.expected.signer.is_none()
            || client::config(&self.paths)
                .map_err(crate::table::failure)?
                .as_ref()
                != Some(&self.expected)
        {
            return Err(denied());
        }
        Ok(())
    }

    fn lock_until(&self, deadline: Instant) -> Result<MutexGuard<'_, State>> {
        self.lock_wait(Some(deadline), false, || {})
    }

    fn lock_wait(
        &self,
        deadline: Option<Instant>,
        revoking: bool,
        waiting: impl FnOnce(),
    ) -> Result<MutexGuard<'_, State>> {
        let mut waiting = Some(waiting);
        loop {
            let now = Instant::now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                return Err(denied());
            }
            match self.state.try_lock() {
                Ok(state) => return Ok(state),
                Err(std::sync::TryLockError::Poisoned(error)) if revoking => {
                    return Ok(error.into_inner());
                }
                Err(std::sync::TryLockError::Poisoned(_)) => return Err(denied()),
                Err(std::sync::TryLockError::WouldBlock) => {
                    if let Some(waiting) = waiting.take() {
                        waiting();
                    }
                    let step = Duration::from_millis(5);
                    std::thread::sleep(deadline.map_or(step, |deadline| {
                        deadline.saturating_duration_since(Instant::now()).min(step)
                    }));
                }
            }
        }
    }

    fn token_digest(&self, bytes: &[u8]) -> Result<Digest> {
        if bytes.len() != 32 {
            return Err(denied());
        }
        Ok(aterm_digest::hmac_sha256_parts(
            self.key.expose(),
            &[b"aterm/broker/token/1\0", bytes],
        ))
    }

    // Capability lifetime and operation waiting have independent clocks in
    // deterministic fixtures. Deadlines and lock waits always use real time.
    fn lease_now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = self.lease_clock {
            return now;
        }
        Instant::now()
    }

    #[cfg(test)]
    pub(crate) fn test_set_lease_time(&mut self, now: Instant) {
        self.lease_clock = Some(now);
    }

    #[cfg(test)]
    pub(crate) fn test_issue_selected_after_billing(
        &self,
        selection: &Selection,
        deadline: Instant,
        after: impl FnOnce(),
    ) -> Result<Token> {
        self.issue_selected_with(selection, Duration::from_secs(60), deadline, after)
    }

    #[cfg(test)]
    pub(crate) fn test_expire(&self, token: &[u8]) {
        self.lock_until(Instant::now() + Duration::from_secs(60))
            .unwrap()
            .leases
            .get_mut(&self.token_digest(token).unwrap())
            .unwrap()
            .expires = self.lease_now();
    }

    #[cfg(test)]
    pub(crate) fn test_revoke_after_contention(&self, token: &[u8]) -> bool {
        std::thread::scope(|scope| {
            let (held, observed) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel();
            scope.spawn(move || {
                let _state = self.state.lock().unwrap();
                held.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(60)).unwrap();
            });
            observed.recv_timeout(Duration::from_secs(60)).unwrap();
            self.revoke_with(token, || release.send(()).unwrap())
                .unwrap()
        })
    }

    #[cfg(test)]
    pub(crate) fn test_dispatch_until(
        &self,
        token: &[u8],
        logical: [u8; 16],
        payload: &[u8],
        deadline: Instant,
        before_entry: impl FnOnce(),
        run: impl FnOnce(&BoundRequest, &[u8]) -> Option<Observation>,
    ) -> Result<Dispatch> {
        self.dispatch_with_until(
            "example-v1",
            token,
            (logical, payload),
            deadline,
            before_entry,
            run,
        )
    }
}

fn held_lease(state: &State, digest: Digest) -> Option<Lease> {
    let mut found = None;
    for (key, lease) in &state.leases {
        if aterm_digest::ct_eq(key, &digest) {
            found = Some(lease.clone());
        }
    }
    found
}

fn denied() -> Error {
    Error::new("broker capability is unavailable, expired or changed")
}
