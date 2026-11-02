// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Secrets-backed table envelopes. A short-lived session pins the control journal
//! while authenticating a batch; it must be dropped before network I/O. Historical
//! authentication is evidence, not current application authorization. Durable
//! storage, catalog cutoffs and resource admission belong above this boundary.

pub mod conflict;
mod signatures;
pub mod store;

use crate::{
    Error, Result,
    crypto::{self, Causal, MAX_BODY, MAX_OPERATION, Reader},
};
use astream_wire::sync::{Actor, Digest, Header, Index, MAX_PARENTS};
use aterm_digest::{Sha256, hkdf_expand_32, hkdf_extract};
use aterm_secrets::{
    SecretBytes,
    commons::{
        bootstrap::Bootstrap,
        journal::{Journal, Trust},
        roster::{Access, Historical, Role},
    },
};
use ring::aead;
use zeroize::Zeroizing;

const DOMAIN: &[u8] = b"aterm/commons/table-operation/1\0";
const SCOPE_LEN: usize = 16 + 32 + 32 + 16 + 8;

pub(crate) fn failure(error: aterm_secrets::Error) -> Error {
    error.into()
}

/// Immutable table/audience identity. Display names do not enter key derivation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scope {
    trust: Trust,
    table: [u8; 16],
    generation: u64,
}
impl Scope {
    /// A known table descriptor, learned inside an authenticated catalog.
    pub fn new(trust: Trust, table: [u8; 16], generation: u64) -> Result<Self> {
        if table == [0; 16] || generation == 0 {
            return Err(Error::new(
                "table identity and audience generation must be nonzero",
            ));
        }
        Ok(Self {
            trust,
            table,
            generation,
        })
    }
    /// A fresh table can be created offline by an admitted writer.
    pub fn fresh(trust: Trust) -> Result<Self> {
        Self::new(trust, crypto::random()?, 1)
    }
    pub const fn trust(&self) -> Trust {
        self.trust
    }
    pub const fn table(&self) -> &[u8; 16] {
        &self.table
    }
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    /// The exact table identifier used in signed revocation cutoff facts.
    pub fn digest(&self) -> Digest {
        let mut bytes = b"aterm/commons/table-id/1\0".to_vec();
        bytes.extend_from_slice(&self.bytes());
        Sha256::digest(&bytes)
    }
    fn bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(SCOPE_LEN);
        bytes.extend_from_slice(&self.trust.network);
        bytes.extend_from_slice(&self.trust.authority);
        bytes.extend_from_slice(&self.trust.genesis);
        bytes.extend_from_slice(&self.table);
        bytes.extend_from_slice(&self.generation.to_le_bytes());
        bytes
    }
    fn key(&self, roster: &Digest, epoch: u64, network_key: &[u8; 32]) -> Zeroizing<[u8; 32]> {
        let prk = Zeroizing::new(hkdf_extract(b"aterm/commons/table-key/1\0", network_key));
        let mut info = self.bytes();
        info.extend_from_slice(roster);
        info.extend_from_slice(&epoch.to_le_bytes());
        Zeroizing::new(hkdf_expand_32(&prk, &info))
    }
}

/// Authenticated historical evidence. This type deliberately grants no writer
/// capability and performs no database projection. Check current table cutoffs
/// and application roles before using its payload; never discard budget liabilities.
pub struct Record {
    scope: Scope,
    roster: Digest,
    epoch: u64,
    header: Header,
    authored_ms: u64,
    payload: SecretBytes,
}
impl Record {
    pub const fn scope(&self) -> Scope {
        self.scope
    }
    pub const fn roster(&self) -> &Digest {
        &self.roster
    }
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn header(&self) -> &Header {
        &self.header
    }
    /// Signed wall-clock claim, never an ordering or permission input.
    pub const fn authored_ms(&self) -> u64 {
        self.authored_ms
    }
    pub fn payload(&self) -> &[u8] {
        self.payload.as_slice()
    }
}

/// A current, secrets-backed membership observation. [`Session::open`] holds the
/// control journal's writer lock for the session, so no control change lands
/// mid-batch; [`Session::observe`] reads without it and grants no table writing.
/// Verified historical handles live only for this session; no raw keys are saved.
pub struct Session<'a> {
    bootstrap: &'a Bootstrap,
    journal: Journal,
    access: Access,
    history: std::cell::RefCell<std::collections::BTreeMap<Digest, Historical>>,
    generation: Option<crate::generation::Observation>,
    generation_deadline: Option<std::time::Instant>,
}

/// Envelope authentication inside one source-pinned local read. The enclosing
/// session rechecks custody and live context before returning its result.
struct ReadBatch<'s, 'b> {
    session: &'s Session<'b>,
    scope: Scope,
    keys: Option<ReadKeys>,
    head: std::cell::OnceCell<ControlHead>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlHead {
    Selected(Digest),
    Unselected,
    Uncertain,
}

struct ReadKey {
    value: Zeroizing<[u8; 32]>,
    reused: bool,
}

type ReadKeys = std::cell::RefCell<std::collections::BTreeMap<Digest, ReadKey>>;

/// Local source guards bound to the exact session they protect. Reusing these
/// guards avoids nesting read locks behind a waiting vault writer.
struct SourceReads<'s, 'b> {
    session: &'s Session<'b>,
    _reads: Vec<aterm_secrets::client::CommonsSourceRead>,
}

impl SourceReads<'_, '_> {
    fn with_read_batch<T>(
        &self,
        scope: Scope,
        read: impl FnOnce(&ReadBatch<'_, '_>) -> Result<T>,
    ) -> Result<T> {
        self.session.read_context(scope)?;
        let batch = ReadBatch {
            session: self.session,
            scope,
            keys: Some(ReadKeys::default()),
            head: std::cell::OnceCell::new(),
        };
        let result = read(&batch)?;
        batch.revalidate()?;
        drop(batch);
        self.session
            .bootstrap
            .revalidate_custody()
            .map_err(failure)?;
        self.session.read_context(scope)?;
        Ok(result)
    }

