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
use astream_wire::sync::{Actor, Digest, Header, MAX_PARENTS};
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
        let mut hash = Sha256::new();
        hash.update(b"aterm/commons/table-id/1\0");
        hash.update(self.bytes());
        hash.finalize()
    }
    fn bytes(&self) -> [u8; SCOPE_LEN] {
        let mut bytes = [0u8; SCOPE_LEN];
        bytes[..16].copy_from_slice(&self.trust.network);
        bytes[16..48].copy_from_slice(&self.trust.authority);
        bytes[48..80].copy_from_slice(&self.trust.genesis);
        bytes[80..96].copy_from_slice(&self.table);
        bytes[96..SCOPE_LEN].copy_from_slice(&self.generation.to_le_bytes());
        bytes
    }
    fn key(&self, roster: &Digest, epoch: u64, network_key: &[u8; 32]) -> Zeroizing<[u8; 32]> {
        let prk = Zeroizing::new(hkdf_extract(b"aterm/commons/table-key/1\0", network_key));
        let mut info = [0u8; SCOPE_LEN + 32 + 8];
        info[..SCOPE_LEN].copy_from_slice(&self.bytes());
        info[SCOPE_LEN..SCOPE_LEN + 32].copy_from_slice(roster);
        info[SCOPE_LEN + 32..].copy_from_slice(&epoch.to_le_bytes());
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
    pub(super) header: Header,
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

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the table writer's wait (one-mesh M4, §11 B11): while set, a
    /// writer that holds a `Remove` whose rotation has not arrived seals its table write at once,
    /// under the epoch that is due it, as before B11. A test sets it for its
    /// thread; a lab instance turns it on for its whole process with `tables-dont-wait` in
    /// `ATERM_TEST_NEGATIVE_CONTROL` ([`aterm_types::negative_control::on`]).
    pub(crate) static TABLES_DONT_WAIT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// The negative control of member-signed table cutoffs (one-mesh §4.10, §11 B11): while set,
    /// a reader ignores them, so a removed writer's operation under an epoch it still held counts
    /// as before B11. Process-wide as `table-cutoffs-off`.
    pub(crate) static TABLE_CUTOFFS_OFF: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// The negative control of the F21 path (§4.10): while set, a removal cutoff also withholds
    /// every other writer's operation sealed under the epoch the cutoff was signed under that is
    /// not in the cutoff's causal past, the rule that cuts by epoch rather than by writer.
    pub(crate) static TABLE_CUT_BY_EPOCH: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether [`TABLE_CUTOFFS_OFF`] is on, on this thread or for this process.
#[cfg(any(test, debug_assertions))]
fn table_cutoffs_off() -> bool {
    TABLE_CUTOFFS_OFF.with(std::cell::Cell::get)
        || aterm_types::negative_control::on("table-cutoffs-off")
}

/// Whether [`TABLES_DONT_WAIT`] is on: set on this thread, or named for this process
/// (`tables-dont-wait`); never in a release build, which has neither switch.
fn tables_dont_wait() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        TABLES_DONT_WAIT.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("tables-dont-wait")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
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
            if aterm_secrets::client::trusted_commons_log_now(
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

    /// Who removed each of `actors` whose seat this session's membership log shows removed: the
    /// signers of the effective `Remove`s of it, or of the quorum `Remove` that took it out. Only
    /// they may cut it from a table (`store::cutoff`). One log read for every actor asked; a
    /// session with no log removes nobody this way.
    pub(crate) fn removers_of(
        &self,
        actors: impl IntoIterator<Item = Actor>,
    ) -> std::collections::BTreeMap<Actor, std::collections::BTreeSet<Actor>> {
        if !self.access.is_log_backed() {
            return std::collections::BTreeMap::new();
        }
        let Some((log, _)) = aterm_secrets::client::trusted_commons_log_now(
            self.bootstrap.paths(),
            self.bootstrap.trust(),
        )
        .ok()
        .flatten() else {
            return std::collections::BTreeMap::new();
        };
        let membership = log.membership();
        actors
            .into_iter()
            .filter_map(|actor| Some((actor, membership.removal(&actor)?.by.clone())))
            .collect()
    }

    pub(crate) fn historical_writer_in(&self, roster: &Digest, actor: &Actor) -> bool {
        if !self.history.borrow().contains_key(roster) {
            let Ok(history) = self
                .bootstrap
                .historical(&self.journal, &self.access, *roster)
            else {
                return false;
            };
            self.history.borrow_mut().insert(*roster, history);
        }
        self.history.borrow()[roster]
            .members()
            .iter()
            .any(|m| m.binding().actor() == *actor && m.role() == Role::Writer)
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
            return Ok(self.access_observation());
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

    /// [`Self::observation`] without asking again: what [`Self::context`] holds current is this
    /// access's own head and basis, so it is the observation wherever that check passes. It proves
    /// nothing by itself: a fence compares it after a `context` of its own.
    pub(crate) fn access_observation(&self) -> [u8; 64] {
        let mut observation = [0; 64];
        observation[..32].copy_from_slice(self.access.head());
        observation[32..].copy_from_slice(self.access.basis());
        observation
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
    pub(crate) fn members(&self) -> Result<Vec<aterm_secrets::commons::binding::Binding>> {
        let roster = aterm_secrets::client::swarm_roster_until(
            self.bootstrap.paths(),
            crate::admission::deadline(),
        )
        .map_err(failure)?;
        Ok(self
            .access
            .members()
            .iter()
            .filter(|member| {
                !roster
                    .as_ref()
                    .is_some_and(|r| binding_denied_in_roster(r, member.binding()))
            })
            .map(|member| member.binding().clone())
            .collect())
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

    fn peer_member(&self, actor: Actor) -> Result<&aterm_secrets::commons::roster::Member> {
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
            .ok_or_else(|| Error::new("table repair peer is not a current member"))
    }

    /// Current peer membership for an already channel-authenticated actor.
    pub(crate) fn peer(&self, actor: Actor) -> Result<aterm_secrets::commons::binding::Binding> {
        self.peer_member(actor)
            .map(|member| member.binding().clone())
    }

    /// A credential contributor or custodian must still be a current Writer, rather
    /// than merely possess historical authority evidence accepted for control repair.
    pub(crate) fn peer_writer(
        &self,
        actor: Actor,
    ) -> Result<aterm_secrets::commons::binding::Binding> {
        let member = self.peer_member(actor)?;
        if member.role() != Role::Writer {
            return Err(Error::new("credential peer is not a current Writer"));
        }
        Ok(member.binding().clone())
    }

    /// Every current member's actor and every current Writer's binding under one membership
    /// check and one roster read.
    pub(crate) fn members_and_writers(
        &self,
    ) -> Result<(
        std::collections::BTreeSet<Actor>,
        std::collections::BTreeMap<Actor, aterm_secrets::commons::binding::Binding>,
    )> {
        if !self.access.is_current(&self.journal) {
            return Err(Error::new(
                "Commons membership changed during writer observation",
            ));
        }
        let roster = aterm_secrets::client::swarm_roster_until(
            self.bootstrap.paths(),
            crate::admission::deadline(),
        )
        .map_err(failure)?;
        let mut members = std::collections::BTreeSet::new();
        let mut writers = std::collections::BTreeMap::new();
        for member in self.access.members() {
            let binding = member.binding();
            if roster
                .as_ref()
                .is_some_and(|r| binding_denied_in_roster(r, binding))
            {
                continue;
            }
            let actor = binding.actor();
            members.insert(actor);
            if member.role() == Role::Writer {
                writers.insert(actor, binding.clone());
            }
        }
        Ok((members, writers))
    }

    /// Every binding [`Self::members`] names whose actor [`Self::peer_writer`] accepts, under
    /// one membership check and one roster read rather than one of each per member (each roster
    /// read verifies the vault and its log).
    pub(crate) fn writers(&self) -> Result<Vec<aterm_secrets::commons::binding::Binding>> {
        if !self.access.is_current(&self.journal) {
            return Err(Error::new(
                "Commons membership changed during writer observation",
            ));
        }
        let roster = aterm_secrets::client::swarm_roster_until(
            self.bootstrap.paths(),
            crate::admission::deadline(),
        )
        .map_err(failure)?;
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
        let roster = aterm_secrets::client::swarm_roster_until(
            self.bootstrap.paths(),
            crate::admission::deadline(),
        )
        .map_err(failure)?;
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

    /// Probe a bounded window of selected historical rosters and, where the
    /// roster is log-backed, the log's Commons epochs, even when a lost handoff
    /// prevents opening an existing table. Requests confer no authority.
    pub(crate) fn history_needs(
        &self,
        after: Option<Digest>,
        limit: usize,
    ) -> Result<(Vec<Digest>, Option<Digest>)> {
        let mut selected: std::collections::BTreeSet<Digest> =
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
        // Log-era rows are sealed under the log's Commons epochs, and under the fold under keys
        // derived from its `[everyone]` epochs (one-mesh M6): a member admitted after one holds
        // no `Wrap` of it and asks a peer for its table key.
        if self.access.is_log_backed()
            && let Some((log, _)) = aterm_secrets::client::trusted_commons_log_now(
                self.bootstrap.paths(),
                self.bootstrap.trust(),
            )
            .map_err(failure)?
        {
            selected.extend(log.commons_epoch_ids());
            selected.extend(log.fold_epoch_ids());
        }
        use std::ops::Bound::{Excluded, Unbounded};
        let tail = match after {
            Some(after) => selected.range((Excluded(after), Unbounded)),
            None => selected.range::<Digest, _>(..),
        };
        let head = after.into_iter().flat_map(|after| selected.range(..=after));
        let ids: Vec<Digest> = tail.chain(head).take(limit).copied().collect();
        let next = ids.last().copied();
        let unknown: Vec<Digest> = ids
            .into_iter()
            .filter(|id| !self.history.borrow().contains_key(id))
            .collect();
        // One batch: a log-backed session leases the log's history once, for the ids the log
        // answers and the handoffs it opens; a fallback to the control journal still checks the
        // membership for each id.
        let answers = self
            .bootstrap
            .historicals(&self.journal, &self.access, &unknown);
        let mut needed = Vec::new();
        for (id, answer) in unknown.into_iter().zip(answers) {
            match answer {
                Ok(history) => {
                    self.history.borrow_mut().insert(id, history);
                }
                Err(_) => needed.push(id),
            }
        }
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
        Self::from_observed_journal(bootstrap, journal, None)
    }

    pub(crate) fn observe_until(
        bootstrap: &'a Bootstrap,
        deadline: std::time::Instant,
    ) -> Result<Self> {
        Self::observe_within(bootstrap, deadline, &|| deadline)
    }

    /// [`Self::observe_until`] under [`crate::generation::Observation::observe_within`]'s two
    /// bounds. `read_by` also stays this session's bound on its later generation checks.
    pub(crate) fn observe_within(
        bootstrap: &'a Bootstrap,
        read_by: std::time::Instant,
        wait: &dyn Fn() -> std::time::Instant,
    ) -> Result<Self> {
        let journal =
            Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).map_err(failure)?;
        bootstrap.revalidate_custody().map_err(failure)?;
        let generation = if crate::generation::is_candidate(bootstrap.paths()) {
            Some(crate::generation::Observation::observe_within(
                bootstrap.paths(),
                read_by,
                wait,
            )?)
        } else {
            crate::generation::guard_until(bootstrap.paths(), false, read_by)?;
            None
        };
        Self::with_generation(bootstrap, journal, generation, Some(read_by))
    }

    /// Keep the read-only control prefix returned with an established bootstrap.
    /// Equal trust does not make another directory this installation's custody,
    /// and a writer cannot become a read-only session through this entry point.
    pub(crate) fn from_observed_journal(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        if journal.directory() != bootstrap.journal_dir() {
            return Err(Error::new(
                "Commons session journal belongs to another custody directory",
            ));
        }
        if journal.is_writer() {
            return Err(Error::new(
                "Commons observed session requires a read-only journal",
            ));
        }
        bootstrap.revalidate_custody().map_err(failure)?;
        Self::from_journal(bootstrap, journal, deadline)
    }

    /// Observe the generation `selection` selected ([`crate::generation::observed_selection_within`]),
    /// whose established custody is `bootstrap` with its read-only control view `journal`, as
    /// [`Self::from_observed_journal`] takes them. A successor home keeps `selection`'s
    /// observation of its predecessors rather than observing each of them again: the session's
    /// generation checks, which every store open and read batch runs before it reads, reobserve
    /// policy, custody and the current control and log view under `read_by`, as a fresh
    /// observation's would.
    ///
    /// # Errors
    /// As [`Self::from_observed_journal`], and `selection` observed another home.
    pub(crate) fn observe_selected(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        selection: crate::generation::Observation,
        read_by: std::time::Instant,
    ) -> Result<Self> {
        if journal.directory() != bootstrap.journal_dir() {
            return Err(Error::new(
                "Commons session journal belongs to another custody directory",
            ));
        }
        if journal.is_writer() {
            return Err(Error::new(
                "Commons observed session requires a read-only journal",
            ));
        }
        if selection.paths() != bootstrap.paths() {
            return Err(Error::new(
                "Commons generation observation belongs to another home",
            ));
        }
        bootstrap.revalidate_custody().map_err(failure)?;
        let generation = if crate::generation::is_candidate(bootstrap.paths()) {
            Some(selection)
        } else {
            crate::generation::guard_until(bootstrap.paths(), false, read_by)?;
            None
        };
        Self::with_generation(bootstrap, journal, generation, Some(read_by))
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
        Self::with_generation(bootstrap, journal, generation, deadline)
    }

    fn with_generation(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        generation: Option<crate::generation::Observation>,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        let access = bootstrap
            .access_in(&journal)
            .map_err(failure)?
            .ok_or_else(|| {
                Error::repair(
                    "Commons membership is not ready for table access",
                    aterm_types::network_state::RepairCause::NotReady,
                )
            })?;
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
    /// The signed audience observation that identifies a Secret cell's key epoch.
    pub(crate) fn secret_basis(&self) -> Digest {
        *self.access.basis()
    }

    /// Derive a table key under a current or retained audience observation. The
    /// same historical handoffs used for table operations keep inner seals readable.
    pub(crate) fn secret_key(&self, scope: Scope, basis: &Digest) -> Result<Zeroizing<[u8; 32]>> {
        if basis == self.access.basis() {
            return self
                .access
                .with_key(&self.journal, |key| scope.key(basis, self.epoch(), key))
                .map_err(failure);
        }
        if !self.history.borrow().contains_key(basis) {
            let history = self
                .bootstrap
                .historical(&self.journal, &self.access, *basis)
                .map_err(failure)?;
            self.history.borrow_mut().insert(*basis, history);
        }
        let histories = self.history.borrow();
        let history = &histories[basis];
        history
            .with_key(&self.journal, |key| {
                scope.key(basis, history.generation(), key)
            })
            .map_err(failure)
    }
    /// The lab this session's membership belongs to.
    pub(crate) fn trust(&self) -> Trust {
        self.journal.trust()
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
        // A store open asks this outside a read batch's source reads, so a vault writer can hold
        // the state lock: that is busy, which a read observes again, not stale membership.
        if scope.trust != self.journal.trust()
            || !self
                .access
                .is_current_observed(&self.journal)
                .map_err(failure)?
        {
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
            || aterm_secrets::client::swarm_roster_until(
                self.bootstrap.paths(),
                crate::admission::deadline(),
            )
            .map_err(failure)?
            .is_some_and(|r| binding_denied_in_roster(&r, self.access.local().binding()))
        {
            return Err(Error::new(
                "current Commons membership does not grant table writing",
            ));
        }
        Ok(())
    }
    /// Whether a write now waits for a removal's rotation (one-mesh M4): the epoch this session
    /// would seal under is due one (P1's gate, `Log::head_due`: its causal past lacks a revoking
    /// row's entry or the `Remove`s that row's gate needs, or it still wraps a Mac the log
    /// removed). The store keeps such a write pending
    /// (`table::store::pending`) and seals it under the new epoch once that arrives.
    pub(crate) fn waits_for_rotation(&self) -> bool {
        self.access.waits_for_rotation(!tables_dont_wait())
    }
    /// The key this Mac seals its pending writes of `scope` under: derived from its table
    /// writer's device seed, so a pending write opens only on the Mac and in the table that kept
    /// it.
    pub(crate) fn pending_key(&self, scope: &Scope) -> Result<Zeroizing<[u8; 32]>> {
        self.bootstrap.device().with_seed(|seed| {
            let prk = Zeroizing::new(aterm_digest::hkdf_extract(
                b"aterm/commons/pending-table-write/1\0",
                seed,
            ));
            Ok(Zeroizing::new(aterm_digest::hkdf_expand_32(
                &prk,
                &scope.bytes(),
            )))
        })
    }
    pub(crate) fn writer(&self) -> Result<()> {
        self.generation_guard(true)?;
        if !self.journal.is_writer()
            || self.access.local().role() != Role::Writer
            || !self.access.is_current(&self.journal)
            || aterm_secrets::client::swarm_roster_until(
                self.bootstrap.paths(),
                crate::admission::deadline(),
            )
            .map_err(failure)?
            .is_some_and(|r| binding_denied_in_roster(&r, self.access.local().binding()))
        {
            return Err(Error::new(
                "current Commons membership does not grant table writing",
            ));
        }
        Ok(())
    }

    /// Require the selected completed application generation for one local read.
    /// This grants no writer role and does not change historical read semantics.
    pub(crate) fn require_current_application_generation(
        &self,
        deadline: std::time::Instant,
    ) -> Result<()> {
        let deadline = self
            .generation_deadline
            .map_or(deadline, |bound| bound.min(deadline));
        match &self.generation {
            Some(observation) => observation.check(true, false, deadline),
            None => crate::generation::guard_until(self.paths(), true, deadline),
        }
    }

    fn generation_guard(&self, authoring: bool) -> Result<()> {
        let Some(observation) = &self.generation else {
            return crate::generation::guard_until(
                self.bootstrap.paths(),
                authoring,
                self.generation_deadline
                    .unwrap_or_else(crate::admission::deadline),
            );
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
            return match self.generation_deadline {
                Some(deadline) => {
                    crate::generation::allow_mutation_until(self.bootstrap.paths(), deadline)
                }
                None => crate::generation::allow_mutation(self.bootstrap.paths()),
            };
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
        if self.waits_for_rotation() {
            return Err(Error::write_pending());
        }
        if payload.len() > MAX_BODY
            || causal.parents.len() > MAX_PARENTS
            || causal.previous == Some([0; 32])
        {
            return Err(Error::new("table operation exceeds its bound"));
        }
        let mut wire = Vec::with_capacity(
            5 + SCOPE_LEN + 181 + causal.parents.len() * 32 + payload.len() + 16 + 64,
        );
        wire.extend_from_slice(b"ACTO1");
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
        let aad_end = wire.len();
        wire.extend_from_slice(payload);
        let (aad, in_out) = wire.split_at_mut(aad_end);
        let tag = self
            .access
            .with_key(&self.journal, |network_key| {
                crypto::cipher(
                    &scope.key(self.access.basis(), self.epoch(), network_key),
                    &salt,
                    aad,
                )?
                .seal_in_place_separate_tag(
                    aead::Nonce::assume_unique_for_key([0; 12]),
                    aead::Aad::from(&*aad),
                    in_out,
                )
                .map_err(|_| Error::new("table encryption failed"))
            })
            .map_err(failure)??;
        wire.extend_from_slice(tag.as_ref());
        wire.extend_from_slice(
            &self
                .bootstrap
                .device()
                .with_seed(|seed| crypto::signed(seed, DOMAIN, &wire))?,
        );
        check_causal_links(causal.sequence, causal.previous, causal.parents)?;
        let id = Sha256::digest(&wire);
        if causal.parents.contains(&id) {
            return Err(Error::new("malformed table causal header"));
        }
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
        // A generation's check can wait, so what follows it is asked afresh, and a journal's own
        // head is its handle's to answer. Under a log with neither, currency asks the log and holds
        // a journal only to its trust: the answer just given, for a journal of that trust.
        if self.generation.is_some() || !self.access.is_log_backed() {
            return self.context(scope);
        }
        if scope.trust != self.journal.trust() || self.journal.trust() != current.trust() {
            return Err(Error::new(
                "table has another trust context or stale membership",
            ));
        }
        Ok(())
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
            // A roster this Mac has not seen, or holds no access to, waits for its view to
            // grow; local contention stays busy.
            let history = self
                .bootstrap
                .historical(&self.journal, &self.access, roster)
                .map_err(|error| {
                    let error = failure(error);
                    if error.is_busy() {
                        error
                    } else {
                        Error::awaiting_membership(&error.0)
                    }
                })?;
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
            return Err(Error::awaiting_membership(
                "record lacks an exact historical writer grant",
            ));
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
        check_causal_links(sequence, previous, &parents)?;
        let id = signatures::verify(&actor[..32].try_into().unwrap(), wire)?;
        if parents.contains(&id) {
            return Err(Error::new("malformed table causal header"));
        }
        let header = Header {
            id,
            actor,
            sequence,
            previous,
            parents,
        };
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

fn check_causal_links(sequence: u64, previous: Option<Digest>, parents: &[Digest]) -> Result<()> {
    if sequence > 0
        && (sequence == 1) == previous.is_none()
        && parents.len() <= MAX_PARENTS
        && parents.windows(2).all(|p| p[0] < p[1])
        && previous.is_none_or(|p| parents.binary_search(&p).is_ok())
    {
        Ok(())
    } else {
        Err(Error::new("malformed table causal header"))
    }
}

fn node_bytes_denied(roster: &aterm_secrets::swarm_roster::SwarmRoster, bytes: &[u8]) -> bool {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0u8; 64];
    for (i, &b) in bytes.iter().enumerate() {
        buf[i * 2] = HEX[(b >> 4) as usize];
        buf[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
    let hex_str = std::str::from_utf8(&buf[..bytes.len() * 2]).unwrap_or("");
    roster.is_node_denied(hex_str)
}

pub(crate) fn binding_denied_in_roster(
    roster: &aterm_secrets::swarm_roster::SwarmRoster,
    binding: &aterm_secrets::commons::binding::Binding,
) -> bool {
    if roster.deny_nodes.is_empty() && roster.deny_labels.is_empty() {
        return false;
    }
    if !roster.deny_nodes.is_empty()
        && (node_bytes_denied(roster, binding.writer())
            || node_bytes_denied(roster, binding.incarnation())
            || node_bytes_denied(roster, binding.machine()))
    {
        return true;
    }
    if roster.members.is_empty() {
        return false;
    }
    let rec_str = binding.recipient().to_bech32();
    roster
        .member_by_recipient(&rec_str)
        .is_some_and(|member| roster.is_member_denied(member))
}

#[cfg(all(test, unix))]
mod cutoff_tests;
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod fixture;
#[cfg(test)]
pub(crate) mod tests;
