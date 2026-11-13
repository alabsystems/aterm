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
    /// The look at this session's vault its checks judge through ([`Self::look`]), where its
    /// admission kept one or was lent one: a table-repair read batch's, standing on a source read
    /// of the vault held from before the batch's admission until the batch returns, so the batch
    /// is one moment of it, and the source read of the vault its store opens and store read
    /// batches take ([`Self::local_source_reads`]); or a retained subscriber's
    /// ([`Self::retain_selected`]), standing on its retained source read. `None` for every other
    /// session, whose checks each observe the vault on their own call.
    look: Option<SessionLook<'a>>,
}

/// The look a session's checks judge through ([`Session::keep_look`], [`Session::lend_look`]).
#[expect(
    clippy::large_enum_variant,
    reason = "a session holds the look it keeps inline, as it did before a look could be lent; \
              boxing it would allocate at every kept look"
)]
enum SessionLook<'a> {
    /// Its own: a retained subscriber's ([`Session::retain_selected`]), and a table-repair read
    /// batch's admitted outside a round group (`replica::GroupLook`): the mesh's rounds and a
    /// round a test builds directly, and, in a round with a group, a read the group does not take
    /// (a writer generation's candidate home, a read of more than one source or of another vault:
    /// `Replica::lead`) or one inside another admission's batch, whose batch's own source read the
    /// look stands on.
    Kept(aterm_secrets::client::VaultLook<'a>),
    /// A table-repair read batch's admitted through its round's group (`replica::GroupLook`):
    /// the group's look, which an earlier read admission's source read stands under and which
    /// the group kept for it, or the batch's own source read's, which the group keeps after the
    /// batch where the batch succeeded and its glance first saw the control anchor settled.
    Lent(&'a aterm_secrets::client::VaultLook<'a>),
}

/// `bootstrap`'s custody checked again ([`Bootstrap::revalidate_custody`]), the vault observed,
/// and its pinned files' digests taken, through `look` where given
/// ([`Bootstrap::revalidate_custody_in`]).
fn revalidate(
    bootstrap: &Bootstrap,
    look: Option<&aterm_secrets::client::VaultLook<'_>>,
) -> Result<()> {
    match look {
        Some(look) => bootstrap.revalidate_custody_in(look),
        None => {
            #[cfg(test)]
            own_check();
            bootstrap.revalidate_custody()
        }
    }
    .map_err(failure)
}