    /// Store opens already fence their single-record replay before finalization.
    /// Keep its ordinary per-call admission when there is no key reuse to amortize.
    fn with_store_replay<T>(
        &self,
        scope: Scope,
        records: usize,
        read: impl FnOnce(&ReadBatch<'_, '_>) -> Result<T>,
    ) -> Result<T> {
        if records > 1 {
            self.with_read_batch(scope, read)
        } else {
            read(&ReadBatch {
                session: self.session,
                scope,
                keys: None,
                head: std::cell::OnceCell::new(),
            })
        }
    }
}

impl ReadBatch<'_, '_> {
    fn discarded_source(&self, envelope: &Envelope<'_>) -> Result<bool> {
        if self.keys.is_none() {
            return self.session.discarded_source(envelope);
        }
        // The enclosing source-pinned batch checks the complete current view
        // before returning. Reuse only its head, never a decision about a record.
        self.session.classify_discarded_source(
            envelope,
            *self.head.get_or_init(|| self.session.control_head()),
        )
    }

    fn revalidate(&self) -> Result<()> {
        // Restoring a file cannot undo uncertainty this batch already observed.
        if self.head.get() == Some(&ControlHead::Uncertain) {
            return Err(Error::snapshot_changed());
        }
        if let Some(keys) = &self.keys {
            let histories = self.session.history.borrow();
            for (roster, key) in &*keys.borrow() {
                if key.reused {
                    histories[roster]
                        .with_key(&self.session.journal, |_| ())
                        .map_err(failure)?;
                }
            }
        }
        Ok(())
    }

    fn authenticate(&self, wire: &[u8]) -> Result<Record> {
        self.open_envelope(Envelope::parse(self.scope, wire)?)
    }

    fn open_envelope(&self, envelope: Envelope<'_>) -> Result<Record> {
        self.session
            .decode_envelope(self.scope, envelope, false, self.keys.as_ref())?
            .ok_or_else(|| Error::new("table payload authentication failed"))
    }
}

impl<'a> Session<'a> {
    pub(crate) fn has_control_writer(&self) -> bool {
        self.journal.is_writer()
    }

    /// The authenticated custody supplying this session, including a selected successor.
    pub(crate) fn paths(&self) -> &aterm_secrets::client::Paths {
        self.bootstrap.paths()
    }

    pub(crate) const fn is_log_backed(&self) -> bool {
        self.access.is_log_backed()
    }

    pub(crate) fn check_known_actor(&self, actor: &Actor) -> Result<()> {
        if self.access.is_log_backed() {
            if self
                .access
                .members()
                .iter()
                .any(|m| m.binding().actor() == *actor)
            {
                return Ok(());
            }
            if aterm_secrets::client::trusted_commons_log(
                self.bootstrap.paths(),
                self.bootstrap.trust(),
            )
            .map_err(failure)?
            .is_some_and(|(log, _)| log.has_known_actor(actor))
            {
                return Ok(());
            }
            return self
                .journal
                .retained_constraints()
                .map_err(failure)?
                .check_known_actor(actor)
                .map_err(failure);
        }
        self.journal
            .constraints()
            .map_err(failure)?
            .check_known_actor(actor)
            .map_err(failure)
    }

    pub(crate) fn historical_writer_in(&self, roster: &Digest, actor: &Actor) -> bool {
        self.historical_binding(roster, actor).is_some()
    }

    /// The binding `actor` held as a Writer in the roster `roster`, the one a record names,
    /// whether or not it is a member now: what attributes a removed Mac's signed rows to its
    /// machine (docs/DESIGN-alab-lab-model-2026-11-01.md §3.7). The current members alone would
    /// blank them.
    pub(crate) fn historical_binding(
        &self,
        roster: &Digest,
        actor: &Actor,
    ) -> Option<aterm_secrets::commons::binding::Binding> {
        if !self.history.borrow().contains_key(roster) {
            let history = self
                .bootstrap
                .historical(&self.journal, &self.access, *roster)
                .ok()?;
            self.history.borrow_mut().insert(*roster, history);
        }
        self.history.borrow()[roster]
            .members()
            .iter()
            .find(|m| m.binding().actor() == *actor && m.role() == Role::Writer)
            .map(|m| m.binding().clone())
    }

    pub(crate) fn observation(&self) -> Result<[u8; 64]> {
        if self.access.is_log_backed() {
            if !self.access.is_current(&self.journal) {
                return Err(Error::new("table repair needs current Commons membership"));
            }
            let mut observation = [0; 64];
            observation[..32].copy_from_slice(self.access.head());
            observation[32..].copy_from_slice(self.access.basis());
            return Ok(observation);
        }
        let aterm_secrets::commons::journal::State::Ready { head, .. } = self.journal.state()
        else {
            return Err(Error::new("table repair needs complete control history"));
        };
        let mut observation = [0; 64];
        observation[..32].copy_from_slice(&head);
        observation[32..].copy_from_slice(self.access.basis());
        Ok(observation)
    }

