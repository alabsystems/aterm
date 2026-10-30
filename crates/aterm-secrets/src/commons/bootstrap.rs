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

fn custody_digest(path: &Path) -> Result<[u8; 32]> {
    let bytes = fsx::read_private(path, MAX_DEVICE)?
        .ok_or_else(|| Error::new("Commons immutable custody is missing"))?;
    Ok(aterm_digest::Sha256::digest(bytes.as_slice()))
}

/// How long an observation waits for another local holder of `commons.lock`.
/// Holders do no network I/O and finish in milliseconds, so one that outlasts
/// this is stuck or one of many back to back: the observation is refused as busy.
const CUSTODY_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

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
        mut held: impl FnMut(),
    ) -> Result<Self> {
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
        let mut observed_held = || {
            #[cfg(test)]
            tests::custody_held();
            held();
        };
        let result = if let Some(deadline) = deadline {
            aterm_uds::private_file::lock_exclusive_until_with(
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
            aterm_uds::private_file::lock_exclusive_until_with(
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
        Self::open_identity_until(paths, identity, require_existing, None)
    }

    fn open_identity_until(
        paths: &Paths,
        identity: Identity,
        require_existing: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        Self::open_identity_checked(paths, identity, require_existing, deadline, || Ok(()))
    }

    fn open_identity_checked(
        paths: &Paths,
        identity: Identity,
        require_existing: bool,
        deadline: Option<std::time::Instant>,
        validate: impl FnOnce() -> Result<()>,
    ) -> Result<Self> {
        // Device creation and loss checks share one lock even when invoked
        // without a network bootstrap. No caller holding this lock takes the
        // bootstrap lock, so bootstrap -> device is the sole lock order.
        let _lock = CustodyLock::acquire_until(paths, "commons-device.lock", true, deadline)?;
        validate()?;
        if client::read_identity(paths)?
            .is_none_or(|current| current.recipient() != identity.recipient())
        {
            return Err(Error::new(
                "Commons secrets identity changed while acquiring device custody",
            ));
        }
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

fn is_transient_vault_revalidation(error: &Error) -> bool {
    let text = error.to_string();
    text.contains("Commons vault became unavailable while acquiring custody (vault-busy)")
        || text.contains("Commons vault source or applied version changed while acquiring custody")
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
        match Self::observe(paths)? {
            Availability::Available(bootstrap) => Ok(*bootstrap),
            Availability::Unavailable(reason) => Err(Error::new(format!(
                "Commons unavailable ({}): {}",
                reason.code(),
                reason.explanation()
            ))),
        }
    }

    /// [`Self::required`] with the shared local lock deadline of
    /// [`Self::observe_until`]. Unavailable vaults retain their specific reason.
    pub fn required_until(paths: &Paths, deadline: std::time::Instant) -> Result<Self> {
        match Self::observe_until(paths, deadline)? {
            Availability::Available(bootstrap) => Ok(*bootstrap),
            Availability::Unavailable(reason) => Err(Error::new(format!(
                "Commons unavailable ({}): {}",
                reason.code(),
                reason.explanation()
            ))),
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
                super::Observation::Unavailable(reason) => Err(Error::new(format!(
                    "Commons unavailable ({}): {}",
                    reason.code(),
                    reason.explanation()
                ))),
            };
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
        Self::open_snapshot_with_journal(paths, snapshot, require_device, deadline, false, held)
            .map(|(bootstrap, _journal)| bootstrap)
    }

    fn open_snapshot_with_journal(
        paths: &Paths,
        snapshot: &Snapshot,
        require_device: bool,
        deadline: Option<std::time::Instant>,
        retain_writer: bool,
        held: impl FnMut(),
    ) -> Result<(Self, Journal)> {
        let descriptor = Descriptor::decode(snapshot.control())?;
        let _lock = CustodyLock::acquire_until_with(paths, "commons.lock", false, deadline, held)?;
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
        let device =
            Device::open_identity_checked(paths, identity, require_device, deadline, || {
                if deadline.is_some() {
                    Self::validate_snapshot(paths, snapshot)?;
                }
                Ok(())
            })?;
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
        // Never wait on a vault writer while holding Commons/control custody:
        // source admission is a nonblocking observation, not a cached grant.
        let current = match super::observe(paths)? {
            super::Observation::Visible(current) => current,
            super::Observation::Unavailable(reason) => {
                return Err(Error::new(format!(
                    "Commons vault became unavailable while acquiring custody ({}): {}",
                    reason.code(),
                    reason.explanation()
                )));
            }
        };
        if !snapshot.same_observation(&current) {
            return Err(Error::new(
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
        for (path, expected) in &self.immutable {
            if &custody_digest(path)? != expected {
                return Err(Error::new(
                    "Commons immutable custody changed while acquiring a lock",
                ));
            }
        }
        Ok(())
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
        Ok(client::trusted_commons_log(&self.paths, self.trust())?.is_some())
    }

    /// Encode the minimal causal membership-log chain proving this device's historical seat for
    /// `ACX2` peer authentication (`F18`).
    ///
    /// # Errors
    /// Local custody or device key cannot be read.
    pub fn log_witness(&self) -> Result<Option<Vec<u8>>> {
        type WitnessCacheEntry = ([u8; 32], Vec<u8>, Vec<u8>);
        static WITNESS_CACHE: std::sync::Mutex<std::collections::VecDeque<WitnessCacheEntry>> =
            std::sync::Mutex::new(std::collections::VecDeque::new());

        let Some((log, heads_digest)) = client::trusted_commons_log(&self.paths, self.trust())?
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
            let seat_ok = sub.has_historical_seat(binding);
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
        let (seat_ok, audience_ok) = check_chain(&chain);
        if !seat_ok || !audience_ok {
            let mut targets = vec![entry_id];
            for rec in log.records().skip(1) {
                match rec.body() {
                    crate::membership_log::Body::Remove { .. } if !seat_ok => {
                        targets.push(*rec.id());
                    }
                    crate::membership_log::Body::Wrap { recipient, .. }
                        if *recipient == binding.recipient() =>
                    {
                        targets.push(*rec.id());
                    }
                    _ => {}
                }
            }
            chain = log.ancestor_closure(&targets);
            if seat_ok && !check_chain(&chain).0 {
                for rec in log.records().skip(1) {
                    if matches!(rec.body(), crate::membership_log::Body::Remove { .. }) {
                        targets.push(*rec.id());
                    }
                }
                chain = log.ancestor_closure(&targets);
            }
        }
        let encoded = crate::membership_log::encode_records(&chain);
        if let Ok(mut cache) = WITNESS_CACHE.lock() {
            cache.retain(|(d, b, _)| !(*d == heads_digest && *b == binding_bytes));
            if cache.len() >= 16 {
                cache.pop_front();
            }
            cache.push_back((heads_digest, binding_bytes, encoded.clone()));
        }
        Ok(Some(encoded))
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
        let journal = Journal::read_existing(&self.journal_dir, self.trust())?;
        self.access_in(&journal)
    }

    /// Validate against an already open journal, a writer or a read-only view,
    /// so a short local batch shares one consistent control observation.
    pub fn access_in(&self, journal: &Journal) -> Result<Option<super::roster::Access>> {
        if journal.trust() != self.trust() {
            return Err(Error::new("Commons access has another trust context"));
        }
        if client::local_recipient_denied_in_roster(&self.paths) {
            return Ok(None);
        }
        let maybe_access = if let Some((log, heads_digest)) =
            client::trusted_commons_log(&self.paths, self.trust())?
        {
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
                || log.membership().removal(&req.binding().actor()).is_some()
                || log.has_historical_seat(req.binding())
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
        if journal.trust() != self.trust() || access.local().binding() != self.request()?.binding()
        {
            return Err(Error::new(
                "historical roster has another local trust binding",
            ));
        }
        if access.is_log_backed()
            && let Some(past) = access.historical_from_log(journal, &self.device.identity, basis)?
        {
            return Ok(past);
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
            .historical_from_log(journal, &self.device.identity, *handoff.source())?
            .is_some()
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

/// The journal in `journal_dir` when it already holds every node of
/// `descriptor`, read without its writer. The view may show a checkpoint whose
/// rename is not yet durable (a writer between its rename and its directory
/// barrier, or one whose barrier failed), so a journal that holds them is made
/// durable, as a writer's open would, before this returns it.
fn holds_descriptor(journal_dir: &Path, descriptor: &Descriptor) -> Result<Option<Journal>> {
    let holds = Journal::read_existing(journal_dir, descriptor.trust())
        .ok()
        .filter(|journal| {
            std::iter::once(&descriptor.genesis)
                .chain(&descriptor.repair)
                .all(|node| journal.node(&node.id()).is_some())
        });
    if holds.is_some() {
        aterm_durable::durable_dir(journal_dir)?;
    }
    Ok(holds)
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
mod tests;