#[cfg(test)]
thread_local! {
    /// How many of this thread's session checks that a look at the vault ([`Session::observe_in`])
    /// can serve were handed none, and asked on their own calls: an open's custody (`revalidate`)
    /// and access (`Session::with_generation`), the session's currency wherever it is judged
    /// (`Session::current_in`: a peer's, an observation's, an endpoint's context, a writer's;
    /// `Session::current_observed`: a store's context, `Session::context`), and a peer's roster
    /// (`Session::peer_member_in`). A check handed a look, or judged through the look its session
    /// keeps or was lent ([`Session::look`]), counts as served by it, though where the look observes
    /// nothing or looks at another vault `aterm-secrets` answers on its own call uncounted; a
    /// peer's roster read on its own call because its look is at another vault counts. A session's
    /// other checks (a store read batch's opening fence (`Session::read_context`, asked afresh by
    /// design), its closing (`Session::close_read`, whose custody and currency are judged through a
    /// look of its own, made after the read, and whose fence's context (`Session::fenced`) is
    /// asked afresh where the session has a generation or is not log-backed), its records' keys
    /// and history, ...) are not counted.
    pub(crate) static OWN_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many of this thread's store read batch fences (`Session::fenced`'s context, at
    /// `Session::read_context` and `Session::close_read`) asked the session's currency afresh, on
    /// their own calls, as they do whatever look the session keeps.
    pub(crate) static FENCES_AFRESH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many store read batch fences (`Session::read_context`, `Session::close_read`) this
    /// thread asked.
    pub(crate) static FENCES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many store read batch closings this thread judged through a look of their own
    /// (`Session::close_read`).
    pub(crate) static CLOSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many keys this thread's store read batches reused and checked again, afresh, before
    /// returning (`ReadBatch::revalidate`).
    pub(crate) static KEYS_REVALIDATED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One more of [`OWN_CHECKS`].
#[cfg(test)]
fn own_check() {
    OWN_CHECKS.set(OWN_CHECKS.get() + 1);
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
    identity: std::cell::OnceCell<crypto::PayloadKeyIdentity>,
    reused: bool,
}

type ReadKeys = std::cell::RefCell<std::collections::BTreeMap<Digest, ReadKey>>;

/// Local source guards bound to the exact session they protect. Reusing these
/// guards avoids nesting read locks behind a waiting vault writer. Where the session keeps or was
/// lent a look that holds its vault (a table-repair read batch's, [`Session::look`]), that look is
/// this vault's guard, and the borrow of the session keeps it for as long as these guards live.
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
        self.session.close_read(scope)?;
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
            // Asked afresh, on their own calls, as the batch's opening fence is, whatever look its
            // session keeps.
            let histories = self.session.history.borrow();
            for (roster, key) in &*keys.borrow() {
                if key.reused {
                    #[cfg(test)]
                    KEYS_REVALIDATED.set(KEYS_REVALIDATED.get() + 1);
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

    /// Replay needs the authenticated header and cutoff, never an application's plaintext.
    /// Only a non-cutoff fact can be reused, after the same historical grant and current key
    /// read as a full opening. The enclosing batch still revalidates reused keys and custody.
    fn replay_metadata(
        &self,
        wire: &signatures::Identified,
        envelope: Envelope<'_>,
    ) -> Result<(Header, Option<store::cutoff::Cut>)> {
        if envelope.header.id != wire.id() {
            return Err(Error::new(
                "replay envelope differs from identified ciphertext",
            ));
        }
        let cut = self
            .session
            .with_payload_key(
                self.scope,
                &envelope,
                false,
                self.keys.as_ref(),
                |key, identity| {
                    let identity = identity.get_or_init(|| crypto::payload_key_identity(key));
                    if wire.verified_non_cut(identity) {
                        return Ok(Some(None));
                    }
                    let mut payload = Zeroizing::new(envelope.body.to_vec());
                    let opened = crypto::cipher(key, &envelope.salt, envelope.aad)?
                        .open_in_place(
                            aead::Nonce::assume_unique_for_key([0; 12]),
                            aead::Aad::from(envelope.aad),
                            &mut payload,
                        )
                        .ok()
                        .map(|plain| store::cutoff::decode(plain));
                    if matches!(opened, Some(None)) {
                        wire.remember_non_cut(*identity);
                    }
                    Ok(opened)
                },
            )?
            .ok_or_else(|| Error::new("table payload authentication failed"))?;
        Ok((envelope.header, cut))
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
        self.observation_in(self.look())
    }

    /// [`Self::observation`], its currency judged through `look` where given
    /// ([`Self::observe_in`]).
    pub(crate) fn observation_in(
        &self,
        look: Option<&aterm_secrets::client::VaultLook<'_>>,
    ) -> Result<[u8; 64]> {
        if self.access.is_log_backed() {
            if !self.current_in(look) {
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
        let deadline = crate::admission::deadline();
        let roster = match self.look().filter(|look| look.paths() == self.paths()) {
            Some(look) => look.swarm_roster_until(deadline),
            None => aterm_secrets::client::swarm_roster_until(self.paths(), deadline),
        }
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
        if !self.current() {
            return Err(Error::new(
                "Commons membership changed during endpoint setup",
            ));
        }
        Ok((self.bootstrap, &self.journal))
    }

    /// Whether this session's access is current, judged through the look it keeps where it keeps
    /// one ([`Self::current_in`]).
    fn current(&self) -> bool {
        self.current_in(self.look())
    }

    /// Whether this session's access is current ([`Access::is_current`]), judged through `look`
    /// where given ([`Access::is_current_in`]).
    fn current_in(&self, look: Option<&aterm_secrets::client::VaultLook<'_>>) -> bool {
        match look {
            Some(look) => self.access.is_current_in(&self.journal, look),
            None => {
                #[cfg(test)]
                own_check();
                self.access.is_current(&self.journal)
            }
        }
    }

    /// [`Access::is_current_observed`], judged through the look this session keeps where it keeps
    /// one ([`Access::is_current_observed_in`]).
    fn current_observed(&self) -> Result<bool> {
        match self.look() {
            Some(look) => self.access.is_current_observed_in(&self.journal, look),
            None => {
                #[cfg(test)]
                own_check();
                self.access.is_current_observed(&self.journal)
            }
        }
        .map_err(failure)
    }

    /// Keep `look`, the look at this session's vault its admission judged through, for every check
    /// this session makes after it until the session drops: a table-repair read batch's admitted
    /// outside a round group (`Replica::admit`: the mesh's rounds, which take no group; a round a
    /// test builds directly; and, in a round with a group, a writer generation's candidate home, a
    /// read of more than one source or of another vault, which the group does not take
    /// (`Replica::lead`), and one inside another admission's batch), which stands on the batch's
    /// source read of the vault, so
    /// the batch's own checks of its currency, its store contexts and its peers' roster stand for
    /// the moment its admission judged, as its admission's did, and its store opens and store read
    /// batches hold the vault through it ([`Self::local_source_reads`]); or a retained
    /// subscriber's, which stands on its retained source read of the vault
    /// ([`Self::retain_selected`]). A read batch admitted through its round's group is lent the
    /// look instead ([`Self::lend_look`]).
    ///
    /// # Errors
    ///
    /// A session that holds the control journal's writer lock ([`Self::open`]) keeps no look:
    /// each check a write makes observes the vault on its own call, so a write is judged by the
    /// vault as it stands, never by the moment its admission looked.
    pub(crate) fn keep_look(&mut self, look: aterm_secrets::client::VaultLook<'a>) -> Result<()> {
        self.reader()?;
        self.look = Some(SessionLook::Kept(look));
        Ok(())
    }

    /// Judge every check this session makes after it, until the session drops, through `look`,
    /// the look at this session's vault its admission judged through, which outlives the session:
    /// a table-repair read batch's admitted through its round's group (`replica::GroupLook`),
    /// standing on a source read of the vault held from before the admission until after the
    /// batch returns (the group's first admission's, which the group keeps for the read
    /// admissions after it, where it keeps one, until the group ends). The batch's checks stand
    /// for the moment its admission judged, and its store opens and store read batches hold the
    /// vault through it, as through a look it keeps ([`Self::keep_look`]).
    ///
    /// # Errors
    ///
    /// As [`Self::keep_look`]: a session that holds the control journal's writer lock is lent no
    /// look.
    pub(crate) fn lend_look(
        &mut self,
        look: &'a aterm_secrets::client::VaultLook<'a>,
    ) -> Result<()> {
        self.reader()?;
        self.look = Some(SessionLook::Lent(look));
        Ok(())
    }

    /// Refuse a session that holds the control journal's writer lock a look
    /// ([`Self::keep_look`], [`Self::lend_look`]).
    fn reader(&self) -> Result<()> {
        if self.journal.is_writer() {
            return Err(Error::new(
                "a table writer session judges each check on its own call and keeps no look",
            ));
        }
        Ok(())
    }

    /// The look this session's checks judge through, where it keeps one ([`Self::keep_look`]) or
    /// was lent one ([`Self::lend_look`]).
    pub(crate) fn look(&self) -> Option<&aterm_secrets::client::VaultLook<'a>> {
        match self.look.as_ref()? {
            SessionLook::Kept(look) => Some(look),
            SessionLook::Lent(look) => Some(look),
        }
    }

    /// Whether this session's bootstrap still stands ([`Bootstrap::stands`]): where it judges
    /// through a look it keeps or was lent ([`Self::look`]), its vault observation and pinned
    /// files' digests through that look, and its control journal by the control view its
    /// admission read, so as its admission judged them, and the rest (the vault's folders, its
    /// custody locks and its creation claim) as they stand now ([`Bootstrap::stands_in`]);
    /// otherwise all of it on its own calls, as the vault stands now.
    ///
    /// # Errors
    /// As [`Bootstrap::stands`] or [`Bootstrap::stands_in`].
    pub(crate) fn bootstrap_stands(&self) -> Result<()> {
        match self.look() {
            Some(look) => self.bootstrap.stands_in(look, &self.journal),
            None => self.bootstrap.stands(),
        }
        .map_err(failure)
    }

    fn peer_member(&self, actor: Actor) -> Result<&aterm_secrets::commons::roster::Member> {
        self.peer_member_in(actor, self.look())
    }

    /// [`Self::peer_member`], the session's currency judged, and its roster read, through `look`
    /// where given: the roster through a look at this session's vault only.
    fn peer_member_in(
        &self,
        actor: Actor,
        look: Option<&aterm_secrets::client::VaultLook<'_>>,
    ) -> Result<&aterm_secrets::commons::roster::Member> {
        if !self.current_in(look) {
            return Err(Error::new("Commons membership changed during table repair"));
        }
        // A roster this device cannot read now could name this peer: the last one it read is
        // used, and while it never read one the peer is refused. Through the look it stands for
        // the moment of the vault the currency above does.
        let roster = match look.filter(|look| look.paths() == self.bootstrap.paths()) {
            Some(look) => crate::control::roster_or_last_in(look),
            None => {
                #[cfg(test)]
                own_check();
                crate::control::roster_or_last(self.bootstrap.paths())
            }
        }
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
        self.peer_in(actor, self.look())
    }

    /// [`Self::peer`], the session's currency judged, and its roster read, through `look` where
    /// given ([`Self::observe_in`]): the roster through a look at this session's vault only.
    pub(crate) fn peer_in(
        &self,
        actor: Actor,
        look: Option<&aterm_secrets::client::VaultLook<'_>>,
    ) -> Result<aterm_secrets::commons::binding::Binding> {
        self.peer_member_in(actor, look)
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
        if !self.current() {
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
        if !self.current() {
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
        if !self.current() {
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
            && let Some((log, _)) = match self.look() {
                Some(look) if look.paths() == self.bootstrap.paths() => {
                    look.trusted_commons_log(self.bootstrap.trust())
                }
                _ => aterm_secrets::client::trusted_commons_log_now(
                    self.bootstrap.paths(),
                    self.bootstrap.trust(),
                ),
            }
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
        // membership for each id. A table-repair read batch's lease, its membership checks (the
        // lease's and the control journal's) and its reads of the log judge through the look its
        // session keeps or was lent; a handoff it opens checks on its own calls. It asks whether each opens, not for its key: an epoch this Mac
        // opened under the same log is answered without its key unwrapped again, and no asked
        // epoch's key is kept, so this round's asks leave the keys its table reads take where those
        // find them. An epoch this Mac holds only through a handoff opens the handoff at each ask,
        // which reads the handoff's issuance epoch through the kept keys as the historical reads do.
        let held =
            self.bootstrap
                .holds_histories_in(&self.journal, &self.access, &unknown, self.look());
        let needed = unknown
            .into_iter()
            .zip(held)
            .filter_map(|(id, held)| (!held).then_some(id))
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

    /// Try the local writer once, retaining the ordinary replay and authorization checks.
    /// Contention leaves the writer's work alone; the caller can retain an observed frontier
    /// for repair. The lock probe's deadline does not expire an admitted session's authority.
    pub(crate) fn try_open(bootstrap: &'a Bootstrap) -> Result<Self> {
        let journal = Journal::open_existing_until(
            bootstrap.journal_dir(),
            bootstrap.trust(),
            std::time::Instant::now(),
        )
        .map_err(failure)?;
        Self::from_admitted_journal(bootstrap, journal)
    }

    /// Open a fresh, locked authorization batch, allowing another local writer
    /// to finish until `deadline`. Replay and current membership checks run only
    /// after acquiring the lock. Share the deadline with bootstrap admission.
    /// After admission, generation checks receive fresh bounded budgets; the
    /// lock-wait deadline is not an expiration of this session's authority.
    pub fn open_until(bootstrap: &'a Bootstrap, deadline: std::time::Instant) -> Result<Self> {
        Self::open_until_in(bootstrap, deadline, None)
    }

    /// [`Self::open_until`], its custody and access judged through `look` where given
    /// ([`Self::observe_in`]).
    pub(crate) fn open_until_in(
        bootstrap: &'a Bootstrap,
        deadline: std::time::Instant,
        look: Option<&aterm_secrets::client::VaultLook<'_>>,
    ) -> Result<Self> {
        let journal =
            Journal::open_existing_until(bootstrap.journal_dir(), bootstrap.trust(), deadline)
                .map_err(failure)?;
        let mut session = Self::from_admitted_journal_in(bootstrap, journal, Some(deadline), look)?;
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
        Self::from_admitted_journal_in(bootstrap, journal, deadline, None)
    }

    /// [`Self::from_admitted_journal_until`], its custody and access judged through `look` where
    /// given.
    fn from_admitted_journal_in(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        deadline: Option<std::time::Instant>,
        look: Option<&aterm_secrets::client::VaultLook<'_>>,
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
        revalidate(bootstrap, look)?;
        Self::from_journal(bootstrap, journal, deadline, look)
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
        Self::observe_in(bootstrap, None)
    }

    /// [`Self::observe`], its custody and access judged through `look` where given: one look at
    /// the vault ([`aterm_secrets::client::VaultLook`]) for a session's open and the checks a
    /// local batch makes at once after it ([`Self::peer_in`], [`Self::observation_in`]), which
    /// stand for one moment of the vault. A table repair round opened a session about 18 times a
    /// round on each side of a loopback pair and checked it twice at once, each check observing
    /// the state lock and looking at the vault's files again: its checks of currency took 23.7%
    /// of a sustained quiet round over a log of 9,605 records and 27.0% over a short one, its
    /// custody checks 8.2% and 10.9% (2026-11-09).
    pub(crate) fn observe_in(
        bootstrap: &'a Bootstrap,
        look: Option<&aterm_secrets::client::VaultLook<'_>>,
    ) -> Result<Self> {
        let journal =
            Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).map_err(failure)?;
        Self::from_observed_journal_in(bootstrap, journal, None, look)
    }

    /// [`Self::observe_in`] through `look`, a look at this session's vault taken before this
    /// session's own control journal read, which earlier admissions asked through (a
    /// table-repair round group's, `replica::GroupLook`): the control journal read first, as
    /// `observe_in` reads it, then `stands` asked, and the session opened through the look only
    /// where it holds; `None` where it does not, with nothing else judged. The round asks there
    /// whether the look's observation still guards the state lock at the vault's path and the look
    /// still shows the control anchor as it first saw it, so the files the lock guards, and the
    /// anchor the journal read reads, stand for this session as of after that read.
    ///
    /// # Errors
    /// As [`Self::observe_in`].
    pub(crate) fn observe_after_in(
        bootstrap: &'a Bootstrap,
        look: &aterm_secrets::client::VaultLook<'_>,
        stands: impl FnOnce() -> bool,
    ) -> Result<Option<Self>> {
        let journal =
            Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).map_err(failure)?;
        if !stands() {
            return Ok(None);
        }
        Self::from_observed_journal_in(bootstrap, journal, None, Some(look)).map(Some)
    }

    /// [`Self::observe`] by `deadline`, which bounds each wait of a successor home's predecessor
    /// observation and stays this session's bound on its later generation checks.
    pub(crate) fn observe_until(
        bootstrap: &'a Bootstrap,
        deadline: std::time::Instant,
    ) -> Result<Self> {
        let journal =
            Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).map_err(failure)?;
        bootstrap.revalidate_custody().map_err(failure)?;
        let generation = if crate::generation::is_candidate(bootstrap.paths()) {
            Some(crate::generation::Observation::observe(
                bootstrap.paths(),
                deadline,
            )?)
        } else {
            crate::generation::guard_until(bootstrap.paths(), false, deadline)?;
            None
        };
        Self::with_generation(bootstrap, journal, generation, Some(deadline), None)
    }

    /// [`Self::observe_until`] for a home its reader named with
    /// [`crate::generation::observed_generation_until`] rather than authorizing its chain as a
    /// writer does ([`crate::generation::selected`]): a successor home's predecessors are observed
    /// once, after `bootstrap`'s custody is open, and the session keeps that observation
    /// ([`crate::documents::application::observed_session`]), with `deadline` bounding each wait
    /// and the session's later generation checks.
    ///
    /// # Errors
    /// As [`Self::observe_until`].
    pub(crate) fn observe_named_until(
        bootstrap: &'a Bootstrap,
        deadline: std::time::Instant,
    ) -> Result<Self> {
        let journal =
            Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).map_err(failure)?;
        crate::documents::application::observed_session(bootstrap, journal, deadline, &|| deadline)
    }

    /// [`Self::observation`] as this session's home stands now: its control view read again and
    /// its access judged again, the kept generation checked as each use checks it. Nothing is
    /// established or observed again, so no holder of a predecessor's custody is waited for.
    ///
    /// # Errors
    /// The control view, the custody or the generation no longer holds, the access is not
    /// ready, or the control history is incomplete; a vault writer holds a state lock this reads
    /// at this moment (busy); or the session's generation deadline has passed.
    pub(crate) fn observation_afresh(&self) -> Result<[u8; 64]> {
        let journal = Journal::read_existing(self.bootstrap.journal_dir(), self.bootstrap.trust())
            .map_err(failure)?;
        self.bootstrap.revalidate_custody().map_err(failure)?;
        self.generation_guard(false)?;
        Self::with_generation(
            self.bootstrap,
            journal,
            None,
            self.generation_deadline,
            None,
        )?
        .observation()
    }

    /// Keep the read-only control prefix returned with an established bootstrap.
    /// Equal trust does not make another directory this installation's custody,
    /// and a writer cannot become a read-only session through this entry point.
    pub(crate) fn from_observed_journal(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        Self::from_observed_journal_in(bootstrap, journal, deadline, None)
    }

    /// [`Self::from_observed_journal`], its custody and access judged through `look` where given.
    fn from_observed_journal_in(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        deadline: Option<std::time::Instant>,
        look: Option<&aterm_secrets::client::VaultLook<'_>>,
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
        revalidate(bootstrap, look)?;
        Self::from_journal(bootstrap, journal, deadline, look)
    }

    /// Observe the generation `selection` observed ([`crate::generation::observe_generation`],
    /// taken after `bootstrap` was opened, as
    /// [`crate::documents::application::observed_session`] takes it), whose established custody
    /// is `bootstrap` with its read-only control view `journal`, as
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
        Self::with_generation(bootstrap, journal, generation, Some(read_by), None)
    }

    /// A current subscriber backed by retained source/control custody. The
    /// caller keeps control guards separately; this journal grants no writer.
    pub(crate) fn retain_selected(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        selection: crate::generation::Observation,
        deadline: std::time::Instant,
        look: aterm_secrets::client::VaultLook<'a>,
    ) -> Result<Self> {
        if journal.is_writer()
            || journal.directory() != bootstrap.journal_dir()
            || selection.paths() != bootstrap.paths()
            || look.paths() != bootstrap.paths()
        {
            return Err(Error::new(
                "subscriber observation belongs to another custody",
            ));
        }
        bootstrap.revalidate_custody_in(&look).map_err(failure)?;
        selection.check(true, false, deadline)?;
        let mut session = Self::with_generation(
            bootstrap,
            journal,
            Some(selection),
            Some(deadline),
            Some(&look),
        )?;
        session.keep_look(look)?;
        session.subscriber(deadline)?;
        Ok(session)
    }

    pub(crate) fn retained_look(&self) -> Option<&aterm_secrets::client::VaultLook<'a>> {
        self.look()
    }

    /// Current read authorization, including completed generation and local
    /// recipient denial, without conferring table-authoring or ingestion rights.
    /// A retained subscriber stays strict: its home may not be fenced by a succession of its own,
    /// even one whose import never completed, since a retained session can persist a projection.
    pub(crate) fn subscriber(&self, deadline: std::time::Instant) -> Result<()> {
        self.require_completed_application_generation(deadline)?;
        if !self.current_observed()?
            || !self
                .members()?
                .iter()
                .any(|member| member.actor() == self.actor())
        {
            return Err(Error::new(
                "current Commons membership does not grant table reading",
            ));
        }
        Ok(())
    }

    fn from_journal(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        deadline: Option<std::time::Instant>,
        look: Option<&aterm_secrets::client::VaultLook<'_>>,
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
        Self::with_generation(bootstrap, journal, generation, deadline, look)
    }

    /// Open the session on `journal`'s access. A vault writer that holds the state lock at this
    /// moment is busy, which a reader observes again, not membership that is not ready, which
    /// nothing retries.
    fn with_generation(
        bootstrap: &'a Bootstrap,
        journal: Journal,
        generation: Option<crate::generation::Observation>,
        deadline: Option<std::time::Instant>,
        look: Option<&aterm_secrets::client::VaultLook<'_>>,
    ) -> Result<Self> {
        let access = match look {
            Some(look) => bootstrap.access_observed_in_look(&journal, look),
            None => {
                #[cfg(test)]
                own_check();
                bootstrap.access_observed_in(&journal)
            }
        }
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
            look: None,
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
        // A table-repair read batch's session keeps, or was lent, the look its admission judged
        // through, which stands on a source read of this vault held from before the admission
        // until the batch returns (the batch's own, or its round group's first admission's): that
        // read holds the vault for each store open and store read batch too, its custody judged
        // through the look.
        let own = match self.look() {
            Some(look) => self.bootstrap.local_source_read_in(look),
            None => self.bootstrap.local_source_read().map(Some),
        }
        .map_err(failure)?;
        let mut reads: Vec<_> = own.into_iter().collect();
        if let Some(generation) = &self.generation {
            reads.extend(generation.local_source_reads()?);
        }
        Ok(SourceReads {
            session: self,
            _reads: reads,
        })
    }

    fn context(&self, scope: Scope) -> Result<()> {
        self.context_judged(scope, false)
    }

    /// [`Self::context`], its currency asked on its own call where `afresh`, whatever look the
    /// session keeps: a store read batch's opening and closing fences ([`Self::fenced`]).
    fn context_judged(&self, scope: Scope, afresh: bool) -> Result<()> {
        if self.generation.is_some() {
            self.generation_guard(false)?;
        }
        // Asked with no source read of the vault held (a store open outside a read batch), a vault
        // writer can hold the state lock: that is busy, which a read observes again, not stale
        // membership. A table-repair read batch asks it through its look, under its source read,
        // except at a store read batch's fences, which ask afresh.
        let current = if afresh {
            #[cfg(test)]
            FENCES_AFRESH.set(FENCES_AFRESH.get() + 1);
            self.access
                .is_current_observed(&self.journal)
                .map_err(failure)?
        } else {
            self.current_observed()?
        };
        if scope.trust != self.journal.trust() || !current {
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
            || !self.current()
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
            || !self.current()
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

    /// Require, for one local read, that every predecessor of this session's home completed its
    /// succession ([`crate::generation::Observation::check_read`]). A home whose own writer
    /// recovery is pending still reads the records it holds. This grants no writer role and
    /// does not change historical read semantics.
    pub(crate) fn require_current_application_generation(
        &self,
        deadline: std::time::Instant,
    ) -> Result<()> {
        let deadline = self
            .generation_deadline
            .map_or(deadline, |bound| bound.min(deadline));
        match &self.generation {
            Some(observation) => observation.check_read(deadline),
            None => crate::generation::guard_read_until(self.paths(), deadline),
        }
    }

    /// [`Self::require_current_application_generation`] as authoring judges it: every
    /// generation complete and this home not fenced by a succession of its own.
    fn require_completed_application_generation(&self, deadline: std::time::Instant) -> Result<()> {
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

    /// A store read batch's opening fence: the control journal read again, its inventory held to
    /// the session's and the session's currency asked afresh, on their own calls, then
    /// [`Self::fenced`].
    fn read_context(&self, scope: Scope) -> Result<()> {
        #[cfg(test)]
        FENCES.set(FENCES.get() + 1);
        let current = Journal::read_existing(self.bootstrap.journal_dir(), self.bootstrap.trust())
            .map_err(failure)?;
        // The projection includes retained restrictive facts as well as the
        // selected roster. A changed prefix needs a fresh complete observation.
        if !current.inventory().eq(self.journal.inventory()) || !self.access.is_current(&current) {
            return Err(Error::snapshot_changed());
        }
        self.fenced(scope, &current)
    }

    /// A store read batch's closing, once its read and its reused keys' checks are done: the
    /// control journal read again, then the bootstrap's custody and the session's currency judged
    /// through one look at the vault of its own ([`aterm_secrets::client::VaultLook::new`]), with
    /// the journal's inventory held to the session's, then [`Self::fenced`]. The look is made
    /// after the batch's read and the journal read, so its glance's first look at each file
    /// follows both; never the session's kept look, whose first looks came before the read.
    fn close_read(&self, scope: Scope) -> Result<()> {
        #[cfg(test)]
        {
            FENCES.set(FENCES.get() + 1);
            CLOSES.set(CLOSES.get() + 1);
        }
        let current = Journal::read_existing(self.bootstrap.journal_dir(), self.bootstrap.trust())
            .map_err(failure)?;
        {
            let look = aterm_secrets::client::VaultLook::new(self.bootstrap.paths());
            self.bootstrap
                .revalidate_custody_in(&look)
                .map_err(failure)?;
            // As at the opening fence: a changed prefix needs a fresh complete observation.
            if !current.inventory().eq(self.journal.inventory())
                || !self.access.is_current_in(&current, &look)
            {
                return Err(Error::snapshot_changed());
            }
        } // The look ends before `fenced`'s generation check, which can wait, asks on its own call.
        self.fenced(scope, &current)
    }

    /// What both of a store read batch's fences ([`Self::read_context`], [`Self::close_read`])
    /// ask once they have read the control journal again (`current`) and judged its inventory and
    /// the session's currency.
    fn fenced(&self, scope: Scope, current: &Journal) -> Result<()> {
        // A generation's check can wait, so what follows it is asked afresh (on its own call, not
        // through a look the session keeps), and a journal's own head is its handle's to answer.
        // Under a log with neither, currency asks the log and holds a journal only to its trust:
        // the answer just given, for a journal of that trust.
        if self.generation.is_some() || !self.access.is_log_backed() {
            return self.context_judged(scope, true);
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
        let payload = self.with_payload_key(scope, &envelope, negative, keys, |key, _| {
            let mut payload = Zeroizing::new(envelope.body.to_vec());
            let len = crypto::cipher(key, &envelope.salt, envelope.aad)?
                .open_in_place(
                    aead::Nonce::assume_unique_for_key([0; 12]),
                    aead::Aad::from(envelope.aad),
                    &mut payload,
                )
                .ok()
                .map(|plain| plain.len());
            let Some(len) = len else { return Ok(None) };
            payload.truncate(len);
            Ok(Some(SecretBytes::new(std::mem::take(&mut *payload))))
        })?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        Ok(Some(Record {
            scope,
            roster: envelope.roster,
            epoch: envelope.epoch,
            header: envelope.header,
            authored_ms: envelope.authored_ms,
            payload,
        }))
    }

    /// The shared historical writer and current key admission for full payloads and replay
    /// metadata. An optional cryptographic fact is consulted only inside `open`, after this ask.
    fn with_payload_key<T>(
        &self,
        scope: Scope,
        envelope: &Envelope<'_>,
        negative: bool,
        keys: Option<&ReadKeys>,
        open: impl FnOnce(
            &[u8; 32],
            &std::cell::OnceCell<crypto::PayloadKeyIdentity>,
        ) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        let (roster, epoch, actor) = (envelope.roster, envelope.epoch, envelope.header.actor);
        if !self.history.borrow().contains_key(&roster) {
            // A roster this Mac has not seen, or holds no access to, waits for its view to
            // grow; local contention stays busy.
            let history = self
                .bootstrap
                .historical_in(&self.journal, &self.access, roster, self.look())
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
        if let Some(keys) = keys {
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
                        .with_key_in(&self.journal, self.look(), |network_key| {
                            scope.key(&roster, epoch, network_key)
                        })
                        .map_err(failure)?,
                    identity: std::cell::OnceCell::new(),
                    reused: false,
                }),
            };
            open(&key.value, &key.identity)
        } else {
            history
                .with_key_in(&self.journal, self.look(), |network_key| {
                    open(
                        &scope.key(&roster, epoch, network_key),
                        &std::cell::OnceCell::new(),
                    )
                })
                .map_err(failure)?
        }
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
        Self::parse_signed(scope, wire, |signer| signatures::verify(signer, wire))
    }

    /// [`Self::parse`] of an identified wire's bytes, whose signature is checked under the
    /// identifier they were read with ([`signatures::verify_identified`]).
    fn parse_identified(scope: Scope, wire: &'w signatures::Identified) -> Result<Self> {
        Self::parse_signed(scope, wire.wire(), |signer| {
            signatures::verify_identified(signer, wire)
        })
    }

    /// The envelope `wire` holds, its signature checked by `verify` for its signer, which gives
    /// the wire's content identifier.
    fn parse_signed(
        scope: Scope,
        wire: &'w [u8],
        verify: impl FnOnce(&[u8; 32]) -> Result<Digest>,
    ) -> Result<Self> {
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
        let id = verify(&actor[..32].try_into().unwrap())?;
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
