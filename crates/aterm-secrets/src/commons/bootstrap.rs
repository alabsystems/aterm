// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Offline bootstrap from an authenticated vault observation. Device secrets
//! are age-encrypted; immutable public claims detect key loss and substitution.
//! Preparing a device or a journal grants no membership or application access.

use super::{
    Snapshot,
    binding::{Binding, Request},
    constraints::Fact,
    journal::{Journal, State, Trust},
    node::{Kind, MAX_NODE, Node},
};
use crate::{
    Error, Result, SecretBytes,
    age::{self, Identity},
    client::{self, Paths},
    fsx,
};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DESCRIPTOR_HEADER: usize = 4 + 16 + 32 + 1;
const MAX_DESCRIPTOR: usize = DESCRIPTOR_HEADER + 3 * (4 + MAX_NODE);
const DEVICE_LEN: usize = 4 + 32 + 16 + 16 + 32;
const CLAIM_LEN: usize = 4 + 32 + 16 + 16 + 32;
const PIN_LEN: usize = 4 + 16 + 32 + 32;
const MAX_DEVICE: usize = 4096;

fn custody_digest(path: &Path) -> Result<[u8; 32]> {
    fsx::private_digest(path, MAX_DEVICE, None)?
        .ok_or_else(|| Error::new("Commons immutable custody is missing"))
}

fn established_files(paths: &Paths) -> Result<()> {
    use aterm_uds::private_file::JudgedDir;
    let commons = paths.dir.join("commons");
    let control = commons.join("control");
    // One verdict for each directory, every file in it still opened and judged on its descriptor.
    let state = JudgedDir::private(&paths.dir)?;
    let device = JudgedDir::private(&commons)?;
    let journal = JudgedDir::private(&control)?;
    for (dir, name) in [
        (&state, "identity"),
        (&state, "commons.claim"),
        (&state, "commons.lock"),
        (&state, "commons-device.lock"),
        (&device, "device.age"),
        (&device, "device.claim"),
        (&device, "default.pin"),
        (&journal, "control.anchor"),
    ] {
        let (_, metadata) = dir
            .open(name)
            .map_err(|_| Error::new("existing Commons custody file is unavailable"))?;
        if metadata.nlink() != 1 || metadata.len() > MAX_DEVICE as u64 {
            return Err(Error::new(
                "existing Commons custody file is not privately held",
            ));
        }
    }
    Ok(())
}

/// Check existing immutable custody against a proposed recovery's original
/// root without creating files, opening a writer or ingesting a descriptor.
pub(crate) fn validate_established_root(paths: &Paths, trust: Trust) -> Result<Journal> {
    established_files(paths)?;
    let pin = fsx::read_private(&paths.dir.join("commons/default.pin"), PIN_LEN)?;
    let claim = fsx::read_private(&paths.dir.join("commons.claim"), PIN_LEN)?;
    if pin
        .as_ref()
        .is_none_or(|bytes| bytes.as_slice() != pin_bytes(trust))
        || claim
            .as_ref()
            .is_none_or(|bytes| bytes.as_slice() != claim_bytes(trust))
    {
        return Err(Error::new(
            "Commons recovery differs from existing immutable custody",
        ));
    }
    Journal::read_existing(&paths.dir.join("commons/control"), trust)
}

/// The trust anchor this installation pinned when it first opened Commons control from a
/// descriptor its authenticated vault delivered: `commons/default.pin`, which only that first
/// observation writes and no later one replaces. Its `commons.claim` and the control journal's own
/// anchor must name the same anchor ([`validate_established_root`]'s checks). It is read whether
/// or not the current Commons epochs still give this installation control, and reading it creates,
/// repairs and ingests nothing and grants no control. `None` where no pin is held: this
/// installation never opened Commons control, or lost its pin, and either way has no anchor here
/// to trust.
///
/// # Errors
/// The pin is malformed, or the established custody beside it is incomplete, not privately held,
/// or names another anchor.
pub(crate) fn established_trust(paths: &Paths) -> Result<Option<Trust>> {
    let Some(pin) = fsx::read_private(&paths.dir.join("commons/default.pin"), PIN_LEN)? else {
        return Ok(None);
    };
    let bytes = pin.as_slice();
    if bytes.len() != PIN_LEN || &bytes[..4] != b"ACP1" {
        return Err(Error::new("Commons network pin is malformed"));
    }
    let trust = Trust {
        network: bytes[4..20].try_into().unwrap(),
        authority: bytes[20..52].try_into().unwrap(),
        genesis: bytes[52..84].try_into().unwrap(),
    };
    established_files(paths)?;
    let claim = fsx::read_private(&paths.dir.join("commons.claim"), PIN_LEN)?;
    if claim
        .as_ref()
        .is_none_or(|kept| kept.as_slice() != claim_bytes(trust))
    {
        return Err(Error::new(
            "Commons network pin differs from its retained history claim",
        ));
    }
    Journal::read_existing(&paths.dir.join("commons/control"), trust)?;
    Ok(Some(trust))
}

/// Fallback custody wait for direct snapshot opens without a shared deadline.
/// Default custody admissions use [`REQUIRED_WAIT`]; their initial vault look is nonblocking.
const CUSTODY_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// A required observation waits for local work under one shared hang detector.
const REQUIRED_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Serializes custody observations independently of the removable control log.
/// The lock is never replaced, and never held during network I/O.
struct CustodyLock(std::fs::File);

impl CustodyLock {
    #[cfg(test)]
    /// Take `name` in `paths.dir`: with `wait`, for as long as it takes; else
    /// for at most [`CUSTODY_WAIT`], refused as [`crate::ErrorKind::CustodyBusy`].
    fn acquire_commons_custody(paths: &Paths, name: &str, wait: bool) -> Result<Self> {
        Self::acquire_until(paths, name, wait, None)
    }

    fn acquire_until(
        paths: &Paths,
        name: &str,
        wait: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        Self::acquire_until_with(paths, name, wait, deadline, || {})
    }

    fn acquire_until_with(
        paths: &Paths,
        name: &str,
        wait: bool,
        deadline: Option<std::time::Instant>,
        held: impl FnMut(),
    ) -> Result<Self> {
        Self::acquire_policy(paths, name, wait, deadline, false, held)
    }

    fn observe_until_with(
        paths: &Paths,
        name: &str,
        deadline: Option<std::time::Instant>,
        held: impl FnMut(),
    ) -> Result<Self> {
        Self::acquire_policy(paths, name, false, deadline, true, held)
    }

    fn acquire_policy(
        paths: &Paths,
        name: &str,
        wait: bool,
        deadline: Option<std::time::Instant>,
        existing_only: bool,
        mut held: impl FnMut(),
    ) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        if existing_only {
            aterm_uds::private_file::check_private_dir(&paths.dir)?;
        } else {
            fsx::ensure_private_dir(&paths.dir)?;
        }
        let path = paths.dir.join(name);
        // This empty inode is the lock, not a replaceable data record. A
        // hard-link publication briefly has two names and is correctly refused
        // by concurrent private-file readers. Publish it with one name from
        // the first instant; never truncate or replace an existing lock.
        if !existing_only {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&path)
            {
                Ok(file) => {
                    aterm_durable::durable(&file)?;
                    aterm_durable::durable_dir(&paths.dir)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        let (file, metadata) =
            aterm_uds::kept_out(|| aterm_uds::private_file::open_private(&paths.dir, name))
                .map_err(|error| {
                    Error::new(format!("Commons custody lock is not private: {error}"))
                })?;
        let custody_file: std::fs::File = file;
        let mut observed_held = || {
            #[cfg(test)]
            tests::admission_held();
            held();
        };
        let result = if let Some(deadline) = deadline {
            aterm_uds::file_lock::lock_exclusive_until_with(
                &custody_file,
                deadline,
                "another local Commons bootstrap is active",
                &mut observed_held,
            )
        } else if wait {
            #[expect(
                clippy::disallowed_methods,
                reason = "CustodyLock is CLOEXEC/CLOFORK and unlocks before close; device creation has no network IO"
            )]
            custody_file.lock()
        } else {
            aterm_uds::file_lock::lock_exclusive_until_with(
                &custody_file,
                std::time::Instant::now() + CUSTODY_WAIT,
                "another local Commons bootstrap is active",
                &mut observed_held,
            )
        };
        result.map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                Error::of_kind(crate::ErrorKind::CustodyBusy, error.to_string())
            } else {
                Error::from(error)
            }
        })?;
        let lock = Self(custody_file);
        let (_, current) = aterm_uds::private_file::open_private(&paths.dir, name)
            .map_err(|error| Error::new(format!("Commons custody lock is not private: {error}")))?;
        if (current.dev(), current.ino()) != (metadata.dev(), metadata.ino()) {
            return Err(Error::new(
                "Commons custody lock changed while acquiring it",
            ));
        }
        Ok(lock)
    }
}

impl Drop for CustodyLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A genesis packaged for the reserved, encrypted Commons vault entry. Only an
/// authenticated vault observation or independently pinned journal establishes
/// its authority; a public foundation or self-signature grants no admission.
#[derive(Clone, Debug)]
pub struct Descriptor {
    genesis: Node,
    repair: Vec<Node>,
}

impl Descriptor {
    /// Package an ACN3 genesis or canonical ACF1 foundation. Retain the node before
    /// placing these bytes in the reserved explicit-member vault entry.
    pub fn from_genesis(genesis: Node) -> Result<Self> {
        if genesis.kind() != Kind::Genesis
            || !genesis.binding_evidence()
            || genesis.network() == [0; 16]
            || genesis.authority() == [0; 32]
        {
            return Err(Error::new(
                "Commons descriptor requires an ACN3 or ACF1 genesis",
            ));
        }
        Ok(Self {
            genesis,
            repair: Vec::new(),
        })
    }

    /// Publish the genesis and the current head/basis as repair witnesses. An
    /// enrolling device can authenticate sealed repair before obtaining all
    /// intermediate ancestry. These witnesses never substitute for that history.
    pub fn from_journal(journal: &Journal) -> Result<Self> {
        let State::Ready { head, basis } = journal.state() else {
            return Err(Error::new(
                "Commons descriptor requires complete unambiguous control history",
            ));
        };
        let genesis = journal
            .node(&journal.trust().genesis)
            .ok_or_else(|| Error::new("Commons descriptor genesis is missing"))?;
        let mut descriptor = Self::from_genesis(genesis.clone())?;
        for id in [head, basis] {
            if id != genesis.id() && !descriptor.repair.iter().any(|node| node.id() == id) {
                let node = journal
                    .node(&id)
                    .ok_or_else(|| Error::new("Commons descriptor witness is missing"))?;
                if !node.binding_evidence() {
                    return Err(Error::new(
                        "Commons descriptor requires ACN3 repair witnesses",
                    ));
                }
                descriptor.repair.push(node.clone());
            }
        }
        descriptor.repair.sort_by_key(Node::id);
        Ok(descriptor)
    }