    /// Older wire commitment from this same validated access snapshot. Local
    /// batches still fence on [`Self::observation`], including selected bindings.
    pub(crate) fn legacy_observation(&self) -> Result<Option<[u8; 64]>> {
        if !self.access.is_log_backed() {
            return Ok(None);
        }
        self.observation()?;
        let mut observation = [0; 64];
        observation[..32].copy_from_slice(self.access.control_history_head());
        observation[32..].copy_from_slice(self.access.basis());
        Ok(Some(observation))
    }

    /// Every current member's binding, for an application that names who has answered.
    pub(crate) fn members(&self) -> Vec<aterm_secrets::commons::binding::Binding> {
        let roster = aterm_secrets::client::swarm_roster(self.bootstrap.paths())
            .ok()
            .flatten();
        self.access
            .members()
            .iter()
            .filter(|member| {
                !roster
                    .as_ref()
                    .is_some_and(|r| binding_denied_in_roster(r, member.binding()))
            })
            .map(|member| member.binding().clone())
            .collect()
    }

    /// Share this batch's validated control context with a detached endpoint
    /// constructor. A caller must release the session before network I/O.
    pub(crate) fn control_context(&self) -> Result<(&Bootstrap, &Journal)> {
        if !self.access.is_current(&self.journal) {
            return Err(Error::new(
                "Commons membership changed during endpoint setup",
            ));
        }
        Ok((self.bootstrap, &self.journal))
    }

    /// Current peer membership for an already channel-authenticated actor.
    pub(crate) fn peer(&self, actor: Actor) -> Result<aterm_secrets::commons::binding::Binding> {
        if !self.access.is_current(&self.journal) {
            return Err(Error::new("Commons membership changed during table repair"));
        }
        // A roster this device cannot read now could name this peer: the last one it read is
        // used, and while it never read one the peer is refused.
        let roster = crate::control::roster_or_last(self.bootstrap.paths())
            .ok_or_else(|| Error::new("this device has not read its roster yet"))?;
        self.access
            .members()
            .iter()
            .find(|member| {
                member.binding().actor() == actor
                    && !roster
                        .as_ref()
                        .is_some_and(|r| binding_denied_in_roster(r, member.binding()))
            })
            .map(|member| member.binding().clone())
            .ok_or_else(|| Error::new("table repair peer is not a current member"))
    }

    /// A credential contributor or custodian must still be a current Writer, rather
    /// than merely possess historical authority evidence accepted for control repair.
    pub(crate) fn peer_writer(
        &self,
        actor: Actor,
    ) -> Result<aterm_secrets::commons::binding::Binding> {
        let binding = self.peer(actor)?;
        if !self
            .access
            .members()
            .iter()
            .any(|member| member.binding().actor() == actor && member.role() == Role::Writer)
        {
            return Err(Error::new("credential peer is not a current Writer"));
        }
        Ok(binding)
    }

    /// Every binding [`Self::members`] names whose actor [`Self::peer_writer`] accepts, under
    /// one membership check and one roster read rather than one of each per member (each roster
    /// read verifies the vault and its log).
    pub(crate) fn writers(&self) -> Vec<aterm_secrets::commons::binding::Binding> {
        if !self.access.is_current(&self.journal) {
            return Vec::new();
        }
        let members = self.access.members();
        self.members()
            .into_iter()
            .filter(|binding| {
                members.iter().any(|member| {
                    member.binding().actor() == binding.actor() && member.role() == Role::Writer
                })
            })
            .collect()
    }

    /// Current admitted writers only; duplicate stable machine identities are
    /// retained so an intent issuer can refuse an ambiguous target.
    pub(crate) fn account_targets(&self) -> Result<Vec<aterm_secrets::commons::binding::Binding>> {
        self.writer()?;
        self.observed_account_targets()
    }

    /// Current Writer identities as metadata. Observation neither requires nor
    /// grants a local mutation lease; authority is rechecked at every write.
    pub(crate) fn observed_account_targets(
        &self,
    ) -> Result<Vec<aterm_secrets::commons::binding::Binding>> {
        if !self.access.is_current(&self.journal) {
            return Err(Error::new(
                "Commons membership changed during target observation",
            ));
        }
        let roster = aterm_secrets::client::swarm_roster(self.bootstrap.paths())
            .ok()
            .flatten();
        Ok(self
            .access
            .members()
            .iter()
            .filter(|member| {
                member.role() == Role::Writer
                    && !roster
                        .as_ref()
                        .is_some_and(|r| binding_denied_in_roster(r, member.binding()))
            })
            .map(|member| member.binding().clone())
            .collect())
    }

    pub(crate) fn sign_credential_delivery(
        &self,
        exchange: &aterm_secrets::need::transfer::Exchange,
        values: &aterm_secrets::need::transfer::Values,
    ) -> Result<Vec<u8>> {
        self.may_write()?;
        if exchange.holder != self.actor() || exchange.network != self.bootstrap.trust().network {
            return Err(Error::new(
                "credential delivery has another holder or network",
            ));
        }
        self.bootstrap
            .device()
            .with_seed(|seed| exchange.seal(seed, values))
            .map_err(failure)
    }

