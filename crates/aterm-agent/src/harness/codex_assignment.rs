// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Canonical assignment custody. A materialized slot is never the authority for
//! the current assignment: each use joins this signed book under its claim.

#[path = "codex_assignment_protocol.rs"]
pub mod protocol;

#[path = "codex_assignment_target_ack.rs"]
pub(in crate::harness) mod target_ack;
use super::native_custody_platform as platform;
#[path = "codex_assignment_policy.rs"]
pub(super) mod policy;
#[path = "codex_assignment_prepare.rs"]
pub(super) mod prepare;
use super::{accounts, cli::Env, codex_grant_owner as owner, codex_session_slot::Caller};
use aterm_secrets::{SecretBytes, age};
pub use policy::{PreparedPreview, ProviderPolicy};
pub use prepare::prepare;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

const MAX_BOOK: usize = 256 * 1024;
const MAX_LIVE: usize = 128;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Consumer {
    pid: u32,
    birth: u64,
    parent: u32,
    group: u32,
    session: u32,
    image: Vec<u8>,
    device: u64,
    inode: u64,
}

impl Consumer {
    fn observe(caller: &Caller) -> Result<Self, String> {
        let process = super::native_act::Process::observe(caller.pid)
            .ok_or("assignment consumer unavailable")?;
        if process.birth != caller.birth || !owner::consumer_capable(&process, true)? {
            return Err("assignment consumer unsupported".into());
        }
        Ok(Self {
            pid: process.pid,
            birth: process.birth,
            parent: process.parent,
            group: process.group,
            session: process.session,
            image: process.image.path,
            device: process.image.device,
            inode: process.image.inode,
        })
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Startup,
    Installing,
    AwaitingAck,
    Applied,
}

/// Signed source correlation. Zero assignment generation is meaningful only
/// with an actual native constructor nonce, never an absent assignment book.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum SourceAuthority {
    Startup { nonce: String },
    Assignment { operation: String },
}

impl SourceAuthority {
    pub(super) fn valid(&self, generation: u64) -> bool {
        match self {
            Self::Startup { nonce } => generation == 0 && hex(nonce, 32),
            Self::Assignment { operation } => generation > 0 && hex(operation, 32),
        }
    }