    /// Exact binary value for `aterm-commons-control-v1`. The vault serializer
    /// handles binary values; no plaintext materialization is necessary.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(DESCRIPTOR_HEADER + 4 + self.genesis.bytes().len());
        out.extend_from_slice(b"ACD1");
        out.extend_from_slice(&self.genesis.network());
        out.extend_from_slice(&self.genesis.authority());
        out.push(1 + self.repair.len() as u8);
        for node in std::iter::once(&self.genesis).chain(&self.repair) {
            out.extend_from_slice(&(node.bytes().len() as u32).to_le_bytes());
            out.extend_from_slice(node.bytes());
        }
        out
    }

    pub fn trust(&self) -> Trust {
        Trust {
            network: self.genesis.network(),
            authority: self.genesis.authority(),
            genesis: self.genesis.id(),
        }
    }

    /// Decode and verify a Commons control value without creating custody or ingesting history.
    /// Only an authenticated vault observation or independently pinned journal establishes
    /// the returned descriptor's authority; decoding alone grants no admission or access.
    ///
    /// # Errors
    /// The schema, node bounds, signatures, roots or repair witnesses are invalid.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < DESCRIPTOR_HEADER || bytes.len() > MAX_DESCRIPTOR || &bytes[..4] != b"ACD1"
        {
            return Err(Error::new("invalid Commons bootstrap descriptor"));
        }
        let count = bytes[52];
        if !(1..=3).contains(&count) {
            return Err(Error::new("invalid Commons descriptor node count"));
        }
        let mut at = DESCRIPTOR_HEADER;
        let mut nodes = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let size_bytes = bytes
                .get(at..at + 4)
                .ok_or_else(|| Error::new("truncated Commons descriptor"))?;
            let size = u32::from_le_bytes(size_bytes.try_into().unwrap()) as usize;
            at += 4;
            if size > MAX_NODE || size > bytes.len() - at {
                return Err(Error::new("invalid Commons descriptor node size"));
            }
            nodes.push(Node::verify(
                &bytes[at..at + size],
                bytes[4..20].try_into().unwrap(),
                bytes[20..52].try_into().unwrap(),
            )?);
            at += size;
        }
        let mut descriptor = Self::from_genesis(nodes.remove(0))?;
        let mut previous = None;
        for node in nodes {
            if !node.binding_evidence()
                || node.kind() == Kind::Genesis
                || previous.is_some_and(|id| id >= node.id())
            {
                return Err(Error::new("invalid Commons descriptor repair witnesses"));
            }
            previous = Some(node.id());
            descriptor.repair.push(node);
        }
        if at != bytes.len() {
            return Err(Error::new("trailing bytes in Commons descriptor"));
        }
        Ok(descriptor)
    }
}

/// Persistent local device identity, shared across this user's network slots.
/// The machine UUID and writer incarnation are independent of human names and
/// network genesis, allowing the founding device to prepare its enrollment too.
pub struct Device {
    identity: Identity,
    machine: [u8; 16],
    incarnation: [u8; 16],
    seed: SecretBytes,
    /// The lab mark of the identity file this device was opened from ([`Device::lab`]).
    lab: Option<crate::LabMark>,
    /// The requests [`Device::request`] made, by the network and authority each names, at most
    /// [`REQUESTS`]: a request is a pure function of those and of this device's fields, which never
    /// change, so it is made once for each.
    requests: std::sync::Mutex<Vec<KeptRequest>>,
}

/// A request a [`Device`] keeps, after the network and authority it names.
type KeptRequest = ([u8; 16], [u8; 32], Request);

/// How many requests a [`Device`] keeps: its own trust's and the binding check's
/// ([`Device::generate`], [`Device::decode`]).
const REQUESTS: usize = 4;

