// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Offline bootstrap from an authenticated vault observation. Device secrets
//! are age-encrypted; immutable public claims detect key loss and substitution.
//! Preparing a device or a journal grants no membership or application access.

use super::{
    Snapshot,
    binding::Request,
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

const DESCRIPTOR_HEADER: usize = 4 + 16 + 32 + 1;
const MAX_DESCRIPTOR: usize = DESCRIPTOR_HEADER + 3 * (4 + MAX_NODE);
const DEVICE_LEN: usize = 4 + 32 + 16 + 16 + 32;
const CLAIM_LEN: usize = 4 + 32 + 16 + 16 + 32;
const PIN_LEN: usize = 4 + 16 + 32 + 32;
const MAX_DEVICE: usize = 4096;

/// Serializes custody observations independently of the removable control log.
/// The lock is never replaced, and never held during network I/O.
struct CustodyLock(std::fs::File);

impl CustodyLock {
    fn acquire_commons_custody(paths: &Paths, name: &str, wait: bool) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        fsx::ensure_private_dir(&paths.dir)?;
        let path = paths.dir.join(name);
        // This empty inode is the lock, not a replaceable data record. A
        // hard-link publication briefly has two names and is correctly refused
        // by concurrent private-file readers. Publish it with one name from
        // the first instant; never truncate or replace an existing lock.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
        {
            Ok(file) => {
                aterm_durable::durable(&file)?;
                aterm_durable::durable_dir(&paths.dir)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let (file, metadata) = aterm_uds::private_file::open_private(&paths.dir, name)
            .map_err(|error| Error::new(format!("Commons custody lock is not private: {error}")))?;
        let custody_file: std::fs::File = file;
        aterm_uds::keep_out_of_children(&custody_file)?;
        if wait {
            #[expect(
                clippy::disallowed_methods,
                reason = "CustodyLock is CLOEXEC/CLOFORK and unlocks before close; device creation has no network IO"
            )]
            custody_file
                .lock()
                .map_err(|_| Error::new("cannot lock Commons device custody"))?;
        } else {
            #[expect(
                clippy::disallowed_methods,
                reason = "CustodyLock is CLOEXEC/CLOFORK and explicitly unlocks before close"
            )]
            custody_file
                .try_lock()
                .map_err(|_| Error::new("another local Commons bootstrap is active"))?;
        }
        let lock = Self(custody_file);
        if fsx::file_id(&path) != Some((metadata.dev(), metadata.ino())) {
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

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
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
}

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

    pub(crate) fn open_existing(paths: &Paths) -> Result<Self> {
        let identity = client::read_identity(paths)?
            .ok_or_else(|| Error::new("Commons secrets identity is missing"))?;
        Self::open_identity(paths, identity, true)
    }

    fn open_identity(paths: &Paths, identity: Identity, require_existing: bool) -> Result<Self> {
        // Device creation and loss checks share one lock even when invoked
        // without a network bootstrap. No caller holding this lock takes the
        // bootstrap lock, so bootstrap -> device is the sole lock order.
        let _lock = CustodyLock::acquire_commons_custody(paths, "commons-device.lock", true)?;
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
        fsx::ensure_private_dir(&dir)?;
        let mut encrypted = fsx::read_private(&dir.join("device.age"), MAX_DEVICE)?;
        let claim = fsx::read_private(&dir.join("device.claim"), CLAIM_LEN)?;
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
        let device = match encrypted {
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
    pub fn request(&self, network: [u8; 16], authority: [u8; 32]) -> Result<Request> {
        self.with_seed(|seed| {
            Request::create(
                network,
                authority,
                self.machine,
                self.incarnation,
                &self.identity,
                seed,
            )
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
}

/// One authenticated vault observation followed by Commons custody validation.
/// Neither an unavailable result nor an error grants initialization authority.
pub enum Availability {
    Unavailable(super::Unavailable),
    Available(Box<Bootstrap>),
}

impl Bootstrap {
    /// Observe the authenticated vault cache offline. An unavailable vault or
    /// invisible control entry returns None without creating custody. A visible
    /// descriptor may establish local custody; it does not grant membership.
    pub fn cached(paths: &Paths) -> Result<Option<Self>> {
        match Self::observe(paths)? {
            Availability::Unavailable(_) => Ok(None),
            Availability::Available(bootstrap) => Ok(Some(*bootstrap)),
        }
    }

    /// Explain an unavailable vault without a second observation. An available
    /// descriptor opens the existing custody or creates its first local pin;
    /// it never substitutes a new network for missing established history.
    pub fn observe(paths: &Paths) -> Result<Availability> {
        match super::observe(paths)? {
            super::Observation::Unavailable(reason) => Ok(Availability::Unavailable(reason)),
            super::Observation::Visible(snapshot) => Self::from_snapshot(paths, &snapshot)
                .map(|bootstrap| Availability::Available(Box::new(bootstrap))),
        }
    }

    /// Open Commons or report the specific unavailable vault boundary.
    pub fn required(paths: &Paths) -> Result<Self> {
        match Self::observe(paths)? {
            Availability::Available(bootstrap) => Ok(*bootstrap),
            Availability::Unavailable(reason) => Err(Error::new(format!(
                "Commons unavailable ({}): {}",
                reason.code(),
                reason.explanation()
            ))),
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
    fn from_snapshot(paths: &Paths, snapshot: &Snapshot) -> Result<Self> {
        Self::open_snapshot(paths, snapshot, false)
    }

    pub(crate) fn from_creation_snapshot(paths: &Paths, snapshot: &Snapshot) -> Result<Self> {
        Self::open_snapshot(paths, snapshot, true)
    }

    fn open_snapshot(paths: &Paths, snapshot: &Snapshot, require_device: bool) -> Result<Self> {
        let descriptor = Descriptor::decode(snapshot.control())?;
        let _lock = CustodyLock::acquire_commons_custody(paths, "commons.lock", false)?;
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
            return Err(Error::new(
                "established Commons directory is missing; recovery is required",
            ));
        }
        fsx::ensure_private_dir(&dir)?;
        let established = fsx::read_private(&dir.join("default.pin"), PIN_LEN)?;
        if established
            .as_ref()
            .is_some_and(|kept| kept.as_slice() != pin)
        {
            return Err(Error::new(
                "Commons default network differs from its immutable pin",
            ));
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
        let device = Device::open_identity(paths, identity, require_device)?;
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
        let mut journal = if must_exist {
            Journal::open_existing(&journal_dir, descriptor.trust())?
        } else {
            Journal::open(&journal_dir, descriptor.trust())?
        };
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
        journal.ingest(descriptor.genesis.bytes())?;
        for node in &descriptor.repair {
            journal.ingest(node.bytes())?;
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
        Ok(Self {
            descriptor,
            device,
            journal_dir,
        })
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
    pub fn request(&self) -> Result<Request> {
        self.device
            .request(self.trust().network, self.trust().authority)
    }

    /// Find historical authority approval for sealed control repair only. This
    /// may name a retired device: current data access is a separate roster check.
    pub fn witness(&self) -> Result<Option<Node>> {
        let request = self.request()?;
        let journal = Journal::open_existing(&self.journal_dir, self.trust())?;
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
        let journal = Journal::open_existing(&self.journal_dir, self.trust())?;
        self.access_in(&journal)
    }

    /// Validate against an already locked journal, allowing a short local batch
    /// to share one consistent control observation without reopening its lock.
    pub fn access_in(&self, journal: &Journal) -> Result<Option<super::roster::Access>> {
        if journal.trust() != self.trust() {
            return Err(Error::new("Commons access has another trust context"));
        }
        let Some(access) = super::roster::current(journal, &self.device.identity)? else {
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
        if journal.trust() != self.trust() || access.local().binding() != self.request()?.binding()
        {
            return Err(Error::new(
                "historical roster has another local trust binding",
            ));
        }
        match access.historical(journal, &self.device.identity, basis) {
            Ok(past) => Ok(past),
            Err(error) => match self.cached_handoff(basis)? {
                Some(handoff) => handoff.open(journal, access, &self.device.identity),
                None => Err(error),
            },
        }
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
    /// One entry per source roster bounds storage by the control journal cap.
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
            && journal.selected_ancestor(*saved.issued_under(), *handoff.issued_under())
        {
            // Keep the earliest usable issuance: an ancestor survives every
            // selected-path recovery that preserves its descendant. Finish a
            // possibly uncertain previous directory barrier before acknowledging.
            aterm_durable::durable_dir(&dir)
                .map_err(|e| Error::new(format!("history cache publication: {e}")))?;
            return Ok(saved);
        }
        history_capacity(&dir, &name, super::journal::MAX_NODES)?;
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
fn history_capacity(dir: &Path, target: &str, limit: usize) -> Result<()> {
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

#[cfg(test)]
mod tests;