    /// Probe a bounded window of selected historical rosters, even when a lost
    /// handoff prevents opening an existing table. Requests confer no authority.
    pub(crate) fn history_needs(
        &self,
        after: Option<Digest>,
        limit: usize,
    ) -> Result<(Vec<Digest>, Option<Digest>)> {
        let selected: std::collections::BTreeSet<Digest> =
            if let aterm_secrets::commons::journal::State::Ready { head, .. } = self.journal.state()
            {
                let mut set = std::collections::BTreeSet::new();
                let mut cursor = Some(head);
                for _ in 0..aterm_secrets::commons::journal::MAX_NODES {
                    let Some(id) = cursor else { break };
                    let node = self
                        .journal
                        .node(&id)
                        .ok_or_else(|| Error::new("selected control node is missing"))?;
                    if node.has_payload() {
                        set.insert(id);
                    }
                    cursor = node.selected().or_else(|| node.parents().first().copied());
                }
                if cursor.is_some() {
                    return Err(Error::new("selected history exceeds its bound"));
                }
                set
            } else if self.access.is_log_backed() {
                self.journal
                    .inventory()
                    .copied()
                    .filter(|id| self.journal.retained_payload_node(*id))
                    .collect()
            } else {
                return Err(Error::new("table repair needs complete control history"));
            };
        let window: Vec<_> = selected
            .iter()
            .filter(|id| after.is_none_or(|after| **id > after))
            .chain(
                selected
                    .iter()
                    .filter(|id| after.is_some_and(|after| **id <= after)),
            )
            .take(limit)
            .copied()
            .collect();
        let next = window.last().copied();
        let needed = window
            .into_iter()
            .filter(|id| {
                self.bootstrap
                    .historical(&self.journal, &self.access, *id)
                    .is_err()
            })
            .collect();
        Ok((needed, next))
    }

    pub(crate) fn handoffs(&self, peer: Actor, sources: &[Digest]) -> Result<Vec<Option<Vec<u8>>>> {
        let peer = self.peer(peer)?;
        Ok(sources
            .iter()
            .map(|source| {
                self.bootstrap
                    .history_handoff(&self.journal, &self.access, *source, &peer)
                    .ok()
                    .map(|handoff| handoff.bytes().to_vec())
            })
            .collect())
    }

    pub(crate) fn import_handoff(&self, source: Digest, bytes: &[u8]) -> Result<()> {
        let handoff = aterm_secrets::commons::history::Handoff::decode(bytes).map_err(failure)?;
        if handoff.source() != &source {
            return Err(Error::new("history response names another source"));
        }
        self.bootstrap
            .import_history(&self.journal, &self.access, bytes)
            .map_err(failure)?;
        Ok(())
    }

    pub fn open(bootstrap: &'a Bootstrap) -> Result<Self> {
        let journal =
            Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).map_err(failure)?;
        Self::from_admitted_journal(bootstrap, journal)
    }

    /// Open a fresh, locked authorization batch, allowing another local writer
    /// to finish until `deadline`. Replay and current membership checks run only
    /// after acquiring the lock. Share the deadline with bootstrap admission.
    /// After admission, generation checks receive fresh bounded budgets; the
    /// lock-wait deadline is not an expiration of this session's authority.
    pub fn open_until(bootstrap: &'a Bootstrap, deadline: std::time::Instant) -> Result<Self> {
        let journal =
            Journal::open_existing_until(bootstrap.journal_dir(), bootstrap.trust(), deadline)
                .map_err(failure)?;
        let mut session = Self::from_admitted_journal_until(bootstrap, journal, Some(deadline))?;
        session.generation_deadline = None;
        Ok(session)
    }

    /// Authorize a local batch using the fresh, exclusively locked journal from
    /// [`Bootstrap::cached_with_journal_until`]. This consumes that same lock
    /// without another open or replay, rechecks the exact source and immutable
    /// custody, and derives current membership from the admitted history.
    /// A journal from another custody directory is refused even with equal trust.
    /// Drop this session before network I/O.
    pub fn from_admitted_journal(bootstrap: &'a Bootstrap, journal: Journal) -> Result<Self> {
        Self::from_admitted_journal_until(bootstrap, journal, None)
    }

    pub(crate) fn from_admitted_journal_until(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        if journal.directory() != bootstrap.journal_dir() {
            return Err(Error::new(
                "Commons session journal belongs to another custody directory",
            ));
        }
        if !journal.is_writer() {
            return Err(Error::new(
                "Commons admitted session requires its exclusive writer",
            ));
        }
        bootstrap.revalidate_custody().map_err(failure)?;
        Self::from_journal(bootstrap, journal, deadline)
    }