impl Device {
    /// Load or create an encrypted device using an EXISTING secrets identity.
    /// No age identity, network, trust pin or admission is created here. A lost
    /// or unreadable device file is never replaced while its public claim exists.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDeviceCustody",
            action = "Fresh",
            project = "aterm_secrets::commons::bootstrap::tests::custody_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDeviceCustody",
            action = "Restart",
            project = "aterm_secrets::commons::bootstrap::tests::custody_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDeviceCustody",
            action = "UnclaimedKey",
            project = "aterm_secrets::commons::bootstrap::tests::custody_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDeviceCustody",
            action = "MissingKey",
            project = "aterm_secrets::commons::bootstrap::tests::custody_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDeviceCustody",
            action = "DamagedKey",
            project = "aterm_secrets::commons::bootstrap::tests::custody_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDeviceCustody",
            action = "WrongRecipient",
            project = "aterm_secrets::commons::bootstrap::tests::custody_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDeviceCustody",
            action = "ChangedClaim",
            project = "aterm_secrets::commons::bootstrap::tests::custody_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDeviceCustody",
            action = "MissingIdentity",
            project = "aterm_secrets::commons::bootstrap::tests::custody_transition"
        )
    )]
    pub fn open(paths: &Paths) -> Result<Option<Self>> {
        let Some(identity) = client::read_identity(paths)? else {
            return Ok(None);
        };
        Self::open_identity(paths, identity, false).map(Some)
    }

    /// The lab mark of the secrets identity this device was opened from
    /// (`docs/DESIGN-alab-one-mesh-2026-11-05.md` A12): its file's, or, for an identity an older
    /// build made unmarked, `VirtualMachine` when this process runs in a virtual machine
    /// (`client::wire_mark`). A joiner's `ask`, `run` and `prove` lines carry it, and a desk on
    /// the live ALab genesis refuses them. `None` for a device made from an identity in hand
    /// rather than a file.
    #[must_use]
    pub const fn lab(&self) -> Option<crate::LabMark> {
        self.lab
    }

    pub(crate) fn open_existing(paths: &Paths) -> Result<Self> {
        let identity = client::read_identity(paths)?
            .ok_or_else(|| Error::new("Commons secrets identity is missing"))?;
        Self::open_identity(paths, identity, true)
    }

    pub(crate) fn open_existing_until(paths: &Paths, deadline: std::time::Instant) -> Result<Self> {
        let identity = client::read_identity(paths)?
            .ok_or_else(|| Error::new("Commons secrets identity is missing"))?;
        Self::open_identity_checked(paths, identity, true, Some(deadline), false, || {
            if std::time::Instant::now() >= deadline {
                Err(Error::new("Commons device deadline expired"))
            } else {
                Ok(())
            }
        })
    }

    /// Observe an existing authenticated binding without acquiring device custody or repairing
    /// files. This grants no bootstrap or membership. A present public claim must match.
    ///
    /// `None` is a Mac with no device, neither key nor claim, which is no node yet. A key gone from
    /// under its claim is that node's device lost, and an error as a failed read is: this Mac may
    /// be any node, so a caller that needs the answer has none.
    pub(crate) fn existing_binding_for_observation(
        paths: &Paths,
        identity: &Identity,
    ) -> Result<Option<Binding>> {
        Self::observe_binding(paths, identity, || {})
    }

    fn observe_binding(
        paths: &Paths,
        identity: &Identity,
        before_claim: impl FnOnce(),
    ) -> Result<Option<Binding>> {
        type Key = ([u8; 32], [u8; 32]);
        type Public = (Binding, [u8; 32]);
        static BINDINGS: std::sync::Mutex<std::collections::VecDeque<(Key, Public)>> =
            std::sync::Mutex::new(std::collections::VecDeque::new());
        let dir = paths.dir.join("commons");
        let Some(ciphertext) = fsx::read_private(&dir.join("device.age"), MAX_DEVICE)? else {
            return match fsx::read_private(&dir.join("device.claim"), CLAIM_LEN)? {
                Some(_) => Err(Error::new("Commons device key is missing")),
                None => Ok(None),
            };
        };
        let key = (
            identity.recipient().0,
            aterm_digest::Sha256::digest(ciphertext.as_slice()),
        );
        let cached = BINDINGS.lock().ok().and_then(|cache| {
            cache
                .iter()
                .find(|(held, _)| *held == key)
                .map(|(_, public)| public.clone())
        });
        let was_cached = cached.is_some();
        let (binding, expected) = match cached {
            Some(public) => public,
            None => {
                let device = Self::decode(
                    Identity::from_secret_bytes(*identity.secret_bytes()),
                    ciphertext.as_slice(),
                )?;
                (
                    device.request([1; 16], [2; 32])?.binding().clone(),
                    aterm_digest::Sha256::digest(device.claim()),
                )
            }
        };
        before_claim();
        let current_claim = fsx::read_private(&dir.join("device.claim"), CLAIM_LEN)?;
        if current_claim
            .as_ref()
            .is_some_and(|claim| aterm_digest::Sha256::digest(claim.as_slice()) != expected)
        {
            return Err(Error::new(
                "Commons device differs from its immutable claim",
            ));
        }
        // Only an authenticated public binding and claim digest survive. Every use
        // privately reads the files again; denial is evaluated by the caller's current
        // roster, never by this cache.
        if !was_cached && let Ok(mut cache) = BINDINGS.lock() {
            if cache.len() >= 16 {
                cache.pop_front();
            }
            cache.push_back((key, (binding.clone(), expected)));
        }
        Ok(Some(binding))
    }

    #[cfg(test)]
    fn denied_in_roster(
        paths: &Paths,
        identity: Identity,
        roster: &crate::swarm_roster::SwarmRoster,
    ) -> Result<bool> {
        let Some(binding) = Self::observe_binding(paths, &identity, || {})? else {
            return Ok(false);
        };
        Ok([
            binding.writer().as_slice(),
            binding.incarnation().as_slice(),
            binding.machine().as_slice(),
        ]
        .iter()
        .any(|id| roster.is_node_denied(&aterm_codec::hex::encode_lower(id))))
    }

    fn open_identity(paths: &Paths, identity: Identity, require_existing: bool) -> Result<Self> {
        Self::open_identity_until(paths, identity, require_existing, None)
    }

    fn open_identity_until(
        paths: &Paths,
        identity: Identity,
        require_existing: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        Self::open_identity_checked(
            paths,
            identity,
            require_existing,
            deadline,
            false,
            || Ok(()),
        )
    }

    fn open_identity_checked(
        paths: &Paths,
        identity: Identity,
        require_existing: bool,
        deadline: Option<std::time::Instant>,
        observe_only: bool,
        validate: impl FnOnce() -> Result<()>,
    ) -> Result<Self> {
        // Device creation and loss checks share one lock even when invoked
        // without a network bootstrap. No caller holding this lock takes the
        // bootstrap lock, so bootstrap -> device is the sole lock order.
        let _lock = if observe_only {
            CustodyLock::observe_until_with(paths, "commons-device.lock", deadline, || {})?
        } else {
            CustodyLock::acquire_until(paths, "commons-device.lock", true, deadline)?
        };
        validate()?;
        // The identity's lab mark comes from the same read that proves the file still holds this
        // key: the mark stays the one its file was made with.
        let Some((_, lab)) = client::read_identity_marked(paths)?
            .filter(|(current, _)| current.recipient() == identity.recipient())
        else {
            return Err(Error::new(
                "Commons secrets identity changed while acquiring device custody",
            ));
        };
        // The secrets installer flushes identity contents before publishing its
        // name. Order that name before any ciphertext depending on this key.
        aterm_durable::durable_dir(&paths.dir)?;
        let dir = paths.dir.join("commons");
        let established = require_existing
            || client::creation_requires_device(paths)?
            || fsx::kind(&paths.dir.join("commons.claim"))? != fsx::Kind::Missing;
        if established && fsx::kind(&dir)? == fsx::Kind::Missing {
            return Err(Error::new(
                "established Commons device directory is missing; recovery is required",
            ));
        }
        if observe_only {
            aterm_uds::private_file::check_private_dir(&dir)?;
        } else {
            fsx::ensure_private_dir(&dir)?;
        }
        let mut encrypted = fsx::read_private(&dir.join("device.age"), MAX_DEVICE)?;
        let claim = fsx::read_private(&dir.join("device.claim"), CLAIM_LEN)?;
        if observe_only && (encrypted.is_none() || claim.is_none()) {
            return Err(Error::new("existing Commons device custody is incomplete"));
        }
        if encrypted.is_none()
            && (established
                || claim.is_some()
                || has_history(paths)?
                || fsx::kind(&dir.join("default.pin"))? != fsx::Kind::Missing)
        {
            // A creator may have published its key, claim and journal since
            // our first reads. The journal appears after the key, so re-read
            // before diagnosing established key loss; never generate here.
            encrypted = fsx::read_private(&dir.join("device.age"), MAX_DEVICE)?;
            if encrypted.is_none() {
                return Err(Error::new(
                    "Commons device key is missing; recovery is required",
                ));
            }
        }
        let mut device = match encrypted {
            Some(encrypted) => Self::decode(identity, encrypted.as_slice())?,
            None => {
                let candidate = Self::generate(identity)?;
                let encrypted = age::encrypt(
                    &[candidate.identity.recipient()],
                    candidate.encode().as_slice(),
                )?;
                fsx::write_new(&dir.join("device.age"), &encrypted, 0o600)?;
                // Always read the winner: another aterm may have created it.
                let kept = fsx::read_private(&dir.join("device.age"), MAX_DEVICE)?
                    .ok_or_else(|| Error::new("Commons device creation was not retained"))?;
                Self::decode(candidate.identity, kept.as_slice())?
            }
        };
        device.lab = client::wire_mark(lab, crate::Making::now());
        let expected = device.claim();
        if claim
            .as_ref()
            .is_some_and(|claim| claim.as_slice() != expected)
        {
            return Err(Error::new(
                "Commons device differs from its immutable claim",
            ));
        }
        // Flush the key name before publishing its claim. write_new flushes the
        // file; this explicit directory flush propagates errors to the caller.
        aterm_durable::durable_dir(&dir)?;
        if claim.is_none() {
            fsx::write_new(&dir.join("device.claim"), &expected, 0o600)?;
        }
        let kept = fsx::read_private(&dir.join("device.claim"), CLAIM_LEN)?
            .ok_or_else(|| Error::new("Commons device claim was not retained"))?;
        if kept.as_slice() != expected {
            return Err(Error::new("Commons device claim changed during creation"));
        }
        aterm_durable::durable_dir(&dir)?;
        aterm_durable::durable_dir(&paths.dir)?;
        Ok(device)
    }

    fn generate(identity: Identity) -> Result<Self> {
        let mut random = [0; 64];
        if let Err(error) = aterm_uds::rand::fill(&mut random) {
            zeroize::Zeroize::zeroize(&mut random);
            return Err(error.into());
        }
        let bytes = SecretBytes::from(random.as_slice());
        zeroize::Zeroize::zeroize(&mut random);
        let device = Self {
            identity,
            machine: bytes.as_slice()[..16].try_into().unwrap(),
            incarnation: bytes.as_slice()[16..32].try_into().unwrap(),
            seed: SecretBytes::from(&bytes.as_slice()[32..]),
            lab: None,
            requests: std::sync::Mutex::new(Vec::new()),
        };
        // Binding validation also rejects zero identifiers and unusable keys.
        device.request([1; 16], [2; 32])?;
        Ok(device)
    }

    fn encode(&self) -> SecretBytes {
        let mut out = Vec::with_capacity(DEVICE_LEN);
        out.extend_from_slice(b"ACV1");
        out.extend_from_slice(&self.identity.recipient().0);
        out.extend_from_slice(&self.machine);
        out.extend_from_slice(&self.incarnation);
        out.extend_from_slice(self.seed.as_slice());
        SecretBytes::new(out)
    }

    fn decode(identity: Identity, ciphertext: &[u8]) -> Result<Self> {
        let plain = age::decrypt(std::slice::from_ref(&identity), ciphertext)
            .map_err(|_| Error::new("cannot decrypt Commons device key"))?;
        let bytes = plain.as_slice();
        if bytes.len() != DEVICE_LEN
            || &bytes[..4] != b"ACV1"
            || bytes[4..36] != identity.recipient().0
        {
            return Err(Error::new("invalid Commons device key record"));
        }
        let device = Self {
            identity,
            machine: bytes[36..52].try_into().unwrap(),
            incarnation: bytes[52..68].try_into().unwrap(),
            seed: SecretBytes::from(&bytes[68..]),
            lab: None,
            requests: std::sync::Mutex::new(Vec::new()),
        };
        device.request([1; 16], [2; 32])?;
        Ok(device)
    }

    fn claim(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(CLAIM_LEN);
        out.extend_from_slice(b"ACC1");
        out.extend_from_slice(&self.identity.recipient().0);
        out.extend_from_slice(&self.machine);
        out.extend_from_slice(&self.incarnation);
        self.with_seed(|seed| {
            use ring::signature::{Ed25519KeyPair, KeyPair};
            let pair = Ed25519KeyPair::from_seed_unchecked(seed).expect("validated device seed");
            out.extend_from_slice(pair.public_key().as_ref());
        });
        out
    }

    pub const fn machine(&self) -> &[u8; 16] {
        &self.machine
    }

    /// Self-signed enrollment request; authority approval still requires the
    /// existing fresh, single-use joint-possession challenge protocol.
    ///
    /// Made once for each network and authority: Ed25519 signing is deterministic (RFC 8032), so
    /// the request kept is the one [`Request::create`] would make again. A quiet Commons repair
    /// round asks for it every time it opens this Mac's access (`Bootstrap::access_in_observed`),
    /// 18 times a round on each side of a loopback pair, and each made a key pair from the seed,
    /// signed and verified its own signature: 21% of the round's CPU (2026-11-08). A request that
    /// fails is not kept.
    pub fn request(&self, network: [u8; 16], authority: [u8; 32]) -> Result<Request> {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, _, request)) = requests
            .iter()
            .find(|(n, a, _)| *n == network && *a == authority)
        {
            return Ok(request.clone());
        }
        let request = self.with_seed(|seed| {
            Request::create(
                network,
                authority,
                self.machine,
                self.incarnation,
                &self.identity,
                seed,
            )
        })?;
        if requests.len() >= REQUESTS {
            requests.remove(0);
        }
        requests.push((network, authority, request.clone()));
        Ok(request)
    }

    /// Open an age file sealed to this device's identity: the bundle a member hands after a
    /// proof (`members::joiner::prove`).
    ///
    /// # Errors
    /// The file is not sealed to this identity, or is damaged.
    pub fn open_sealed(&self, sealed: &[u8]) -> Result<SecretBytes> {
        crate::age::decrypt(std::slice::from_ref(&self.identity), sealed).map_err(|error| {
            Error::new(format!(
                "the member's bundle does not open with this Mac's key: {error}"
            ))
        })
    }

    pub fn answer(&self, request: &Request, challenge: &[u8]) -> Result<Vec<u8>> {
        if request.binding().machine() != &self.machine
            || request.binding().incarnation() != &self.incarnation
        {
            return Err(Error::new("Commons challenge belongs to another device"));
        }
        self.with_seed(|seed| request.answer(&self.identity, seed, challenge))
    }

    /// Expose the seed only to a trusted in-process signing operation. The caller
    /// must not serialize it; application epoch keys come from the current roster.
    pub fn with_seed<T>(&self, use_seed: impl FnOnce(&[u8; 32]) -> T) -> T {
        use_seed(self.seed.as_slice().try_into().unwrap())
    }
}

/// Authenticated default network plus its durable local device and journal path.
/// There is no current-membership claim in this type.
pub struct Bootstrap {
    descriptor: Descriptor,
    device: Device,
    journal_dir: PathBuf,
    paths: Paths,
    snapshot: Snapshot,
    immutable: Vec<(PathBuf, [u8; 32])>,
}

/// One authenticated vault observation followed by Commons custody validation.
/// Neither an unavailable result nor an error grants initialization authority.
pub enum Availability {
    Unavailable(super::Unavailable),
    Available(Box<Bootstrap>),
}

/// Whether this Mac is a current Commons Writer, read from custody it already holds: a Mac
/// with no device key yet is none, and this never creates one (a credential request's
/// readiness, docs/DESIGN-credential-requests-2026-10-27.md §9.4). Any failure reads as no.
pub fn writer_here(paths: &Paths) -> bool {
    if !matches!(
        fsx::kind(&paths.dir.join("commons").join("device.age")),
        Ok(fsx::Kind::File)
    ) {
        return false;
    }
    let Ok(Some(bootstrap)) = Bootstrap::cached(paths) else {
        return false;
    };
    let Ok(journal) = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()) else {
        return false;
    };
    matches!(
        bootstrap.access_in(&journal),
        Ok(Some(access)) if access.local().role() == super::roster::Role::Writer
            && access.is_current(&journal)
    )
}