    pub(super) fn native_intent(&self) -> Option<&str> {
        match self {
            Self::Startup { .. } => None,
            Self::Assignment { operation } => Some(operation),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Current {
    binding: owner::GrantBinding,
    generation: u64,
    previous_generation: u64,
    authority: SourceAuthority,
    boundary: Option<String>,
    phase: Phase,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Book {
    schema: u8,
    caller: Caller,
    consumer: Consumer,
    clock: u64,
    current: Current,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    prepared: Vec<prepare::Record>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    acknowledged: Vec<target_ack::Observation>,
}

impl Book {
    fn valid(&self, caller: &Caller) -> bool {
        ((self.schema == 2 && self.prepared.is_empty() && self.acknowledged.is_empty())
            || (self.schema == 3
                && self.acknowledged.is_empty()
                && !self.prepared.is_empty()
                && self
                    .prepared
                    .iter()
                    .all(|record| record.commit_boundary.is_none()))
            || (self.schema == 4
                && self.acknowledged.is_empty()
                && self
                    .prepared
                    .iter()
                    .any(|record| record.commit_boundary.is_some()))
            || (self.schema == 5 && !self.acknowledged.is_empty()))
            && target_ack::records_valid(self)
            && prepare::records_valid(&self.prepared)
            && prepare::current_record_valid(self)
            && self.caller == *caller
            && key(caller).is_ok()
            && self.consumer.pid == caller.pid
            && self.consumer.birth == caller.birth
            && self.current.generation == self.clock
            && self.current.binding.generation > 0
            && self.current.binding.request.valid()
            && self.current.binding.reference.valid()
            && self.current.authority.valid(self.current.generation)
            && match self.current.authority {
                SourceAuthority::Startup { .. } => {
                    self.current.phase == Phase::Startup
                        && self.current.previous_generation == 0
                        && self.current.boundary.is_none()
                }
                SourceAuthority::Assignment { .. } => {
                    self.current.phase != Phase::Startup
                        && self.current.previous_generation < self.current.generation
                        && self
                            .current
                            .boundary
                            .as_deref()
                            .is_some_and(|id| hex(id, 32))
                }
            }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    book: Book,
    mac: String,
}

struct Claimed {
    store: platform::Store,
    book: Book,
    pending_install: Option<prepare::install::Pending>,
}

type Lease = Arc<Mutex<Claimed>>;
static LIVE: OnceLock<Mutex<HashMap<String, Weak<Mutex<Claimed>>>>> = OnceLock::new();

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn key(caller: &Caller) -> Result<String, String> {
    if caller.pid <= 1
        || caller.birth == 0
        || !caller.sid.starts_with("s-")
        || caller.sid.len() <= 2
        || caller.sid.len() > 64
        || !caller
            .sid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("invalid assignment caller".into());
    }
    let bytes = aterm_json::to_vec(&("aterm/codex-assignment-caller/v1", caller))
        .map_err(|_| "assignment caller unavailable")?;
    Ok(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(&bytes),
    ))
}

fn signature(identity: &age::Identity, book: &Book) -> Result<String, String> {
    let bytes =
        SecretBytes::new(aterm_json::to_vec(book).map_err(|_| "assignment book unavailable")?);
    owner::machine_mac(
        identity,
        b"aterm/codex-session-assignment/key/v1\0",
        bytes.as_slice(),
    )
}

fn open(env: &Env, caller: &Caller) -> Result<Lease, String> {
    open_inner(env, caller, None)
}

fn validate_startup(
    book: &Book,
    binding: &owner::GrantBinding,
    permit: &super::codex_source_registry::InitialSourcePermit,
) -> Result<(), String> {
    permit.current().map_err(|_| "initial source changed")?;
    if !book.valid(permit.caller())
        || binding != permit.binding()
        || book.current.binding != *binding
        || book.current.authority
            != (SourceAuthority::Startup {
                nonce: permit.startup_nonce().into(),
            })
        || book.consumer != Consumer::observe(permit.caller())?
    {
        return Err("initial source does not own this assignment".into());
    }
    Ok(())
}

/// Keeps the authenticated startup book claimed through materialization and
/// the constructor's final peer check. Serialized startup data cannot mint it.
pub(super) struct StartupClaim(Lease);

pub(super) fn initialize_startup(
    env: &Env,
    grant: &owner::AdmittedGrant<'_>,
    permit: &super::codex_source_registry::InitialSourcePermit,
) -> Result<StartupClaim, String> {
    permit.current().map_err(|_| "initial source changed")?;
    if grant.binding() != permit.binding() || !hex(permit.startup_nonce(), 32) {
        return Err("initial source grant changed".into());
    }
    open_inner(env, permit.caller(), Some((grant.binding(), permit))).map(StartupClaim)
}

/// Physical metadata of an actual authenticated Book claim; not a grant.
pub(super) type BookNamespace = platform::PhysicalIdentity;

impl StartupClaim {
    pub(super) fn with_current<R>(
        &self,
        grant: &owner::AdmittedGrant<'_>,
        permit: &super::codex_source_registry::InitialSourcePermit,
        materialize: impl FnOnce(&BookNamespace) -> Result<R, String>,
    ) -> Result<R, String> {
        let claimed = self.0.try_lock().map_err(|_| "assignment custody busy")?;
        validate_startup(&claimed.book, grant.binding(), permit)?;
        claimed.store.unchanged().map_err(str::to_owned)?;
        let namespace = claimed.store.physical_identity().map_err(str::to_owned)?;
        let result = materialize(&namespace)?;
        claimed.store.unchanged().map_err(str::to_owned)?;
        validate_startup(&claimed.book, grant.binding(), permit)?;
        Ok(result)
    }
}

fn open_inner(
    env: &Env,
    caller: &Caller,
    startup: Option<(
        &owner::GrantBinding,
        &super::codex_source_registry::InitialSourcePermit,
    )>,
) -> Result<Lease, String> {
    platform::require_available()?;
    let key = key(caller)?;
    let home = env.home.as_ref().ok_or("assignment home unavailable")?;
    let directory = env
        .accounts_store_for(accounts::Cli::Codex)
        .ok_or("assignment store unavailable")?
        .dir
        .join("session-assignments")
        .join(&key);
    // A live kernel caller can be observed through distinct native config
    // roots. The process-global lease table must not join their authorities.
    let registry_key = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(
        &aterm_json::to_vec(&("aterm/codex-assignment-lease/v1", &directory, home))
            .map_err(|_| "assignment location unavailable")?,
    ));
    let live_books = LIVE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut live = live_books
        .try_lock()
        .map_err(|_| "assignment custody busy")?;
    live.retain(|_, lease| lease.strong_count() > 0);
    if let Some(lease) = live.get(&registry_key).and_then(Weak::upgrade) {
        if let Some((binding, permit)) = startup {
            let claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
            validate_startup(&claimed.book, binding, permit)?;
            permit.note_startup_published();
            claimed.store.unchanged().map_err(str::to_owned)?;
        }
        return Ok(lease);
    }
    if live.len() >= MAX_LIVE {
        return Err("assignment custody busy".into());
    }
    let identity = platform::machine_identity(home)
        .map_err(|_| "assignment identity unavailable")?
        .ok_or("assignment identity unavailable")?;
    let (mut store, bytes) = if startup.is_some() {
        platform::ensure_private_dir(
            directory
                .parent()
                .ok_or("assignment location unavailable")?,
        )
        .map_err(|_| "assignment location unavailable")?;
        platform::Store::open_owned(&directory, home, MAX_BOOK)?
    } else {
        platform::Store::open_owned_existing(&directory, home, MAX_BOOK)?
            .ok_or("assignment is unknown")?
    };
    let previously_published = bytes.is_some();
    let book = if let Some(bytes) = bytes {
        let envelope: Envelope =
            aterm_json::from_slice(bytes.as_slice()).map_err(|_| "assignment book invalid")?;
        if !envelope.book.valid(caller)
            || !hex(&envelope.mac, 64)
            || !aterm_digest::ct_eq(
                signature(&identity, &envelope.book)?.as_bytes(),
                envelope.mac.as_bytes(),
            )
        {
            return Err("assignment book invalid".into());
        }
        envelope.book
    } else {
        let (binding, permit) = startup.ok_or("assignment is unknown")?;
        permit.current().map_err(|_| "initial source changed")?;
        permit
            .require_unpublished_startup()
            .map_err(|_| "initial assignment history unavailable")?;
        Book {
            schema: 2,
            caller: caller.clone(),
            consumer: Consumer::observe(caller)?,
            clock: 0,
            prepared: Vec::new(),
            acknowledged: Vec::new(),
            current: Current {
                binding: binding.clone(),
                generation: 0,
                previous_generation: 0,
                authority: SourceAuthority::Startup {
                    nonce: permit.startup_nonce().into(),
                },
                boundary: None,
                phase: Phase::Startup,
            },
        }
    };
    if let Some((binding, permit)) = startup {
        validate_startup(&book, binding, permit)?;
        if previously_published {
            permit.note_startup_published();
        }
        let mac = signature(&identity, &book)?;
        let envelope = Envelope { book, mac };
        let bytes = SecretBytes::new(
            aterm_json::to_vec(&envelope).map_err(|_| "assignment book unavailable")?,
        );
        // A lost durability acknowledgement is retried only for this exact
        // authenticated startup. No provider request or new assignment occurs.
        store.save(bytes.as_slice())?;
        permit.note_startup_published();
        store.unchanged().map_err(str::to_owned)?;
        permit.current().map_err(|_| "initial source changed")?;
        let lease = Arc::new(Mutex::new(Claimed {
            store,
            book: envelope.book,
            pending_install: None,
        }));
        live.insert(registry_key, Arc::downgrade(&lease));
        return Ok(lease);
    }
    let lease = Arc::new(Mutex::new(Claimed {
        store,
        book,
        pending_install: None,
    }));
    live.insert(registry_key, Arc::downgrade(&lease));
    Ok(lease)
}

/// Only the native assignment state machine may create this lease. Serialized
/// book data and a signed Slot record cannot be deserialized into this proof.
pub(super) struct AssignmentCommit {
    caller: Caller,
    binding: owner::GrantBinding,
    generation: u64,
    previous_generation: u64,
    operation: String,
    boundary: String,
    _lease: Lease,
}

impl AssignmentCommit {
    pub(super) fn with_namespace<R>(
        &self,
        use_assignment: impl FnOnce(&BookNamespace) -> Result<R, String>,
    ) -> Result<R, String> {
        let claimed = self
            ._lease
            .try_lock()
            .map_err(|_| "assignment custody busy")?;
        let current = &claimed.book.current;
        if claimed.pending_install.is_some()
            || !claimed.book.valid(&self.caller)
            || current.binding != self.binding
            || current.generation != self.generation
            || current.authority
                != (SourceAuthority::Assignment {
                    operation: self.operation.clone(),
                })
            || current.boundary.as_deref() != Some(self.boundary.as_str())
            || claimed.book.consumer != Consumer::observe(&self.caller)?
        {
            return Err("installed assignment changed".into());
        }
        claimed.store.unchanged().map_err(str::to_owned)?;
        let namespace = claimed.store.physical_identity().map_err(str::to_owned)?;
        let result = use_assignment(&namespace)?;
        claimed.store.unchanged().map_err(str::to_owned)?;
        if claimed.book.consumer != Consumer::observe(&self.caller)? {
            return Err("assignment consumer changed".into());
        }
        Ok(result)
    }
    pub(super) fn caller(&self) -> &Caller {
        &self.caller
    }
    pub(super) fn binding(&self) -> &owner::GrantBinding {
        &self.binding
    }
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    pub(super) fn previous_assignment_generation(&self) -> u64 {
        self.previous_generation
    }
    pub(super) fn operation(&self) -> &str {
        &self.operation
    }
}

/// Hold the canonical assignment claim through a slot read/write. The shared
/// live lease permits a Source read between install and ACK without reacquiring
/// the same file lock. Each callback remains serialized, bounded and rechecked.
pub(super) fn with_current_assignment<R>(
    env: &Env,
    caller: &Caller,
    binding: &owner::GrantBinding,
    generation: u64,
    authority: &SourceAuthority,
    use_assignment: impl FnOnce() -> Result<R, String>,
) -> Result<R, String> {
    with_current_assignment_namespace(env, caller, binding, generation, authority, |_| {
        use_assignment()
    })
}

pub(super) fn with_current_assignment_namespace<R>(
    env: &Env,
    caller: &Caller,
    binding: &owner::GrantBinding,
    generation: u64,
    authority: &SourceAuthority,
    use_assignment: impl FnOnce(&BookNamespace) -> Result<R, String>,
) -> Result<R, String> {
    let lease = open(env, caller)?;
    let claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
    let current = &claimed.book.current;
    if claimed.pending_install.is_some()
        || prepare::pending_for_current(&claimed.book)
        || !matches!(current.phase, Phase::Startup | Phase::Applied)
    {
        return Err("session assignment is not ready".into());
    }
    if !claimed.book.valid(caller)
        || !authority.valid(generation)
        || current.generation != generation
        || current.authority != *authority
        || current.binding != *binding
        || claimed.book.consumer != Consumer::observe(caller)?
    {
        return Err("session assignment changed".into());
    }
    claimed.store.unchanged().map_err(str::to_owned)?;
    let namespace = claimed.store.physical_identity().map_err(str::to_owned)?;
    let result = use_assignment(&namespace)?;
    claimed.store.unchanged().map_err(str::to_owned)?;
    if claimed.book.consumer != Consumer::observe(caller)? {
        return Err("assignment consumer changed".into());
    }
    Ok(result)
}

/// Reopen the exact captured namespace, not the current default selector. This
/// is read-only authentication; neither metadata nor a path grants authority.
pub(super) fn with_owed_source_namespace<R>(
    env: &Env,
    proof: &super::codex_source_registry::UseNamespace,
    inspect: impl FnOnce(&BookNamespace) -> Result<R, String>,
) -> Result<R, String> {
    with_owed_namespace(
        env,
        proof.caller(),
        proof.source(),
        proof.origin().ok_or("owed source has no reserved origin")?,
        || {
            proof
                .current()
                .map_err(|_| "owed source use changed".into())
        },
        inspect,
    )
}

pub(super) fn with_owed_preload_namespace<R>(
    env: &Env,
    proof: &super::codex_source_registry::PreloadReadNamespace,
    intent: &prepare::TargetIntent,
    inspect: impl FnOnce(&BookNamespace) -> Result<R, String>,
) -> Result<R, String> {
    with_owed_namespace(
        env,
        proof.caller(),
        proof.source(),
        intent,
        || proof.current().map_err(|_| "owed preload changed".into()),
        inspect,
    )
}

fn with_owed_namespace<R>(
    env: &Env,
    caller: &Caller,
    source: &super::codex_session_slot::SourceHandle,
    intent: &prepare::TargetIntent,
    current: impl Fn() -> Result<(), String>,
    inspect: impl FnOnce(&BookNamespace) -> Result<R, String>,
) -> Result<R, String> {
    current()?;
    let lease = open(env, caller)?;
    let claimed = lease.try_lock().map_err(|_| "assignment custody busy")?;
    if claimed.pending_install.is_some()
        || !claimed.book.valid(caller)
        || !prepare::origin_namespace_matches(&claimed.book, intent)?
        || claimed.book.current.binding != *source.binding()
        || claimed.book.current.generation != source.assignment_generation()
        || claimed.book.current.authority != *source.source_authority()
        || claimed.book.consumer != Consumer::observe(caller)?
    {
        return Err("owed source assignment changed".into());
    }
    claimed.store.unchanged().map_err(str::to_owned)?;
    let namespace = claimed.store.physical_identity().map_err(str::to_owned)?;
    let result = inspect(&namespace)?;
    claimed.store.unchanged().map_err(str::to_owned)?;
    current()?;
    if claimed.book.consumer != Consumer::observe(caller)? {
        return Err("owed source consumer changed".into());
    }
    Ok(result)
}

/// Reopen the exact captured namespace, not the current default selector. This
/// is read-only authentication; neither metadata nor a path grants authority.
pub(super) fn authenticate_namespace<R>(
    home: &std::path::Path,
    namespace: &BookNamespace,
    caller: &Caller,
    binding: &owner::GrantBinding,
    generation: u64,
    authority: &SourceAuthority,
    inspect: impl FnOnce() -> Result<R, String>,
) -> Result<R, String> {
    #[cfg(unix)]
    {
        if !namespace.current() || !authority.valid(generation) {
            return Err("assignment namespace changed".into());
        }
        let identity = platform::machine_identity(home)
            .map_err(|_| "assignment identity unavailable")?
            .ok_or("assignment identity unavailable")?;
        // A typed installed assignment already retains the canonical claim.
        // Capture candidates, release the global table, then join the exact
        // physical namespace under its existing Book mutex. Never reacquire its
        // ProfileLock or perform filesystem probes under the global table.
        let candidates: Vec<_> = if let Some(live) = LIVE.get() {
            live.try_lock()
                .map_err(|_| "assignment custody busy")?
                .values()
                .filter_map(Weak::upgrade)
                .collect()
        } else {
            Vec::new()
        };
        for lease in candidates {
            let Ok(claimed) = lease.try_lock() else {
                continue;
            };
            if claimed.store.physical_identity().map_err(str::to_owned)? != *namespace {
                continue;
            }
            if claimed.pending_install.is_some() {
                return Err("assignment write is pending".into());
            }
            let ciphertext = aterm_secrets::fsx::read_private(
                &namespace.directory().join("candidate.age"),
                MAX_BOOK,
            )
            .map_err(|_| "assignment book unavailable")?
            .ok_or("assignment book unavailable")?;
            let plaintext = age::decrypt(std::slice::from_ref(&identity), ciphertext.as_slice())
                .map_err(|_| "assignment book unavailable")?;
            let envelope: Envelope = aterm_json::from_slice(plaintext.as_slice())
                .map_err(|_| "assignment book invalid")?;
            if !envelope.book.valid(caller)
                || !hex(&envelope.mac, 64)
                || !aterm_digest::ct_eq(
                    signature(&identity, &envelope.book)?.as_bytes(),
                    envelope.mac.as_bytes(),
                )
                || !aterm_digest::ct_eq(
                    signature(&identity, &claimed.book)?.as_bytes(),
                    envelope.mac.as_bytes(),
                )
                || envelope.book.current.binding != *binding
                || envelope.book.current.generation != generation
                || envelope.book.current.authority != *authority
                || envelope.book.consumer != Consumer::observe(caller)?
            {
                return Err("assignment namespace authority changed".into());
            }
            claimed.store.unchanged().map_err(str::to_owned)?;
            let result = inspect()?;
            claimed.store.unchanged().map_err(str::to_owned)?;
            if claimed.book.consumer != Consumer::observe(caller)? {
                return Err("assignment consumer changed".into());
            }
            return Ok(result);
        }
        let (store, bytes) =
            platform::Store::open_owned_existing(namespace.directory(), home, MAX_BOOK)?
                .ok_or("assignment is unknown")?;
        let bytes = bytes.ok_or("assignment is unknown")?;
        let envelope: Envelope =
            aterm_json::from_slice(bytes.as_slice()).map_err(|_| "assignment book invalid")?;
        if !envelope.book.valid(caller)
            || !hex(&envelope.mac, 64)
            || !aterm_digest::ct_eq(
                signature(&identity, &envelope.book)?.as_bytes(),
                envelope.mac.as_bytes(),
            )
            || envelope.book.current.binding != *binding
            || envelope.book.current.generation != generation
            || envelope.book.current.authority != *authority
            || envelope.book.consumer != Consumer::observe(caller)?
            || store.physical_identity().map_err(str::to_owned)? != *namespace
        {
            return Err("assignment namespace authority changed".into());
        }
        store.unchanged().map_err(str::to_owned)?;
        let result = inspect()?;
        store.unchanged().map_err(str::to_owned)?;
        if envelope.book.consumer != Consumer::observe(caller)? {
            return Err("assignment consumer changed".into());
        }
        Ok(result)
    }
    #[cfg(not(unix))]
    {
        let _ = (
            home, namespace, caller, binding, generation, authority, inspect,
        );
        Err("assignment namespace unavailable".into())
    }
}

#[cfg(test)]
#[path = "codex_assignment_tests.rs"]
mod tests;