    /// Observe existing custody through a read-only control view, so another
    /// journal writer is no obstacle. The same authorization checks run on the
    /// acknowledged prefix. Stores opened through this session refuse mutations.
    ///
    /// # Errors
    ///
    /// Returns an error when existing control history, authenticated custody or
    /// current membership cannot be validated, including missing or unready
    /// local membership.
    pub fn observe(bootstrap: &'a Bootstrap) -> Result<Self> {
        let journal =
            Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).map_err(failure)?;
        bootstrap.revalidate_custody().map_err(failure)?;
        Self::from_journal(bootstrap, journal, None)
    }

    pub(crate) fn observe_until(
        bootstrap: &'a Bootstrap,
        deadline: std::time::Instant,
    ) -> Result<Self> {
        let journal =
            Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).map_err(failure)?;
        bootstrap.revalidate_custody().map_err(failure)?;
        Self::from_journal(bootstrap, journal, Some(deadline))
    }

    fn from_journal(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        let generation = if crate::generation::is_candidate(bootstrap.paths()) {
            let capture = if journal.is_writer() {
                crate::generation::Observation::capture
            } else {
                crate::generation::Observation::observe
            };
            Some(capture(
                bootstrap.paths(),
                deadline.unwrap_or_else(crate::admission::deadline),
            )?)
        } else {
            match deadline {
                Some(deadline) => {
                    crate::generation::guard_until(bootstrap.paths(), false, deadline)?
                }
                None => crate::generation::guard(bootstrap.paths(), false)?,
            }
            None
        };
        let access = bootstrap
            .access_in(&journal)
            .map_err(failure)?
            .ok_or_else(|| Error::new("Commons membership is not ready for table access"))?;
        Ok(Self {
            bootstrap,
            journal,
            access,
            history: std::cell::RefCell::default(),
            generation,
            generation_deadline: deadline,
        })
    }
    pub fn actor(&self) -> Actor {
        self.access.local().binding().actor()
    }

    pub(crate) fn issue_account(
        &self,
        admin: &aterm_secrets::client::Admin,
        request: aterm_secrets::commons::accounts::Request,
    ) -> Result<aterm_secrets::commons::accounts::Record> {
        self.writer()?;
        admin
            .prepare_account_decision(self.bootstrap, &self.journal, request)
            .map_err(failure)
    }

    pub(crate) fn issue_account_with_intent(
        &self,
        admin: &aterm_secrets::client::Admin,
        request: aterm_secrets::commons::accounts::Request,
        intent: &aterm_secrets::commons::accounts::Record,
    ) -> Result<aterm_secrets::commons::accounts::Record> {
        self.writer()?;
        admin
            .prepare_account_decision_with_intent(self.bootstrap, &self.journal, request, intent)
            .map_err(failure)
    }

    pub(crate) fn issue_account_with_recovered_issuer(
        &self,
        paths: &aterm_secrets::client::Paths,
        admin: &aterm_secrets::client::Admin,
        request: aterm_secrets::commons::accounts::Request,
        intent: &aterm_secrets::commons::accounts::Record,
        deadline: std::time::Instant,
    ) -> Result<aterm_secrets::commons::accounts::Record> {
        self.writer()?;
        let aterm_secrets::commons::accounts::Action::Intent(terms) = &intent.request().action
        else {
            return Err(Error::new("account issuer succession needs an intent"));
        };
        let lineage = crate::generation::lineage_in(paths, self, deadline)?;
        let predecessor =
            crate::generation::issuer_predecessor(paths, self, &terms.issuer, deadline)?;
        let proof = admin
            .account_issuer_succession(predecessor, self.bootstrap, &self.journal, intent)
            .map_err(failure)?;
        if crate::generation::lineage_in(paths, self, deadline)? != lineage {
            return Err(Error::new("account issuer succession changed"));
        }
        let decision = admin
            .prepare_account_decision_after_succession(
                self.bootstrap,
                &self.journal,
                request,
                intent,
                &proof,
            )
            .map_err(failure)?;
        if crate::generation::lineage_in(paths, self, deadline)? != lineage {
            return Err(Error::new(
                "account issuer succession changed before commit",
            ));
        }
        Ok(decision)
    }

    pub(crate) fn account_binding_unchanged(
        &self,
        admin: &aterm_secrets::client::Admin,
        record: &aterm_secrets::commons::accounts::Record,
    ) -> Result<()> {
        self.writer()?;
        admin
            .account_binding_unchanged(self.bootstrap, &self.journal, record)
            .map_err(failure)
    }

    pub(crate) fn permit_account(
        &self,
        decision: &aterm_secrets::commons::accounts::Record,
    ) -> Result<()> {
        use aterm_secrets::commons::accounts::Action;
        self.writer()?;
        let Action::Prepare(preparation) = &decision.request().action else {
            return Err(Error::new("account is stopped"));
        };
        let control_valid = if self.access.is_log_backed() {
            decision.control() == *self.access.head()
                || decision.control() == *self.access.basis()
                || self.historical_writer_in(&decision.control(), &preparation.device.actor())
                || matches!(
                    self.journal.state(),
                    aterm_secrets::commons::journal::State::Ready { head, .. }
                        if self.journal.selected_ancestor(decision.control(), head)
                )
        } else {
            self.journal
                .selected_ancestor(decision.control(), *self.access.head())
        };
        if decision.trust() != self.journal.trust()
            || preparation.device != *self.access.local().binding()
            || !control_valid
        {
            return Err(Error::new(
                "account preparation has stale control or another device",
            ));
        }
        Ok(())
    }
    pub const fn epoch(&self) -> u64 {
        self.access.generation()
    }
    pub fn scope(&self) -> Result<Scope> {
        self.writer()?;
        Scope::fresh(self.journal.trust())
    }
    fn local_source_reads(&self) -> Result<SourceReads<'_, 'a>> {
        let mut reads = vec![self.bootstrap.local_source_read().map_err(failure)?];
        if let Some(generation) = &self.generation {
            reads.extend(generation.local_source_reads()?);
        }
        Ok(SourceReads {
            session: self,
            _reads: reads,
        })
    }

    fn context(&self, scope: Scope) -> Result<()> {
        if self.generation.is_some() {
            self.generation_guard(false)?;
        }
        if scope.trust != self.journal.trust() || !self.access.is_current(&self.journal) {
            return Err(Error::new(
                "table has another trust context or stale membership",
            ));
        }
        Ok(())
    }
    /// Whether the current membership grants this device table writing: [`Self::writer`]'s role
    /// and currency, without the journal writer lock a read does not hold.
    pub(crate) fn may_write(&self) -> Result<()> {
        if self.access.local().role() != Role::Writer
            || !self.access.is_current(&self.journal)
            || aterm_secrets::client::swarm_roster(self.bootstrap.paths())
                .ok()
                .flatten()
                .is_some_and(|r| binding_denied_in_roster(&r, self.access.local().binding()))
        {
            return Err(Error::new(
                "current Commons membership does not grant table writing",
            ));
        }
        Ok(())
    }
    pub(crate) fn writer(&self) -> Result<()> {
        self.generation_guard(true)?;
        if !self.journal.is_writer()
            || self.access.local().role() != Role::Writer
            || !self.access.is_current(&self.journal)
            || aterm_secrets::client::swarm_roster(self.bootstrap.paths())
                .ok()
                .flatten()
                .is_some_and(|r| binding_denied_in_roster(&r, self.access.local().binding()))
        {
            return Err(Error::new(
                "current Commons membership does not grant table writing",
            ));
        }
        Ok(())
    }

    fn generation_guard(&self, authoring: bool) -> Result<()> {
        let Some(observation) = &self.generation else {
            return crate::generation::guard(self.bootstrap.paths(), authoring);
        };
        observation.check(
            authoring,
            false,
            self.generation_deadline
                .unwrap_or_else(crate::admission::deadline),
        )
    }

    pub(crate) fn mutation_guard(&self) -> Result<()> {
        let Some(observation) = &self.generation else {
            return crate::generation::allow_mutation(self.bootstrap.paths());
        };
        observation.check(
            false,
            true,
            self.generation_deadline
                .unwrap_or_else(crate::admission::deadline),
        )
    }
    /// Internal envelope codec for the store's locked commit path and protocol
    /// fixtures. These signed bytes must never escape before durable storage.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "ReaderWrites",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    fn seal(&self, scope: Scope, causal: Causal<'_>, payload: &[u8]) -> Result<Vec<u8>> {
        let authored_ms = aterm_time::unix_millis()
            .ok_or_else(|| Error::new("system clock cannot represent an author timestamp"))?;
        self.seal_at(scope, causal, payload, authored_ms)
    }
    fn seal_at(
        &self,
        scope: Scope,
        causal: Causal<'_>,
        payload: &[u8],
        authored_ms: u64,
    ) -> Result<Vec<u8>> {
        self.context(scope)?;
        self.writer()?;
        if payload.len() > MAX_BODY
            || causal.parents.len() > MAX_PARENTS
            || causal.previous == Some([0; 32])
        {
            return Err(Error::new("table operation exceeds its bound"));
        }
        let mut wire = b"ACTO1".to_vec();
        wire.extend_from_slice(&scope.bytes());
        wire.extend_from_slice(self.access.basis());
        wire.extend_from_slice(&self.access.generation().to_le_bytes());
        wire.extend_from_slice(&self.actor());
        wire.extend_from_slice(&causal.sequence.to_le_bytes());
        wire.extend_from_slice(&authored_ms.to_le_bytes());
        wire.extend_from_slice(&causal.previous.unwrap_or([0; 32]));
        wire.push(causal.parents.len() as u8);
        for parent in causal.parents {
            wire.extend_from_slice(parent);
        }
        let salt = crypto::random()?;
        wire.extend_from_slice(&salt);
        wire.extend_from_slice(&((payload.len() + 16) as u32).to_le_bytes());
        let mut body = payload.to_vec();
        self.access
            .with_key(&self.journal, |network_key| {
                crypto::cipher(
                    &scope.key(self.access.basis(), self.epoch(), network_key),
                    &salt,
                    &wire,
                )?
                .seal_in_place_append_tag(
                    aead::Nonce::assume_unique_for_key([0; 12]),
                    aead::Aad::from(&wire),
                    &mut body,
                )
                .map_err(|_| Error::new("table encryption failed"))
            })
            .map_err(failure)??;
        wire.extend_from_slice(&body);
        wire.extend_from_slice(
            &self
                .bootstrap
                .device()
                .with_seed(|seed| crypto::signed(seed, DOMAIN, &wire))?,
        );
        check_shape(Header {
            id: Sha256::digest(&wire),
            actor: self.actor(),
            sequence: causal.sequence,
            previous: causal.previous,
            parents: causal.parents.to_vec(),
        })?;
        Ok(wire)
    }
    /// Authenticate a bounded record under its exact historical writer roster.
    /// Missing keys/history, discarded control branches and reader signatures
    /// refuse. The returned evidence still requires current application policy.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "Valid",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "UnknownTable",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "ForgedReader",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "WrongEpoch",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "Tampered",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "Malformed",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "OldEpoch",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    pub fn authenticate(&self, scope: Scope, wire: &[u8]) -> Result<Record> {
        self.open_envelope(scope, self.envelope(scope, wire)?)
    }

    /// Amortize live context checks across a bounded local interpretation. Each
    /// envelope still verifies its historical writer, signature and ciphertext.
    /// No result leaves until custody and context have been checked again. The
    /// callback performs no publication or network I/O while source guards live.
    fn with_read_batch<T>(
        &self,
        scope: Scope,
        read: impl FnOnce(&ReadBatch<'_, '_>) -> Result<T>,
    ) -> Result<T> {
        self.local_source_reads()?.with_read_batch(scope, read)
    }

    fn read_context(&self, scope: Scope) -> Result<()> {
        let current = Journal::read_existing(self.bootstrap.journal_dir(), self.bootstrap.trust())
            .map_err(failure)?;
        // The projection includes retained restrictive facts as well as the
        // selected roster. A changed prefix needs a fresh complete observation.
        if !current.inventory().eq(self.journal.inventory()) || !self.access.is_current(&current) {
            return Err(Error::snapshot_changed());
        }
        self.context(scope)
    }

    fn envelope<'w>(&self, scope: Scope, wire: &'w [u8]) -> Result<Envelope<'w>> {
        self.context(scope)?;
        Envelope::parse(scope, wire)
    }

    fn open_envelope(&self, scope: Scope, envelope: Envelope<'_>) -> Result<Record> {
        self.decode_envelope(scope, envelope, false, None)?
            .ok_or_else(|| Error::new("table payload authentication failed"))
    }

    /// Negative-only bytes have no prior positive admission. Missing selected
    /// history still withholds evaluation; invalid payloads or writer roles do
    /// not make an obsolete certificate a fault of an unrelated writer.
    fn negative_record(&self, scope: Scope, wire: &[u8]) -> Result<Option<Record>> {
        self.decode_envelope(scope, self.envelope(scope, wire)?, true, None)
    }

    fn decode_envelope(
        &self,
        scope: Scope,
        envelope: Envelope<'_>,
        negative: bool,
        keys: Option<&ReadKeys>,
    ) -> Result<Option<Record>> {
        let Envelope {
            roster,
            epoch,
            header,
            authored_ms,
            salt,
            aad,
            body,
        } = envelope;
        let actor = header.actor;
        if !self.history.borrow().contains_key(&roster) {
            let history = self
                .bootstrap
                .historical(&self.journal, &self.access, roster)
                .map_err(failure)?;
            self.history.borrow_mut().insert(roster, history);
        }
        let histories = self.history.borrow();
        let history = &histories[&roster];
        if history.generation() != epoch
            || !history
                .members()
                .iter()
                .any(|member| member.binding().actor() == actor && member.role() == Role::Writer)
        {
            if negative {
                return Ok(None);
            }
            return Err(Error::new("record lacks an exact historical writer grant"));
        }
        let mut payload = Zeroizing::new(body.to_vec());
        let mut open = |key: &[u8; 32]| -> Result<Option<usize>> {
            Ok(crypto::cipher(key, &salt, aad)?
                .open_in_place(
                    aead::Nonce::assume_unique_for_key([0; 12]),
                    aead::Aad::from(aad),
                    &mut payload,
                )
                .ok()
                .map(|plain| plain.len()))
        };
        let len = if let Some(keys) = keys {
            // Only a source-pinned ReadBatch supplies this map. It validates
            // each historical key at acquisition, revalidates reused keys
            // before returning, and erases the derived keys with the batch.
            let mut keys = keys.borrow_mut();
            let key = match keys.entry(roster) {
                std::collections::btree_map::Entry::Occupied(entry) => {
                    let key = entry.into_mut();
                    key.reused = true;
                    key
                }
                std::collections::btree_map::Entry::Vacant(entry) => entry.insert(ReadKey {
                    value: history
                        .with_key(&self.journal, |network_key| {
                            scope.key(&roster, epoch, network_key)
                        })
                        .map_err(failure)?,
                    reused: false,
                }),
            };
            open(&key.value)?
        } else {
            history
                .with_key(&self.journal, |network_key| {
                    open(&scope.key(&roster, epoch, network_key))
                })
                .map_err(failure)??
        };
        let Some(len) = len else { return Ok(None) };
        payload.truncate(len);
        Ok(Some(Record {
            scope,
            roster,
            epoch,
            header,
            authored_ms,
            payload: SecretBytes::new(std::mem::take(&mut *payload)),
        }))
    }

    // Only classifies source metadata. The store must first prove these exact
    // bytes were already witnessed; this never authorizes remote ingestion.
    fn discarded_source(&self, envelope: &Envelope<'_>) -> Result<bool> {
        self.classify_discarded_source(envelope, self.control_head())
    }

    fn control_head(&self) -> ControlHead {
        match self.journal.state() {
            aterm_secrets::commons::journal::State::Ready { head, .. } => {
                ControlHead::Selected(head)
            }
            aterm_secrets::commons::journal::State::Uncertain => ControlHead::Uncertain,
            // A log-backed member can still be authorized when the legacy
            // control journal has a signed fork but selects no data branch.
            _ => ControlHead::Unselected,
        }
    }

    fn classify_discarded_source(
        &self,
        envelope: &Envelope<'_>,
        head: ControlHead,
    ) -> Result<bool> {
        match head {
            ControlHead::Selected(head) => Ok(self.discarded_source_at(envelope, head)),
            ControlHead::Unselected => Ok(false),
            ControlHead::Uncertain => Err(Error::snapshot_changed()),
        }
    }

    fn discarded_source_at(&self, envelope: &Envelope<'_>, head: Digest) -> bool {
        use aterm_secrets::commons::constraints::Fact;
        let Some(node) = self.journal.node(&envelope.roster) else {
            return false;
        };
        node.has_payload()
            && !self.journal.selected_ancestor(envelope.roster, head)
            && node.facts().iter().any(|fact| {
                matches!(fact,
                Fact::Epoch { generation, .. } if *generation == envelope.epoch)
            })
            && node.facts().iter().any(|fact| {
                matches!(fact,
                Fact::Bind { binding } if binding.actor() == envelope.header.actor)
            })
    }

    // A named recovery authenticates a historical branch choice. Current
    // selected-path status is checked separately before applying quarantine;
    // an overruled choice never supplies a current restriction or grant.
    fn discarded_by(&self, envelope: &Envelope<'_>, recovery: Digest) -> bool {
        use aterm_secrets::commons::constraints::Fact;
        let Some(choice) = self.journal.node(&recovery) else {
            return false;
        };
        let Some(selected) = choice.selected() else {
            return false;
        };
        let Some(source) = self.journal.node(&envelope.roster) else {
            return false;
        };
        source.has_payload()
            && !self.journal.selected_ancestor(envelope.roster, selected)
            && choice
                .parents()
                .iter()
                .any(|parent| self.journal.selected_ancestor(envelope.roster, *parent))
            && source.facts().iter().any(|fact| {
                matches!(fact,
                Fact::Epoch { generation, .. } if *generation == envelope.epoch)
            })
            && source.facts().iter().any(|fact| {
                matches!(fact,
                Fact::Bind { binding } if binding.actor() == envelope.header.actor)
            })
    }

    fn discarding_recovery(&self, envelope: &Envelope<'_>) -> Option<Digest> {
        self.journal
            .inventory()
            .copied()
            .find(|id| self.discarded_by(envelope, *id))
    }
}