/// What a home that kept its Commons claim and lost its `commons` folder (a partial reset) is
/// refused with: custody to recover, never one a fresh open makes again.
const MISSING_COMMONS_DIRECTORY: &str =
    "established Commons directory is missing; recovery is required";

/// Whether this installation has opened Commons control here: it keeps a claim, a pin, or control
/// or application history, or its `commons` is a symlink, whose contents cannot be told from here
/// ([`retains_control`]). `false` on a Mac whose vault names Commons but which no Commons open has
/// reached yet (an aterm window, `aterm node serve` or the Commons runtime makes the custody when
/// it first opens Commons here); a device key alone is no custody. Reads names only and creates
/// nothing.
///
/// # Errors
/// One of those names could not be read.
pub fn custody_opened(paths: &Paths) -> Result<bool> {
    retains_control(paths)
}

fn is_transient_vault_revalidation(error: &Error) -> bool {
    matches!(
        error.kind(),
        crate::ErrorKind::VaultBusy | crate::ErrorKind::VaultChanged
    )
}

impl Bootstrap {
    /// Observe the authenticated vault cache offline without waiting on its initial writer. An unavailable vault or
    /// invisible control entry returns None without creating custody. A visible
    /// descriptor may establish local custody; it does not grant membership.
    pub fn cached(paths: &Paths) -> Result<Option<Self>> {
        Self::cached_started(paths, std::time::Instant::now())
    }

    fn cached_started(paths: &Paths, started: std::time::Instant) -> Result<Option<Self>> {
        match Self::observe_started(paths, started)? {
            Availability::Unavailable(_) => Ok(None),
            Availability::Available(bootstrap) => Ok(Some(*bootstrap)),
        }
    }

    /// [`Self::cached`] with one absolute deadline for all local custody and
    /// control-writer lock waits. Every successful observation still validates
    /// the authenticated vault, device custody, immutable pins and full history.
    pub fn cached_until(paths: &Paths, deadline: std::time::Instant) -> Result<Option<Self>> {
        match Self::observe_until(paths, deadline)? {
            Availability::Unavailable(_) => Ok(None),
            Availability::Available(bootstrap) => Ok(Some(*bootstrap)),
        }
    }

    /// [`Self::cached_until`], retaining the exclusively locked, freshly observed
    /// control journal from that same custody admission. Consumers may authorize
    /// one local batch without reopening or replaying the journal. Revalidate
    /// custody before authorization, and drop the journal before network I/O.
    /// This pair grants neither current membership nor application access.
    pub fn cached_with_journal_until(
        paths: &Paths,
        deadline: std::time::Instant,
    ) -> Result<Option<(Self, Journal)>> {
        loop {
            return match super::observe_until(paths, deadline)? {
                super::Observation::Unavailable(_) => Ok(None),
                super::Observation::Visible(snapshot) => {
                    match Self::open_snapshot_with_journal(
                        paths,
                        &snapshot,
                        false,
                        Some(deadline),
                        true,
                        false,
                        || {},
                    ) {
                        Ok(pair) => Ok(Some(pair)),
                        Err(error)
                            if std::time::Instant::now() < deadline
                                && is_transient_vault_revalidation(&error) =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            continue;
                        }
                        Err(error) => Err(error),
                    }
                }
            };
        }
    }

    /// Explain an unavailable vault without waiting on its initial writer. An available
    /// descriptor opens the existing custody or creates its first local pin;
    /// it never substitutes a new network for missing established history.
    pub fn observe(paths: &Paths) -> Result<Availability> {
        Self::observe_started(paths, std::time::Instant::now())
    }

    fn observe_started(paths: &Paths, started: std::time::Instant) -> Result<Availability> {
        match super::observe(paths)? {
            super::Observation::Unavailable(reason) => Ok(Availability::Unavailable(reason)),
            super::Observation::Visible(snapshot) => {
                let deadline = Self::observation_deadline(started)?;
                Self::open_snapshot(paths, &snapshot, false, Some(deadline))
                    .map(|bootstrap| Availability::Available(Box::new(bootstrap)))
            }
        }
    }

    /// [`Self::observe`], waiting for the initial vault observation, then local
    /// custody and control-writer contention until one shared `deadline`.
    /// Timeouts retain the vault-busy reason, `CustodyBusy` or `JournalBusy`.
    /// Revalidation under Commons custody never waits on a vault writer.
    pub fn observe_until(paths: &Paths, deadline: std::time::Instant) -> Result<Availability> {
        loop {
            return match super::observe_until(paths, deadline)? {
                super::Observation::Unavailable(reason) => Ok(Availability::Unavailable(reason)),
                super::Observation::Visible(snapshot) => {
                    match Self::open_snapshot(paths, &snapshot, false, Some(deadline)) {
                        Ok(bootstrap) => Ok(Availability::Available(Box::new(bootstrap))),
                        Err(error)
                            if std::time::Instant::now() < deadline
                                && is_transient_vault_revalidation(&error) =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            continue;
                        }
                        Err(error) => Err(error),
                    }
                }
            };
        }
    }

    /// Open Commons or report the specific unavailable vault boundary.
    pub fn required(paths: &Paths) -> Result<Self> {
        Self::required_started(paths, std::time::Instant::now())
    }

    fn required_started(paths: &Paths, started: std::time::Instant) -> Result<Self> {
        match Self::observe_started(paths, started)? {
            Availability::Available(bootstrap) => Ok(*bootstrap),
            Availability::Unavailable(reason) => Err(reason.failure("Commons unavailable")),
        }
    }

    fn observation_deadline(started: std::time::Instant) -> Result<std::time::Instant> {
        let deadline = started + REQUIRED_WAIT;
        if std::time::Instant::now() >= deadline {
            return Err(Error::new(
                "Commons required observation did not finish before its hang detector",
            ));
        }
        Ok(deadline)
    }

    /// [`Self::required`] with the shared local lock deadline of
    /// [`Self::observe_until`]. Unavailable vaults retain their specific reason.
    pub fn required_until(paths: &Paths, deadline: std::time::Instant) -> Result<Self> {
        match Self::observe_until(paths, deadline)? {
            Availability::Available(bootstrap) => Ok(*bootstrap),
            Availability::Unavailable(reason) => Err(reason.failure("Commons unavailable")),
        }
    }

    /// [`Self::required_until`], retaining the same fresh, exclusively locked
    /// journal as [`Self::cached_with_journal_until`]. Unavailable vault errors
    /// retain their specific reason. Revalidate custody before authorization,
    /// and drop the journal before network I/O.
    pub fn required_with_journal_until(
        paths: &Paths,
        deadline: std::time::Instant,
    ) -> Result<(Self, Journal)> {
        loop {
            return match super::observe_until(paths, deadline)? {
                super::Observation::Visible(snapshot) => {
                    match Self::open_snapshot_with_journal(
                        paths,
                        &snapshot,
                        false,
                        Some(deadline),
                        true,
                        false,
                        || {},
                    ) {
                        Ok(pair) => Ok(pair),
                        Err(error)
                            if std::time::Instant::now() < deadline
                                && is_transient_vault_revalidation(&error) =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            continue;
                        }
                        Err(error) => Err(error),
                    }
                }
                super::Observation::Unavailable(reason) => {
                    Err(reason.failure("Commons unavailable"))
                }
            };
        }
    }

    /// Observe only already-established custody and control history. This never creates a
    /// device, backfills a claim, updates a pin or ingests a descriptor. A missing setup
    /// boundary remains unavailable for the ordinary convergence path to handle.
    /// A transient source revalidation releases custody and reobserves the vault
    /// within the same deadline; it never waits on a vault writer while holding custody.
    ///
    /// # Errors
    /// This installation has opened no Commons custody yet ([`custody_opened`]), it kept its claim
    /// and lost its `commons` folder (recovery is required), or the authenticated vault, existing
    /// private custody, descriptor or current control history is unavailable, changed, incomplete
    /// or busy before the shared deadline.
    pub fn established_with_journal_until(
        paths: &Paths,
        deadline: std::time::Instant,
    ) -> Result<(Self, Journal)> {
        if !retains_control(paths)? {
            return Err(Error::new(
                "this Mac has opened no Commons custody yet: an aterm window, `aterm node serve` \
                 or the Commons runtime opens it",
            ));
        }
        if fsx::kind(&paths.dir.join("commons.claim"))? != fsx::Kind::Missing
            && fsx::kind(&paths.dir.join("commons"))? == fsx::Kind::Missing
        {
            return Err(Error::new(MISSING_COMMONS_DIRECTORY));
        }
        established_files(paths)?;
        loop {
            let snapshot = match super::observe_until(paths, deadline)? {
                super::Observation::Visible(snapshot) => snapshot,
                super::Observation::Unavailable(reason) => {
                    return Err(reason.failure("existing Commons vault observation is unavailable"));
                }
            };
            match Self::open_snapshot_with_journal(
                paths,
                &snapshot,
                true,
                Some(deadline),
                false,
                true,
                || {},
            ) {
                Ok(pair) => return Ok(pair),
                Err(error)
                    if std::time::Instant::now() < deadline
                        && is_transient_vault_revalidation(&error) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(error) => return Err(error),
            }
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "FirstBoot",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "Reopen",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "VaultHandoff",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "LostHistory",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "ChangedTrust",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "MissingOuterPin",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "MissingPinAndHistory",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "MissingCommonsDirectory",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "ChangedTrustMissingPin",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "CorruptHistoryClaim",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "LegacyBackfill",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "MissingOuterClaims",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "BusyBootstrap",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "PreparingResume",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "ForeignPreparing",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "LegacyDamagedAnchor",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "OrphanTables",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "WrongIdentity",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "UnclaimedPartial",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "PreparingPinLostLog",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbBootstrapHistory",
            action = "OrphanHistory",
            project = "aterm_secrets::commons::bootstrap::tests::history_transition"
        )
    )]
    #[cfg(test)]
    fn from_snapshot(paths: &Paths, snapshot: &Snapshot) -> Result<Self> {
        Self::open_snapshot(paths, snapshot, false, None)
    }

    pub(crate) fn from_creation_snapshot(paths: &Paths, snapshot: &Snapshot) -> Result<Self> {
        Self::open_snapshot(paths, snapshot, true, None)
    }

    fn open_snapshot(
        paths: &Paths,
        snapshot: &Snapshot,
        require_device: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        Self::open_snapshot_with(paths, snapshot, require_device, deadline, || {})
    }

    fn open_snapshot_with(
        paths: &Paths,
        snapshot: &Snapshot,
        require_device: bool,
        deadline: Option<std::time::Instant>,
        held: impl FnMut(),
    ) -> Result<Self> {
        Self::open_snapshot_with_journal(
            paths,
            snapshot,
            require_device,
            deadline,
            false,
            false,
            held,
        )
        .map(|(bootstrap, _journal)| bootstrap)
    }

    fn open_snapshot_with_journal(
        paths: &Paths,
        snapshot: &Snapshot,
        require_device: bool,
        deadline: Option<std::time::Instant>,
        retain_writer: bool,
        observe_only: bool,
        held: impl FnMut(),
    ) -> Result<(Self, Journal)> {
        let descriptor = Descriptor::decode(snapshot.control())?;
        let _lock = if observe_only {
            CustodyLock::observe_until_with(paths, "commons.lock", deadline, held)?
        } else {
            CustodyLock::acquire_until_with(paths, "commons.lock", false, deadline, held)?
        };
        if observe_only {
            established_files(paths)?;
        }
        if deadline.is_some() {
            Self::validate_snapshot(paths, snapshot)?;
        }
        let identity = client::read_identity(paths)?
            .ok_or_else(|| Error::new("Commons secrets identity is missing"))?;
        if identity.recipient() != snapshot.recipient() {
            return Err(Error::new(
                "Commons secrets identity changed since the vault observation",
            ));
        }
        let dir = paths.dir.join("commons");
        let pin = pin_bytes(descriptor.trust());
        let claim = claim_bytes(descriptor.trust());
        let retained = fsx::read_private(&paths.dir.join("commons.claim"), PIN_LEN)?;
        let preparing = retained
            .as_ref()
            .is_some_and(|kept| kept.as_slice() == preparing_bytes(descriptor.trust()));
        if retained
            .as_ref()
            .is_some_and(|kept| kept.as_slice() != claim)
            && !preparing
        {
            return Err(Error::new(
                "Commons default network differs from its retained history claim",
            ));
        }
        if retained.is_some() && fsx::kind(&dir)? == fsx::Kind::Missing {
            return Err(Error::new(MISSING_COMMONS_DIRECTORY));
        }
        if observe_only {
            aterm_uds::private_file::check_private_dir(&dir)?;
        } else {
            fsx::ensure_private_dir(&dir)?;
        }
        let established = fsx::read_private(&dir.join("default.pin"), PIN_LEN)?;
        if established
            .as_ref()
            .is_some_and(|kept| kept.as_slice() != pin)
        {
            return Err(Error::new(
                "Commons default network differs from its immutable pin",
            ));
        }
        if observe_only
            && (retained
                .as_ref()
                .is_none_or(|kept| kept.as_slice() != claim)
                || established
                    .as_ref()
                    .is_none_or(|kept| kept.as_slice() != pin))
        {
            return Err(Error::new("existing Commons pins or claims are incomplete"));
        }
        // Validate the journal's independent immutable pin before writing ours.
        // If our pin was lost, a replacement descriptor must not poison it before
        // the older journal has had the opportunity to refuse reanchoring.
        let journal_dir = dir.join("control");
        let history = has_history(paths)?;
        let must_exist = established.is_some()
            || (retained.is_some() && !preparing)
            || (history && !preparing)
            || has_application_history(paths)?;
        if must_exist && fsx::kind(&journal_dir)? != fsx::Kind::Dir {
            return Err(Error::new(
                "established Commons control directory is missing; recovery is required",
            ));
        }
        let device = Device::open_identity_checked(
            paths,
            identity,
            require_device,
            deadline,
            observe_only,
            || {
                if observe_only {
                    established_files(paths)?;
                }
                if deadline.is_some() {
                    Self::validate_snapshot(paths, snapshot)?;
                }
                Ok(())
            },
        )?;
        let device_custody = [
            paths.dir.join("identity"),
            dir.join("device.age"),
            dir.join("device.claim"),
        ]
        .into_iter()
        .map(|path| custody_digest(&path).map(|digest| (path, digest)))
        .collect::<Result<Vec<_>>>()?;
        if !must_exist && !preparing {
            // A crash during the first journal open can resume only with this
            // durable same-trust intent. No bootstrap accessor escapes until
            // the marker has advanced to the established phase below.
            let pending = preparing_bytes(descriptor.trust());
            fsx::write_new(&paths.dir.join("commons.claim"), &pending, 0o600)?;
            let kept = fsx::read_private(&paths.dir.join("commons.claim"), PIN_LEN)?
                .ok_or_else(|| Error::new("Commons initialization claim was not retained"))?;
            if kept.as_slice() != pending {
                return Err(Error::new("Commons initialization claim changed"));
            }
            aterm_durable::durable_dir(&paths.dir)?;
        }
        // A settled history that already retains the descriptor's nodes has
        // nothing to append: observe it without the journal's writer lock, so
        // a journal writer (peer repair's ingest, a revocation) never refuses
        // it. The custody lock above is still held.
        let settled = if !retain_writer
            && must_exist
            && !preparing
            && retained
                .as_ref()
                .is_some_and(|kept| kept.as_slice() == claim)
            && established
                .as_ref()
                .is_some_and(|kept| kept.as_slice() == pin)
        {
            holds_descriptor(&journal_dir, &descriptor)?
        } else {
            None
        };
        let mut journal = if let Some(journal) = settled {
            journal
        } else if observe_only {
            return Err(Error::new(
                "existing Commons control awaits normal descriptor ingestion",
            ));
        } else if must_exist {
            match deadline {
                Some(deadline) => {
                    Journal::open_existing_until(&journal_dir, descriptor.trust(), deadline)?
                }
                None => Journal::open_existing(&journal_dir, descriptor.trust())?,
            }
        } else {
            match deadline {
                Some(deadline) => Journal::open_until(&journal_dir, descriptor.trust(), deadline)?,
                None => Journal::open(&journal_dir, descriptor.trust())?,
            }
        };
        if deadline.is_some() {
            Self::validate_snapshot(paths, snapshot)?;
            for (path, expected) in &device_custody {
                if &custody_digest(path)? != expected {
                    return Err(Error::new(
                        "Commons device custody changed while acquiring control",
                    ));
                }
            }
        }
        // End the empty-journal initialization window before the first record
        // is ingested. Once any restriction can be acknowledged, both the log
        // and its anchor must survive even if no accessor has returned yet.
        if preparing || (!must_exist && retained.is_none()) {
            fsx::write_atomic(&paths.dir.join("commons.claim"), &claim, 0o600)?;
        } else if retained.is_none() {
            fsx::write_new(&paths.dir.join("commons.claim"), &claim, 0o600)?;
        }
        let kept = fsx::read_private(&paths.dir.join("commons.claim"), PIN_LEN)?
            .ok_or_else(|| Error::new("Commons history claim was not retained"))?;
        if kept.as_slice() != claim {
            return Err(Error::new("Commons history claim changed during creation"));
        }
        aterm_durable::durable_dir(&paths.dir)?;
        if journal.is_writer() {
            journal.ingest(descriptor.genesis.bytes())?;
            for node in &descriptor.repair {
                journal.ingest(node.bytes())?;
            }
        }
        if established.is_none() {
            fsx::write_new(&dir.join("default.pin"), &pin, 0o600)?;
        }
        let kept = fsx::read_private(&dir.join("default.pin"), PIN_LEN)?
            .ok_or_else(|| Error::new("Commons network pin was not retained"))?;
        if kept.as_slice() != pin {
            return Err(Error::new("Commons network pin changed during creation"));
        }
        aterm_durable::durable_dir(&dir)?;
        let immutable = [
            paths.dir.join("identity"),
            paths.dir.join("commons.claim"),
            dir.join("device.age"),
            dir.join("device.claim"),
            dir.join("default.pin"),
            journal_dir.join("control.anchor"),
        ]
        .into_iter()
        .map(|path| custody_digest(&path).map(|digest| (path, digest)))
        .collect::<Result<_>>()?;
        Ok((
            Self {
                descriptor,
                device,
                journal_dir,
                paths: paths.clone(),
                snapshot: snapshot.clone(),
                immutable,
            },
            journal,
        ))
    }

    fn validate_snapshot(paths: &Paths, snapshot: &Snapshot) -> Result<()> {
        // Never wait on a vault writer while holding Commons/control custody: source admission
        // is a nonblocking observation at each check, never a grant kept past a change to the
        // files it was observed from. A writer holding the vault meets a fresh read, which finds
        // it busy; with none, files whose stamps have not moved give the observation they gave
        // (`client::observe_commons_quiet`).
        Self::validate_observation(snapshot, client::observe_commons_quiet(paths)?)
    }

    /// Whether `observed`, the vault's observation now, is the one `snapshot` was taken under.
    fn validate_observation(snapshot: &Snapshot, observed: super::Observation) -> Result<()> {
        let current = match observed {
            super::Observation::Visible(current) => current,
            super::Observation::Unavailable(reason) => {
                return Err(
                    reason.failure("Commons vault became unavailable while acquiring custody")
                );
            }
        };
        if !snapshot.same_observation(&current) {
            return Err(Error::of_kind(
                crate::ErrorKind::VaultChanged,
                "Commons vault source or applied version changed while acquiring custody",
            ));
        }
        Ok(())
    }

    /// Reobserve this exact authenticated source/version/recipient and immutable
    /// local custody after acquiring a later lock. Call before using a bootstrap
    /// across a waiting boundary. The vault observation never waits on a writer;
    /// changed, missing, busy or no longer private state grants no authorization.
    pub fn revalidate_custody(&self) -> Result<()> {
        Self::validate_snapshot(&self.paths, &self.snapshot)?;
        self.revalidate_immutable_custody()
    }

    /// [`Self::revalidate_custody`], the vault observed through `look`, one look at this
    /// bootstrap's vault for checks that stand for one moment of it
    /// ([`client::VaultLook::observe_commons`]); a look at another vault is not taken.
    ///
    /// # Errors
    /// As [`Self::revalidate_custody`].
    pub fn revalidate_custody_in(&self, look: &client::VaultLook<'_>) -> Result<()> {
        if look.paths() != &self.paths {
            return self.revalidate_custody();
        }
        Self::validate_observation(&self.snapshot, look.observe_commons()?)?;
        self.revalidate_immutable_custody()
    }

    /// Whether an open of this bootstrap's vault ([`Self::cached_until`]) would give it back now,
    /// field for field, and neither refuse nor write: what lets a caller that keeps a bootstrap
    /// across its work (`aterm-db`'s table-repair `Replica`, one a round) take it in place of a
    /// fresh open, and open afresh, and keep that, only where this refuses.
    ///
    /// Beside its vault's paths, every field is a function of the vault observation and of the
    /// files it pins (the descriptor and trust of the observation's control bytes, the device and
    /// its lab mark of the identity and `device.age`), so [`Self::revalidate_custody`] judges them
    /// all: the observation's every field as the open took it ([`Snapshot::same_observation`]) and
    /// the digests of the six files an open pins (the identity, `commons.claim`, `device.age`,
    /// `device.claim`, `default.pin`, `control.anchor`). The rest is what an open would repair or
    /// refuse: the vault's folder and `commons/` this user's and exactly `0700`, as
    /// `fsx::ensure_private_dir` leaves a folder (an open makes any other mode `0700`, but for a
    /// vault folder its owner cannot search, whose observation it refuses first), each custody
    /// lock a private file in the vault's folder, judged with it (an open makes a missing one,
    /// narrows a folder whose mode opens it to others, and refuses a lock that is not private or a
    /// folder an access list opens to others), the vault's creation claim
    /// readable as an open's device custody reads it (`client::creation_requires_device`, which
    /// refuses a claim that is malformed or not private), and the control journal holding the
    /// descriptor's nodes, read as
    /// [`Journal::read_existing`] reads it (an open refuses a control folder that is missing or no
    /// folder, and would take the journal's writer and ingest a node it lacks; the read refuses a
    /// folder that is missing, linked or holds no journal). The vault is judged as every session
    /// judges it (`client::observe_commons_quiet`): files whose stamps have not moved give the
    /// observation they gave, and a writer holding the vault meets a fresh read, which finds it
    /// busy. This takes neither custody lock and makes no folder durable, as it writes nothing.
    ///
    /// # Errors
    /// An open would give another bootstrap, repair or refuse, or would wait for a writer: the
    /// vault is busy, changed or unavailable, a pinned file changed or is missing, a folder is not
    /// this user's or not `0700`, a custody lock is missing or not private, the creation claim
    /// cannot be read, or the control journal cannot be read or lacks a node of the descriptor.
    pub fn stands(&self) -> Result<()> {
        self.revalidate_custody()?;
        let state = aterm_uds::private_file::JudgedDir::private(&self.paths.dir)?;
        for name in ["commons.lock", "commons-device.lock"] {
            state.open(name).map_err(|error| {
                Error::new(format!("Commons custody lock is not private: {error}"))
            })?;
        }
        fsx::private_dir_settled(&self.paths.dir)?;
        fsx::private_dir_settled(&self.paths.dir.join("commons"))?;
        client::creation_requires_device(&self.paths)?;
        if descriptor_held(&self.journal_dir, &self.descriptor).is_none() {
            return Err(Error::new(
                "Commons control no longer holds its vault's descriptor",
            ));
        }
        Ok(())
    }

    /// Check the same immutable custody after a caller has independently
    /// revalidated and exclusively held the exact vault observation. This does
    /// not acquire another state observation or grant any authority by itself.
    pub(crate) fn revalidate_immutable_custody(&self) -> Result<()> {
        for (path, expected) in &self.immutable {
            if &custody_digest(path)? != expected {
                return Err(Error::new(
                    "Commons immutable custody changed while acquiring a lock",
                ));
            }
        }
        Ok(())
    }

    /// Hold this freshly revalidated source against vault writers for one local
    /// batch. Acquisition never waits while Commons or journal custody is held.
    /// The guard grants no membership or policy rights; drop it before network
    /// I/O or a vault mutation.
    ///
    /// # Errors
    /// The state lock is unavailable or unsafe, or the source, applied version
    /// or immutable custody no longer matches this bootstrap.
    pub fn local_source_read(&self) -> Result<crate::client::CommonsSourceRead> {
        let read = crate::client::hold_commons_source(&self.paths)?;
        self.revalidate_custody()?;
        Ok(read)
    }

    pub fn trust(&self) -> Trust {
        self.descriptor.trust()
    }
    pub const fn device(&self) -> &Device {
        &self.device
    }
    pub fn journal_dir(&self) -> &Path {
        &self.journal_dir
    }
    pub const fn paths(&self) -> &Paths {
        &self.paths
    }
    pub fn request(&self) -> Result<Request> {
        self.device
            .request(self.trust().network, self.trust().authority)
    }

    /// Whether Commons control on this Mac is governed by the membership log (`F18`).
    ///
    /// # Errors
    /// Local custody files cannot be read.
    pub fn is_log_backed(&self) -> Result<bool> {
        Ok(client::trusted_commons_log_now(&self.paths, self.trust())?.is_some())
    }

    /// Select the causal membership log proving this device's historical seat and any later
    /// restored node or audience access. Keep the verified log typed: local custody may exceed
    /// a legacy wire decoder's record limit, and each transport enforces its own wire bounds.
    ///
    /// # Errors
    /// Local custody or device key cannot be read, or the selected evidence cannot form a log.
    pub fn log_witness(&self) -> Result<Option<Arc<crate::membership_log::Log>>> {
        type WitnessCacheEntry = ([u8; 32], Vec<u8>, Arc<crate::membership_log::Log>);
        static WITNESS_CACHE: std::sync::Mutex<std::collections::VecDeque<WitnessCacheEntry>> =
            std::sync::Mutex::new(std::collections::VecDeque::new());

        let Some((log, heads_digest)) = client::trusted_commons_log_now(&self.paths, self.trust())?
        else {
            return Ok(None);
        };
        let request = self.request()?;
        let binding_bytes = request.binding().bytes().to_vec();
        if let Ok(cache) = WITNESS_CACHE.lock()
            && let Some((_, _, witness)) = cache
                .iter()
                .find(|(d, b, _)| *d == heads_digest && *b == binding_bytes)
        {
            return Ok(Some(witness.clone()));
        }
        let binding = request.binding();
        let Some(entry_id) = log.historical_seat_entry(binding) else {
            return Ok(None);
        };
        let Some(mut chain) = log.ancestor_chain(&entry_id) else {
            return Ok(None);
        };
        if chain.is_empty() {
            return Ok(None);
        }
        let full_membership = log.membership();
        let actor_removed_in_full = full_membership.removal(&binding.actor()).is_some();
        let recipient_removed_in_full = log.recipient_is_removed(&binding.recipient());
        let check_chain = |records: &[crate::membership_log::Record]| -> (bool, bool) {
            let mut iter = records.iter().cloned();
            let Some(mut sub) = iter
                .next()
                .and_then(|g| crate::membership_log::Log::new(g).ok())
            else {
                return (false, false);
            };
            for rec in iter {
                if sub.insert(rec).is_err() {
                    return (false, false);
                }
            }
            let sub_membership = sub.membership();
            let removal_ok =
                actor_removed_in_full || sub_membership.removal(&binding.actor()).is_none();
            let recipient_ok =
                recipient_removed_in_full || !sub.recipient_is_removed(&binding.recipient());
            let node_ok = !matches!(
                sub_membership.node(binding.writer()),
                crate::membership_log::NodeStanding::Removed(_)
            ) && !sub_membership.kin_removed(binding);
            let seat_ok = sub.has_historical_seat(binding) && removal_ok && recipient_ok && node_ok;
            let audience_ok = !sub.audience_left_out(
                &crate::membership_log::EpochAudience::Commons,
                &binding.actor(),
            ) && !sub.audience_left_out(
                &crate::membership_log::EpochAudience::Everyone,
                &binding.actor(),
            ) && !sub.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Commons,
                &binding.recipient(),
            ) && !sub.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &binding.recipient(),
            );
            (seat_ok, audience_ok)
        };
        let push_seat_targets = |targets: &mut Vec<crate::membership_log::RecordId>| {
            for rec in log.records().skip(1) {
                match rec.body() {
                    crate::membership_log::Body::Remove { .. } => {
                        targets.push(*rec.id());
                    }
                    crate::membership_log::Body::Admit { entrant, .. }
                        if entrant.request.binding() == binding
                            || entrant.request.binding().recipient() == binding.recipient() =>
                    {
                        targets.push(*rec.id());
                    }
                    crate::membership_log::Body::Node { .. }
                        if *rec.actor() == binding.actor()
                            || full_membership
                                .get(rec.actor())
                                .is_some_and(|m| m.binding.recipient() == binding.recipient()) =>
                    {
                        targets.push(*rec.id());
                    }
                    _ => {}
                }
            }
        };
        let (seat_ok, audience_ok) = check_chain(&chain);
        if !seat_ok || !audience_ok {
            let mut targets = vec![entry_id];
            if !seat_ok {
                push_seat_targets(&mut targets);
            }
            for rec in log.records().skip(1) {
                if let crate::membership_log::Body::Wrap { recipient, .. } = rec.body()
                    && *recipient == binding.recipient()
                {
                    targets.push(*rec.id());
                }
            }
            chain = log.ancestor_closure(&targets);
            if seat_ok && !check_chain(&chain).0 {
                push_seat_targets(&mut targets);
                chain = log.ancestor_closure(&targets);
            }
        }
        let mut records = chain.into_iter();
        let genesis = records
            .next()
            .ok_or_else(|| Error::new("Commons log witness has no genesis"))?;
        let mut witness = crate::membership_log::Log::new(genesis)?;
        for record in records {
            witness.insert(record)?;
        }
        let witness = Arc::new(witness);
        if let Ok(mut cache) = WITNESS_CACHE.lock() {
            cache.retain(|(d, b, _)| !(*d == heads_digest && *b == binding_bytes));
            if cache.len() >= 16 {
                cache.pop_front();
            }
            cache.push_back((heads_digest, binding_bytes, witness.clone()));
        }
        Ok(Some(witness))
    }

    /// Find historical authority approval for sealed control repair only. This
    /// may name a retired device: current data access is a separate roster check.
    pub fn witness(&self) -> Result<Option<Node>> {
        let journal = Journal::read_existing(&self.journal_dir, self.trust())?;
        self.witness_in(&journal)
    }

    /// [`Self::witness`] with bounded admission to the existing journal's lock.
    /// The deadline is shared with the caller's other local setup work.
    pub fn witness_until(&self, deadline: std::time::Instant) -> Result<Option<Node>> {
        let journal = Journal::open_existing_until(&self.journal_dir, self.trust(), deadline)?;
        self.revalidate_custody()?;
        self.witness_in(&journal)
    }

    /// Find the same historical approval within an already locked control
    /// observation. The journal must belong to this bootstrap's trust context.
    /// This grants no current application access.
    pub fn witness_in(&self, journal: &Journal) -> Result<Option<Node>> {
        if journal.trust() != self.trust() {
            return Err(Error::new("Commons witness has another trust context"));
        }
        let request = self.request()?;
        Ok(journal
            .inventory()
            .filter_map(|id| journal.node(id))
            .find(|node| {
                node.facts().iter().any(
                    |fact| matches!(fact, Fact::Bind { binding } if binding == request.binding()),
                )
            })
            .cloned())
    }

    /// Validate the current roster and the exact local writer binding. Holding
    /// the age recipient alone cannot activate a different enrolled writer.
    /// The returned handle still rechecks its journal before exposing any key.
    pub fn access(&self) -> Result<Option<super::roster::Access>> {
        client::with_roster_observation(&self.paths, |observation| {
            let journal = Journal::read_existing(&self.journal_dir, self.trust())?;
            self.access_in_observed(&journal, observation.state_lock())
        })
        .map(Option::flatten)
    }

    pub(crate) fn access_held(
        &self,
        lock: &client::StateLock,
    ) -> Result<Option<super::roster::Access>> {
        let journal = Journal::read_existing(&self.journal_dir, self.trust())?;
        self.access_in_held(&journal, lock)
    }

    /// Validate against an already open journal, a writer or a read-only view,
    /// so a short local batch shares one consistent control observation.
    pub fn access_in(&self, journal: &Journal) -> Result<Option<super::roster::Access>> {
        client::with_roster_observation(&self.paths, |observation| {
            self.access_in_observed(journal, observation.state_lock())
        })
        .map(Option::flatten)
    }

    /// [`Self::access_in`] for a caller that may observe again: a vault writer that holds the
    /// state lock at this moment is [`crate::ErrorKind::VaultBusy`], not "no access".
    ///
    /// # Errors
    /// As [`Self::access_in`], or a vault writer holds the state lock
    /// ([`crate::ErrorKind::VaultBusy`]).
    pub fn access_observed_in(&self, journal: &Journal) -> Result<Option<super::roster::Access>> {
        client::with_roster_observation_or_busy(&self.paths, |observation| {
            self.access_in_observed(journal, observation.state_lock())
        })
        .map(Option::flatten)
    }

    /// [`Self::access_observed_in`] through `look`, one look at this bootstrap's vault for checks
    /// that stand for one moment of it: judged under the look's shared observation of the state
    /// lock and its look at each file, where it looks at this vault and observes that lock;
    /// otherwise [`Self::access_observed_in`]'s own.
    ///
    /// # Errors
    /// As [`Self::access_observed_in`].
    pub fn access_observed_in_look(
        &self,
        journal: &Journal,
        look: &client::VaultLook<'_>,
    ) -> Result<Option<super::roster::Access>> {
        match look.held() {
            Some((lock, glance)) if look.paths() == &self.paths => {
                self.access_in_observed_with(journal, Some(lock), glance)
            }
            _ => self.access_observed_in(journal),
        }
    }

    pub(crate) fn access_in_held(
        &self,
        journal: &Journal,
        lock: &client::StateLock,
    ) -> Result<Option<super::roster::Access>> {
        self.access_in_observed(journal, Some(lock))
    }

    fn access_in_observed(
        &self,
        journal: &Journal,
        lock: Option<&client::StateLock>,
    ) -> Result<Option<super::roster::Access>> {
        self.access_in_observed_with(journal, lock, &fsx::Glance::new())
    }

    /// [`Self::access_in_observed`], each file looked at through `glance`.
    fn access_in_observed_with(
        &self,
        journal: &Journal,
        lock: Option<&client::StateLock>,
        glance: &fsx::Glance,
    ) -> Result<Option<super::roster::Access>> {
        if journal.trust() != self.trust() {
            return Err(Error::new("Commons access has another trust context"));
        }
        // This Mac's own denial, kept while none of the files it was judged from moved. A read
        // fault on one of them that has not moved gives the judgement those files gave, so this
        // Mac keeps the access it held, as a server keeps the role it holds while its device
        // cannot be read; the swarm's checks of this Mac
        // (`client::local_recipient_denied_in_roster`), but for its LAN presence, a hint kept as
        // this is (`client::swarm_member_heard_now`), read afresh and give such a Mac nothing
        // new. Both memos take the caller's own shared observation for theirs
        // (`client::trusted_commons_log_under`), and the access looks at the vault's files once
        // for both and for the state its log stands on, through `glance` (`fsx::Glance`).
        if let Some(lock) = lock
            && client::local_recipient_denied_under(&self.paths, lock, glance)?
        {
            return Ok(None);
        }
        let trusted = match lock {
            Some(lock) => {
                client::trusted_commons_log_under(&self.paths, self.trust(), lock, glance)?
            }
            None => client::trusted_commons_log_now(&self.paths, self.trust())?,
        };
        let maybe_access = if let Some((log, heads_digest)) = trusted {
            let lock =
                lock.ok_or_else(|| Error::new("Commons membership log has no vault observation"))?;
            let me = self.device.identity.recipient();
            let req = self.request()?;
            if log.wraps_to(&me)
                || log.removes(&me)
                || log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Everyone,
                    &me,
                )
                || log.audience_left_out_recipient(
                    &crate::membership_log::EpochAudience::Commons,
                    &me,
                )
                || log.audience_left_out(
                    &crate::membership_log::EpochAudience::Everyone,
                    &req.binding().actor(),
                )
                || log.audience_left_out(
                    &crate::membership_log::EpochAudience::Commons,
                    &req.binding().actor(),
                )
                || log.membership().removal(&req.binding().actor()).is_some()
                || log.has_historical_seat(req.binding())
                || log.has_known_actor(&req.binding().actor())
                || matches!(
                    log.genesis().body(),
                    crate::membership_log::Body::Genesis { founders, .. }
                        if founders.iter().any(|f| {
                            f.recipient == me || f.binding.as_ref() == Some(req.binding())
                        })
                )
                || log.records().skip(1).any(|r| {
                    matches!(
                        r.body(),
                        crate::membership_log::Body::Admit { entrant, .. }
                            if entrant.request.binding() == req.binding()
                                || entrant.request.binding().recipient() == me
                    ) || matches!(
                        r.body(),
                        crate::membership_log::Body::Wrap { recipient, .. } if *recipient == me
                    )
                })
            {
                super::roster::current_from_loaded_log(
                    &self.paths,
                    self.trust(),
                    &self.device.identity,
                    &log,
                    heads_digest,
                    lock,
                    glance,
                )?
            } else {
                super::roster::current(journal, &self.device.identity)?
            }
        } else {
            super::roster::current(journal, &self.device.identity)?
        };
        let Some(access) = maybe_access else {
            return Ok(None);
        };
        if access.local().binding() != self.request()?.binding() {
            return Err(Error::new("Commons roster names another local writer"));
        }
        Ok(Some(access))
    }

    /// Read an earlier roster through this device's encrypted custody. The
    /// observation must still authorize this exact local writer and recipient.
    pub fn historical(
        &self,
        journal: &Journal,
        access: &super::roster::Access,
        basis: [u8; 32],
    ) -> Result<super::roster::Historical> {
        self.historical_in(journal, access, basis, None)
    }

    /// [`Bootstrap::historical`], judged through `look` where given
    /// ([`Bootstrap::historicals_in`]): a table-repair read batch's record history.
    pub fn historical_in(
        &self,
        journal: &Journal,
        access: &super::roster::Access,
        basis: [u8; 32],
        look: Option<&crate::client::VaultLook<'_>>,
    ) -> Result<super::roster::Historical> {
        self.historicals_in(journal, access, &[basis], look)
            .pop()
            .expect("one answer for each basis")
    }

    /// [`Bootstrap::historical`] for each of `bases`, in order. A log-backed observation checks
    /// its membership, reads its log and leases its history once for all of them
    /// ([`super::roster::Access::with_log_history`]), and a basis the log does not open falls back
    /// on its own: to the control journal, which checks the membership again, and to the handoff
    /// it was given, which opens under the same history.
    pub fn historicals(
        &self,
        journal: &Journal,
        access: &super::roster::Access,
        bases: &[[u8; 32]],
    ) -> Vec<Result<super::roster::Historical>> {
        self.historicals_in(journal, access, bases, None)
    }

    /// [`Bootstrap::historicals`], its membership checks and its read of the log judged through
    /// `look` where given ([`super::roster::Access::with_log_history_in`], and the control
    /// journal's fallback through [`super::roster::Access::historical_in`]): a table-repair read
    /// batch's record history, through the look its session keeps (`aterm-db`'s
    /// `Session::decode_envelope`). A handoff a basis falls back to opens as it does for
    /// [`Bootstrap::historicals`].
    pub fn historicals_in(
        &self,
        journal: &Journal,
        access: &super::roster::Access,
        bases: &[[u8; 32]],
        look: Option<&crate::client::VaultLook<'_>>,
    ) -> Vec<Result<super::roster::Historical>> {
        self.each_historical(
            journal,
            access,
            bases,
            look,
            |history, basis| history.historical(basis),
            |past| past,
        )
    }

    /// Whether this device holds each of `bases`, in order: whether [`Bootstrap::historicals`]
    /// answers it with a key, asked without one. A log-era epoch this device opened under the same
    /// held records before is answered without its key unwrapped again, and a key unwrapped to
    /// answer is neither kept nor given (`roster::Access::opens_from_lease`); a basis the log does
    /// not open falls back as [`Bootstrap::historicals`] does. A repair round asks this of each
    /// epoch of its history, sixteen a round (`aterm-db`'s `Session::history_needs`).
    pub fn holds_histories(
        &self,
        journal: &Journal,
        access: &super::roster::Access,
        bases: &[[u8; 32]],
    ) -> Vec<bool> {
        self.holds_histories_in(journal, access, bases, None)
    }

    /// [`Bootstrap::holds_histories`], judged through `look` where given, as
    /// [`Bootstrap::historicals_in`] judges: a table-repair read batch's probe of the history it
    /// needs (`aterm-db`'s `Session::history_needs`).
    pub fn holds_histories_in(
        &self,
        journal: &Journal,
        access: &super::roster::Access,
        bases: &[[u8; 32]],
        look: Option<&crate::client::VaultLook<'_>>,
    ) -> Vec<bool> {
        self.each_historical(
            journal,
            access,
            bases,
            look,
            |history, basis| Ok(history.opens(basis)?.then_some(())),
            |_| (),
        )
        .iter()
        .map(Result::is_ok)
        .collect()
    }

    /// [`Bootstrap::historicals_in`] answering each of `bases` as `from_log` reads it of the log's
    /// history, and as `held` reads a [`super::roster::Historical`] the control journal or a
    /// handoff opens where the log does not.
    fn each_historical<T>(
        &self,
        journal: &Journal,
        access: &super::roster::Access,
        bases: &[[u8; 32]],
        look: Option<&crate::client::VaultLook<'_>>,
        from_log: impl Fn(&super::roster::LogHistory<'_>, [u8; 32]) -> Result<Option<T>>,
        held: impl Fn(super::roster::Historical) -> T,
    ) -> Vec<Result<T>> {
        let each =
            |error: Error| -> Vec<Result<T>> { bases.iter().map(|_| Err(error.clone())).collect() };
        if bases.is_empty() {
            return Vec::new();
        }
        if journal.trust() != self.trust() {
            return each(Error::new(
                "historical roster has another local trust binding",
            ));
        }
        match self.request() {
            Ok(request) if access.local().binding() == request.binding() => {}
            Ok(_) => {
                return each(Error::new(
                    "historical roster has another local trust binding",
                ));
            }
            Err(error) => return each(error),
        }
        let identity = &self.device.identity;
        access.with_log_history_in(journal, identity, look, |history| {
            let history = history.as_ref().map(Option::as_ref);
            bases
                .iter()
                .map(|&basis| {
                    let from_log = match &history {
                        Ok(Some(history)) => from_log(history, basis),
                        Ok(None) => Ok(None),
                        Err(error) => Err((*error).clone()),
                    };
                    let native = match from_log {
                        Ok(Some(past)) => Ok(past),
                        Ok(None) => access
                            .historical_in(journal, identity, basis, look)
                            .map(&held),
                        Err(error) => Err(error),
                    };
                    // A log-era Commons epoch this device holds no `Wrap` of (one from before it
                    // joined) opens through the handoff it was given, as an older control node
                    // does.
                    native.or_else(|error| match self.cached_handoff(basis)? {
                        Some(handoff) => match &history {
                            Ok(Some(history)) => {
                                handoff.open_in(journal, access, identity, history)
                            }
                            _ => handoff.open(journal, access, identity),
                        }
                        .map(&held),
                        None => Err(error),
                    })
                })
                .collect()
        })
    }

    /// Prepare encrypted historical access for one exact current member. The
    /// caller transmits only these sealed bytes, after releasing the journal.
    /// A member that learned history through a handoff can pass it on too.
    pub fn history_handoff(
        &self,
        journal: &Journal,
        access: &super::roster::Access,
        basis: [u8; 32],
        recipient: &super::binding::Binding,
    ) -> Result<super::history::Handoff> {
        self.check_history_local(journal, access)?;
        let issue = |inherited| {
            self.device.with_seed(|seed| {
                super::history::Handoff::issue(
                    journal,
                    access,
                    &self.device.identity,
                    seed,
                    basis,
                    recipient,
                    inherited,
                )
            })
        };
        match issue(None) {
            Ok(handoff) => Ok(handoff),
            Err(error) => match self.cached_handoff(basis)? {
                Some(inherited) => issue(Some(&inherited)),
                None => Err(error),
            },
        }
    }

    /// Authenticate a handoff completely before caching its encrypted bytes.
    /// One entry per control source or membership-log Commons epoch bounds storage by the sum
    /// of those independently bounded domains.
    /// A failed durability barrier is uncertain; retrying revalidates the entry.
    pub fn import_history(
        &self,
        journal: &Journal,
        access: &super::roster::Access,
        wire: &[u8],
    ) -> Result<super::history::Handoff> {
        self.check_history_local(journal, access)?;
        let handoff = super::history::Handoff::decode(wire)?;
        handoff.open(journal, access, &self.device.identity)?;
        // Native custody already retains these keys. Avoid spending a handoff
        // slot or doing a durable write for redundant transport.
        if access
            .historical_from_log(journal, &self.device.identity, *handoff.source())
            .is_ok_and(|past| past.is_some())
            || access
                .historical(journal, &self.device.identity, *handoff.source())
                .is_ok()
        {
            return Ok(handoff);
        }
        let dir = self.history_dir();
        aterm_journal::create_private_dir_durable(&dir)
            .map_err(|e| Error::new(format!("history cache directory: {e}")))?;
        let name = history_name(handoff.source());
        if let Some(saved) = fsx::read_private(&dir.join(&name), super::history::MAX_HANDOFF)?
            && let Ok(saved) = super::history::Handoff::decode(saved.as_slice())
            && saved.source() == handoff.source()
            && saved.open(journal, access, &self.device.identity).is_ok()
            && (saved.issued_under() == handoff.issued_under()
                || journal.selected_ancestor(*saved.issued_under(), *handoff.issued_under()))
        {
            // Keep the earliest usable issuance: an ancestor survives every
            // selected-path recovery that preserves its descendant. Finish a
            // possibly uncertain previous directory barrier before acknowledging.
            aterm_durable::durable_dir(&dir)
                .map_err(|e| Error::new(format!("history cache publication: {e}")))?;
            return Ok(saved);
        }
        history_capacity(
            &dir,
            &name,
            super::journal::MAX_NODES,
            crate::membership_log::MAX_HELD_RECORDS.min(crate::membership_log::MAX_KEYED_RECORDS),
        )?;
        aterm_journal::write_private_durable(&dir, &name, wire)
            .map_err(|e| Error::new(format!("history cache publication: {e}")))?;
        Ok(handoff)
    }

    fn check_history_local(&self, journal: &Journal, access: &super::roster::Access) -> Result<()> {
        if journal.trust() != self.trust()
            || !access.is_current(journal)
            || access.local().binding() != self.request()?.binding()
        {
            return Err(Error::new(
                "history handoff has another local trust binding",
            ));
        }
        Ok(())
    }

    /// This device's record of Commons contacts ([`super::hearing::Ledger`]).
    pub fn heard_file(&self) -> PathBuf {
        self.journal_dir.parent().unwrap().join("heard")
    }

    fn history_dir(&self) -> PathBuf {
        self.journal_dir.parent().unwrap().join("history")
    }

    fn cached_handoff(&self, basis: [u8; 32]) -> Result<Option<super::history::Handoff>> {
        let dir = self.history_dir();
        match aterm_uds::private_file::check_private_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::new(format!("history cache directory: {e}"))),
        }
        fsx::read_private(&dir.join(history_name(&basis)), super::history::MAX_HANDOFF)?
            .map(|wire| {
                let handoff = super::history::Handoff::decode(wire.as_slice())?;
                if handoff.source() != &basis {
                    return Err(Error::new("cached history has another source"));
                }
                Ok(handoff)
            })
            .transpose()
    }
}