// A valid signature and bounded shape are not a historical writer grant.
// Keep this structural envelope private to the table and its custody store.
struct Envelope<'w> {
    roster: Digest,
    epoch: u64,
    header: Header,
    authored_ms: u64,
    salt: [u8; 32],
    aad: &'w [u8],
    body: &'w [u8],
}
impl<'w> Envelope<'w> {
    fn parse(scope: Scope, wire: &'w [u8]) -> Result<Self> {
        if wire.len() > MAX_OPERATION {
            return Err(Error::new("table operation exceeds its bound"));
        }
        let mut r = Reader::new(wire);
        if r.take(5)? != b"ACTO1" || r.take(SCOPE_LEN)? != scope.bytes() {
            return Err(Error::new(
                "table operation has another audience or protocol",
            ));
        }
        let roster = r.array()?;
        let epoch = u64::from_le_bytes(r.array()?);
        let actor: Actor = r.array()?;
        let sequence = u64::from_le_bytes(r.array()?);
        let authored_ms = u64::from_le_bytes(r.array()?);
        let value = r.array()?;
        let previous = if value == [0; 32] { None } else { Some(value) };
        let count = r.byte()? as usize;
        if count > MAX_PARENTS {
            return Err(Error::new("too many table causal parents"));
        }
        let mut parents = Vec::with_capacity(count);
        for _ in 0..count {
            parents.push(r.array()?);
        }
        let salt = r.array()?;
        let n = u32::from_le_bytes(r.array()?) as usize;
        if !(16..=MAX_BODY + 16).contains(&n) {
            return Err(Error::new("invalid table payload length"));
        }
        let aad_end = wire.len() - r.remaining();
        let body = r.take(n)?;
        r.take(64)?;
        r.end()?;
        let header = Header {
            id: signatures::verify(&actor[..32].try_into().unwrap(), wire)?,
            actor,
            sequence,
            previous,
            parents,
        };
        check_shape(header.clone())?;
        Ok(Self {
            roster,
            epoch,
            header,
            authored_ms,
            salt,
            aad: &wire[..aad_end],
            body,
        })
    }
}

pub(crate) fn envelope_roster(wire: &[u8]) -> Option<Digest> {
    if wire.get(..5) != Some(b"ACTO1") {
        return None;
    }
    wire.get(5 + SCOPE_LEN..37 + SCOPE_LEN)
        .and_then(|s| s.try_into().ok())
}

fn check_shape(header: Header) -> Result<()> {
    Index::new(1)
        .insert(header)
        .map(|_| ())
        .map_err(|_| Error::new("malformed table causal header"))
}

pub(crate) fn binding_denied_in_roster(
    roster: &aterm_secrets::swarm_roster::SwarmRoster,
    binding: &aterm_secrets::commons::binding::Binding,
) -> bool {
    if roster.is_node_denied(&aterm_codec::hex::encode_lower(binding.writer()))
        || roster.is_node_denied(&aterm_codec::hex::encode_lower(binding.incarnation()))
        || roster.is_node_denied(&aterm_codec::hex::encode_lower(binding.machine()))
    {
        return true;
    }
    let rec_str = binding.recipient().to_bech32();
    roster
        .member_by_recipient(&rec_str)
        .is_some_and(|member| roster.is_member_denied(member))
}

#[cfg(test)]
pub(crate) mod tests;