// Preserve bounded crash leftovers without charging them as usable key slots.
// Unknown files get the same small headroom; none are silently deleted.
fn history_capacity(
    dir: &Path,
    target: &str,
    control_sources: usize,
    log_sources: usize,
) -> Result<()> {
    let limit = control_sources
        .checked_add(log_sources)
        .ok_or_else(|| Error::new("history cache source bound overflow"))?;
    let mut found = false;
    let mut records = 0;
    let mut auxiliary = 0;
    for entry in
        std::fs::read_dir(dir).map_err(|e| Error::new(format!("history cache inventory: {e}")))?
    {
        let entry = entry.map_err(|e| Error::new(format!("history cache entry: {e}")))?;
        let name = entry.file_name();
        if name
            .to_str()
            .and_then(|name| name.strip_suffix(".handoff"))
            .is_some_and(|stem| {
                stem.len() == 64
                    && stem
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            found |= name == target;
            records += 1;
        } else {
            auxiliary += 1;
        }
        if records > limit || auxiliary > 32 {
            return Err(Error::new(
                "history cache exceeds its record or auxiliary bound",
            ));
        }
    }
    if !found && records == limit {
        return Err(Error::new("history cache is full"));
    }
    Ok(())
}

fn history_name(basis: &[u8; 32]) -> String {
    format!("{}.handoff", aterm_codec::hex::encode_lower(basis))
}

/// The journal in `journal_dir` when it already holds every node of
/// `descriptor`, read without its writer. The view may show a checkpoint whose
/// rename is not yet durable (a writer between its rename and its directory
/// barrier, or one whose barrier failed), so a journal that holds them is made
/// durable, as a writer's open would, before this returns it.
fn holds_descriptor(journal_dir: &Path, descriptor: &Descriptor) -> Result<Option<Journal>> {
    let holds = descriptor_held(journal_dir, descriptor);
    if holds.is_some() {
        aterm_durable::durable_dir(journal_dir)?;
    }
    Ok(holds)
}

/// [`holds_descriptor`]'s read with no flush: the journal in `journal_dir` when it holds every
/// node of `descriptor`, read without its writer, `None` where it lacks one or cannot be read.
fn descriptor_held(journal_dir: &Path, descriptor: &Descriptor) -> Option<Journal> {
    Journal::read_existing(journal_dir, descriptor.trust())
        .ok()
        .filter(|journal| {
            std::iter::once(&descriptor.genesis)
                .chain(&descriptor.repair)
                .all(|node| journal.node(&node.id()).is_some())
        })
}

fn pin_bytes(trust: Trust) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(PIN_LEN);
    bytes.extend_from_slice(b"ACP1");
    bytes.extend_from_slice(&trust.network);
    bytes.extend_from_slice(&trust.authority);
    bytes.extend_from_slice(&trust.genesis);
    bytes
}

fn claim_bytes(trust: Trust) -> Vec<u8> {
    let mut bytes = pin_bytes(trust);
    bytes[..4].copy_from_slice(b"ACH1");
    bytes
}

fn preparing_bytes(trust: Trust) -> Vec<u8> {
    let mut bytes = pin_bytes(trust);
    bytes[..4].copy_from_slice(b"ACI1");
    bytes
}

fn has_application_history(paths: &Paths) -> Result<bool> {
    let dir = paths.dir.join("commons");
    Ok(fsx::kind(&dir.join("history"))? != fsx::Kind::Missing
        || fsx::kind(&dir.join("tables"))? != fsx::Kind::Missing)
}

fn has_history(paths: &Paths) -> Result<bool> {
    Ok(
        fsx::kind(&paths.dir.join("commons/control"))? != fsx::Kind::Missing
            || has_application_history(paths)?,
    )
}

/// Whether this installation keeps Commons control state for its default network: a claim, a
/// pin, or control or application history, of any authority, or a `commons` directory that is
/// a symlink (what it held cannot be told from here). A device key alone is not control state.
/// Reads names only; creates and repairs nothing.
/// # Errors
/// One of those names could not be read.
pub(crate) fn retains_control(paths: &Paths) -> Result<bool> {
    Ok(
        fsx::kind(&paths.dir.join("commons.claim"))? != fsx::Kind::Missing
            || fsx::kind(&paths.dir.join("commons"))? == fsx::Kind::Symlink
            || fsx::kind(&paths.dir.join("commons/default.pin"))? != fsx::Kind::Missing
            || has_history(paths)?,
    )
}

#[cfg(test)]
pub(crate) mod tests;
