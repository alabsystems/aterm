// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One durable creation intent per local vault slot. Only retained, signed bytes
//! cross the transport boundary. The outer claim survives loss of the astream
//! directory; it never turns missing acknowledged history into a new identity.

use super::*;
use astream_wire::Offset;
use aterm_journal::{Limits, Log, create_private_dir_durable, write_private_durable};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

const CLAIM: &str = "creation.claim";
const PREPARED: &str = "vault.creation.prepared";
const COMPLETED: &str = "vault.creation.completed";
const MAX_PACKET: usize = envelope::MAX_ENVELOPE + 32 * 1024;
const MAX_INTENT: usize = 4 + 4 * (4 + 4096);

struct Lease(std::fs::File);
impl Lease {
    fn take(paths: &Paths) -> crate::Result<Self> {
        fsx::ensure_private_dir(&paths.dir)?;
        let path = paths.dir.join("creation.lock");
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
        let (file, metadata) =
            aterm_uds::private_file::open_private(&paths.dir, "creation.lock")
                .map_err(|e| Error::new(format!("creation lease is not private: {e}")))?;
        aterm_uds::keep_out_of_children(&file)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "creation lease is CLOEXEC/CLOFORK, explicitly unlocks, and never blocks readers"
        )]
        file.try_lock()
            .map_err(|_| Error::new("another local vault creation is active"))?;
        let lease = Self(file);
        if fsx::file_id(&path) != Some((metadata.dev(), metadata.ino())) {
            return Err(Error::new("creation lease changed while acquiring it"));
        }
        Ok(lease)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

struct Intent {
    source: Source,
    host: String,
    import: String,
    recipient: String,
}
impl Intent {
    fn bytes(&self) -> crate::Result<Vec<u8>> {
        let mut bytes = b"ACI1".to_vec();
        let source = match &self.source {
            Source::Url(url) => url.as_str(),
            Source::Path(path) => path
                .to_str()
                .ok_or_else(|| Error::new("creation source is not UTF-8"))?,
        };
        for field in [source, &self.host, &self.import, &self.recipient] {
            if field.len() > 4096 {
                return Err(Error::new("creation context exceeds its size limit"));
            }
            bytes.extend_from_slice(&(field.len() as u32).to_le_bytes());
            bytes.extend_from_slice(field.as_bytes());
        }
        Ok(bytes)
    }
    fn decode(bytes: &[u8], paths: &Paths) -> crate::Result<Self> {
        if !bytes.starts_with(b"ACI1") {
            return Err(Error::new("unknown vault creation intent"));
        }
        let mut tail = &bytes[4..];
        let mut field = || -> crate::Result<String> {
            let size = tail
                .get(..4)
                .map(|n| u32::from_le_bytes(n.try_into().unwrap()) as usize)
                .ok_or_else(|| Error::new("truncated creation context"))?;
            tail = &tail[4..];
            if size > 4096 || size > tail.len() {
                return Err(Error::new("invalid creation context size"));
            }
            let value = std::str::from_utf8(&tail[..size])
                .map_err(|_| Error::new("creation context is not UTF-8"))?
                .to_string();
            tail = &tail[size..];
            Ok(value)
        };
        let source_text = field()?;
        let host = field()?;
        let import = field()?;
        let recipient = field()?;
        if !tail.is_empty() || !text::valid_machine_name(&host) {
            return Err(Error::new("invalid creation context"));
        }
        let (source, pin) = transport::parse_source(&source_text, &paths.home)?;
        if pin.is_some() {
            return Err(Error::new("creation context contains a foreign pin"));
        }
        let intent = Self {
            source,
            host,
            import,
            recipient,
        };
        if intent.bytes()? != bytes {
            return Err(Error::new("noncanonical creation context"));
        }
        Ok(intent)
    }
}

struct Pending {
    intent: Intent,
    packet: Vec<u8>,
    envelope: Vec<u8>,
    complete: bool,
}
impl Pending {
    fn config(&self) -> crate::Result<Config> {
        let env = envelope::parse(&self.envelope)?;
        Ok(Config {
            source: self.intent.source.clone(),
            name: env.name,
            signer: Some(env.signer),
            followed: Vec::new(),
            host: self.intent.host.clone(),
        })
    }
    fn admin(&self, paths: &Paths) -> crate::Result<admin_keys::AdminText> {
        let env = envelope::parse(&self.envelope)?;
        let identity = identity(paths, &self.intent)?;
        let locker = env
            .admin_locker()
            .ok_or_else(|| Error::new("creation envelope has no admin locker"))?;
        let admin = parse_admin(
            &age::decrypt(std::slice::from_ref(&identity), locker.armored.as_bytes())
                .map_err(|e| Error::new(format!("creation admin locker: {e}")))?,
        )?;
        admin.admissions.validate(&admin.doc, env.seq)?;
        if envelope::signer_public(&admin.seed)? != env.signer || !admin.commons_bound {
            return Err(Error::new(
                "creation envelope has invalid authority custody",
            ));
        }
        Ok(admin)
    }
    fn receipt(&self, paths: &Paths, local: Option<String>) -> crate::Result<Created> {
        let config = self.config()?;
        let env = envelope::parse(&self.envelope)?;
        let admin = self.admin(paths)?;
        let mut imported = admin
            .doc
            .entries()
            .into_iter()
            .filter(|entry| entry.audience == Audience::Everyone)
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        imported.sort();
        Ok(Created {
            name: config.name,
            signer: crate::b64::encode_padded(&env.signer),
            fingerprint: envelope::fingerprint(&env.signer),
            imported,
            local,
        })
    }
}

fn identity(paths: &Paths, intent: &Intent) -> crate::Result<Identity> {
    read_identity(paths)?
        .filter(|id| id.recipient().to_bech32() == intent.recipient)
        .ok_or_else(|| Error::new("creation identity is missing or changed; recovery is required"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Preparing,
    Ready,
    Complete,
    /// The first publication was refused before anything was stored ([`retire`]). The slot is
    /// free: every reader takes this claim for none, and the next creation removes it and the
    /// outbox it leaves.
    Retired,
    /// Ready, and a first publication was sent whose outcome is not known (its reply lost, the
    /// process ended, an error that does not say nothing was stored): it may have landed, so a
    /// later refusal, which may come from another server answering the same name or from a
    /// volume that is gone, never retires the intent.
    Sent,
}
struct Claim {
    context: [u8; 32],
    digest: [u8; 32],
    intent: Intent,
    phase: Phase,
}
fn claim(paths: &Paths) -> crate::Result<Option<Claim>> {
    let Some(bytes) = fsx::read_private(&paths.dir.join(CLAIM), 101 + MAX_INTENT)? else {
        return Ok(None);
    };
    if bytes.as_slice().len() <= 101 || &bytes.as_slice()[..4] != b"ACC2" {
        return Err(Error::new("invalid vault creation claim"));
    }
    let (bytes, checksum) = bytes.as_slice().split_at(bytes.as_slice().len() - 32);
    if aterm_digest::Sha256::digest(bytes).as_slice() != checksum {
        return Err(Error::new("vault creation claim checksum failed"));
    }
    let context = bytes[4..36].try_into().unwrap();
    let phase = match bytes[68] {
        0 => Phase::Preparing,
        1 => Phase::Ready,
        2 => Phase::Complete,
        3 => Phase::Retired,
        4 => Phase::Sent,
        _ => return Err(Error::new("invalid creation claim phase")),
    };
    let intent = Intent::decode(&bytes[69..], paths)?;
    if aterm_digest::Sha256::digest(&intent.bytes()?) != context {
        return Err(Error::new("creation claim has a mismatched context"));
    }
    Ok(Some(Claim {
        context,
        digest: bytes[36..68].try_into().unwrap(),
        intent,
        phase,
    }))
}
fn save_claim(paths: &Paths, intent: &Intent, digest: [u8; 32], phase: Phase) -> crate::Result<()> {
    let intent = intent.bytes()?;
    let mut bytes = b"ACC2".to_vec();
    bytes.extend_from_slice(&aterm_digest::Sha256::digest(&intent));
    bytes.extend_from_slice(&digest);
    bytes.push(match phase {
        Phase::Preparing => 0,
        Phase::Ready => 1,
        Phase::Complete => 2,
        Phase::Retired => 3,
        Phase::Sent => 4,
    });
    bytes.extend_from_slice(&intent);
    bytes.extend_from_slice(&aterm_digest::Sha256::digest(&bytes));
    write_private_durable(&paths.dir, CLAIM, &bytes)?;
    Ok(())
}
/// The claim that holds this slot: none when a refused intent was retired.
fn live(paths: &Paths) -> crate::Result<Option<Claim>> {
    Ok(claim(paths)?.filter(|claim| claim.phase != Phase::Retired))
}

/// Gives up the intent `context` names after its first publication was refused
/// ([`crate::ErrorKind::Refused`]): nothing was stored, so its version one, whose admin locker
/// is the only copy of its keys, can go. The machine identity and device stay. The claim turns
/// [`Phase::Retired`] in one durable write, under StateLock with no configuration or installed
/// version, before anything is removed, so a crash part-way leaves a free slot, never a Ready
/// claim without its outbox (which refuses, as a lost journal must). Called under the lease.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "RefusedFirst",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
fn retire(paths: &Paths, context: [u8; 32]) -> crate::Result<()> {
    {
        let _lock = StateLock::take(paths)?;
        let held = claim(paths)?
            .filter(|claim| {
                claim.context == context && matches!(claim.phase, Phase::Ready | Phase::Sent)
            })
            .ok_or_else(|| Error::new("the refused creation's claim changed"))?;
        if config(paths)?.is_some() || load_state(paths)?.seq > 0 {
            return Err(Error::new(
                "the refused creation's slot was configured meanwhile",
            ));
        }
        save_claim(paths, &held.intent, held.digest, Phase::Retired)?;
    }
    clear_retired(paths)
}

/// Removes a retired intent: its outbox, then its claim, so a crash between them leaves the
/// retired claim to remove again.
fn clear_retired(paths: &Paths) -> crate::Result<()> {
    let outbox = paths.dir.join("creation-outbox");
    match std::fs::remove_dir_all(&outbox) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(Error::new(format!("{}: {error}", outbox.display()))),
    }
    aterm_durable::durable_dir(&paths.dir)?;
    match std::fs::remove_file(paths.dir.join(CLAIM)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    aterm_durable::durable_dir(&paths.dir)?;
    Ok(())
}

fn open(paths: &Paths, context: [u8; 32], ready: bool) -> crate::Result<Log> {
    let dir = paths.dir.join("creation-outbox");
    if !ready {
        create_private_dir_durable(&dir)?;
    }
    Ok(Log::open(
        &dir,
        "creation",
        &context,
        ready,
        Limits {
            records: 2,
            bytes: MAX_PACKET + 4096,
            record_bytes: MAX_PACKET,
        },
    )?)
}
fn replay(paths: &Paths, log: &Log, context: [u8; 32]) -> crate::Result<Option<Pending>> {
    let records = log.read_range(Offset::ZERO, 3)?;
    let Some(first) = records.first() else {
        return Ok(None);
    };
    if first.subject != PREPARED || !first.body.starts_with(b"ACP1") {
        return Err(Error::new("invalid creation preparation record"));
    }
    let size = first
        .body
        .get(4..8)
        .map(|n| u32::from_le_bytes(n.try_into().unwrap()) as usize)
        .ok_or_else(|| Error::new("truncated creation preparation"))?;
    let end = 8usize
        .checked_add(size)
        .filter(|end| *end < first.body.len())
        .ok_or_else(|| Error::new("invalid creation preparation size"))?;
    let intent = Intent::decode(&first.body[8..end], paths)?;
    if aterm_digest::Sha256::digest(&first.body[8..end]) != context {
        return Err(Error::new("creation preparation belongs to another intent"));
    }
    let envelope = first.body[end..].to_vec();
    let env = envelope::parse(&envelope)?;
    env.verify_signature()?;
    if env.seq != 1 || env.name != transport::vault_name(&intent.source)? {
        return Err(Error::new(
            "creation preparation is not the initial envelope",
        ));
    }
    let digest = aterm_digest::Sha256::digest(&first.body);
    let complete = match records.as_slice() {
        [_] => false,
        [_, last] if last.subject == COMPLETED && last.body == digest => true,
        _ => return Err(Error::new("invalid creation completion record")),
    };
    let pending = Pending {
        intent,
        packet: first.body.clone(),
        envelope,
        complete,
    };
    pending.receipt(paths, None)?;
    Ok(Some(pending))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "ChangedImport",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "LostJournal",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "ChangedIntent",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "CompletedRetry",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "NewerLocal",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "ChangedConfig",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
pub(super) fn create(
    paths: &Paths,
    source: &str,
    import: Option<&Path>,
    host: Option<&str>,
    approval: Approval,
) -> crate::Result<Created> {
    let _lease = Lease::take(paths)?;
    let (context, pending) = prepare(paths, source, import, host, approval)?;
    finish(paths, context, pending)
}

/// A retried creation finishes the version its first try prepared, so `approval` shapes only a
/// version prepared now.
fn prepare(
    paths: &Paths,
    source: &str,
    import: Option<&Path>,
    host: Option<&str>,
    approval: Approval,
) -> crate::Result<([u8; 32], Pending)> {
    let (source, pin) = transport::parse_source(source, &paths.home)?;
    if pin.is_some() {
        return Err(Error::new("a new vault cannot use an existing signer pin"));
    }
    let mut held = claim(paths)?;
    if held
        .as_ref()
        .is_some_and(|claim| claim.phase == Phase::Retired)
    {
        clear_retired(paths)?;
        held = None;
    }
    let host = host.map(str::to_string).unwrap_or_else(|| {
        held.as_ref()
            .map(|claim| claim.intent.host.clone())
            .unwrap_or_else(default_host_name)
    });
    if !text::valid_machine_name(&host) {
        return Err(Error::new("invalid creation machine name"));
    }
    let recipient = if held.is_some() {
        read_identity(paths)?
            .ok_or_else(|| Error::new("creation identity is missing; recovery is required"))?
    } else {
        let _lock = StateLock::take(paths)?;
        empty_slot(paths)?;
        machine_identity(paths)?
    };
    let import_path = import
        .map(|path| {
            if path.is_absolute() {
                Ok(path.to_path_buf())
            } else {
                std::env::current_dir().map(|cwd| cwd.join(path))
            }
        })
        .transpose()?;
    let intent = Intent {
        source,
        host,
        import: import_path
            .as_deref()
            .map(|p| {
                p.to_str()
                    .ok_or_else(|| Error::new("import path is not UTF-8"))
            })
            .transpose()?
            .unwrap_or_default()
            .to_string(),
        recipient: recipient.recipient().to_bech32(),
    };
    let context = aterm_digest::Sha256::digest(&intent.bytes()?);
    let held = match held {
        Some(old) if old.context == context => Some(old),
        Some(_) => {
            return Err(Error::new(
                "another vault creation intent occupies this slot",
            ));
        }
        None => None,
    };
    if let Some(held) = &held {
        let mut log = open(paths, context, held.phase != Phase::Preparing)?;
        let pending = replay(paths, &log, context)?;
        if held.phase != Phase::Preparing
            && pending
                .as_ref()
                .map(|p| aterm_digest::Sha256::digest(&p.packet))
                != Some(held.digest)
        {
            return Err(Error::new(
                "acknowledged creation bytes are missing or changed",
            ));
        }
        if held.phase == Phase::Complete && !pending.as_ref().is_some_and(|p| p.complete) {
            return Err(Error::new("creation completion record is missing"));
        }
        log.finish_open()?;
        if let Some(pending) = pending {
            save_claim(
                paths,
                &pending.intent,
                aterm_digest::Sha256::digest(&pending.packet),
                if pending.complete {
                    Phase::Complete
                } else if held.phase == Phase::Sent {
                    Phase::Sent
                } else {
                    Phase::Ready
                },
            )?;
            return Ok((context, pending));
        }
    }
    // Validate the source and the complete imported document before reserving
    // a new slot. A typo must not strand a durable creation intent.
    ensure_absent(&intent.source)?;
    {
        let _lock = StateLock::take(paths)?;
        empty_slot(paths)?;
    }
    let name = transport::vault_name(&intent.source)?;
    let (envelope, _, _) = prepare_initial(
        paths,
        &name,
        import_path.as_deref(),
        &intent.host,
        true,
        approval,
    )?;
    {
        let _lock = StateLock::take(paths)?;
        empty_slot(paths)?;
        save_claim(paths, &intent, [0; 32], Phase::Preparing)?;
    }
    let intent_bytes = intent.bytes()?;
    let mut packet = b"ACP1".to_vec();
    packet.extend_from_slice(&(intent_bytes.len() as u32).to_le_bytes());
    packet.extend_from_slice(&intent_bytes);
    packet.extend_from_slice(&envelope);
    let mut log = open(paths, context, false)?;
    if replay(paths, &log, context)?.is_some() {
        return Err(Error::new(
            "creation preparation appeared while holding its lease",
        ));
    }
    log.finish_open()?;
    log.append(PREPARED, packet)?;
    let pending = replay(paths, &log, context)?
        .ok_or_else(|| Error::new("creation preparation was not retained"))?;
    save_claim(
        paths,
        &pending.intent,
        aterm_digest::Sha256::digest(&pending.packet),
        Phase::Ready,
    )?;
    drop(log);
    Ok((context, pending))
}

fn empty_slot(paths: &Paths) -> crate::Result<()> {
    if config(paths)?.is_some() || load_state(paths)?.seq > 0 {
        return Err(Error::new(
            "this vault slot is already configured; use a separate slot",
        ));
    }
    for path in [
        paths.cache_file(),
        paths.dir.join("commons.claim"),
        paths.dir.join("commons/default.pin"),
        paths.dir.join("commons/control"),
        paths.dir.join("commons/tables"),
    ] {
        if fsx::kind(&path)? != fsx::Kind::Missing {
            return Err(Error::new(
                "this vault slot retains Commons or vault history; recovery is required",
            ));
        }
    }
    Ok(())
}

fn ensure_absent(source: &Source) -> crate::Result<()> {
    match source {
        Source::Path(path) => {
            // Its first write, and every later one, refuses all but a folder (fsx::write_atomic);
            // the folder is read first, since a file's name as one fails the path's own read.
            if let Some(folder) = path
                .parent()
                .filter(|folder| !folder.as_os_str().is_empty())
            {
                let why = match fsx::kind(folder)? {
                    fsx::Kind::Dir => None,
                    fsx::Kind::Missing => Some("no such folder"),
                    fsx::Kind::Symlink => {
                        Some("a symlink, not a folder; name the folder it points to")
                    }
                    _ => Some("not a folder"),
                };
                if let Some(why) = why {
                    return Err(Error::new(format!(
                        "{}: {why}; nothing was created",
                        folder.display()
                    )));
                }
            }
            if fsx::kind(path)? == fsx::Kind::Missing {
                Ok(())
            } else {
                Err(Error::new(
                    "the creation source already exists; join it instead",
                ))
            }
        }
        Source::Url(_)
            if matches!(
                transport::fetch(source, None, Duration::from_secs(30))?,
                Fetched::Missing
            ) =>
        {
            Ok(())
        }
        _ => Err(Error::new(
            "the creation source already exists; join it instead",
        )),
    }
}

/// Called by the subscriber before its normal configured-source lookup. Only an
/// independently claimed preparation can resume; absence is never admission.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "Resume",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "BeforeReady",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "LostDevice",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
pub(super) fn resume(paths: &Paths) -> crate::Result<()> {
    let Some(claim) = live(paths)? else {
        return Ok(());
    };
    if claim.phase == Phase::Complete {
        return Ok(());
    }
    let _lease = Lease::take(paths)?;
    let Claim {
        context,
        digest,
        intent,
        phase,
    } = claim;
    if phase == Phase::Preparing {
        // The claim retains the original request. Before Ready there has been
        // no publication; a complete prepared record still takes precedence.
        let source = match &intent.source {
            Source::Url(url) => url.as_str(),
            Source::Path(path) => path
                .to_str()
                .ok_or_else(|| Error::new("creation source is not UTF-8"))?,
        };
        let import = (!intent.import.is_empty()).then(|| Path::new(&intent.import));
        // The intent does not record how the vault admits: production creation always opens it.
        let (context, pending) =
            prepare(paths, source, import, Some(&intent.host), Approval::Open)?;
        finish(paths, context, pending)?;
        return Ok(());
    }
    let mut log = open(paths, context, true)?;
    let pending = replay(paths, &log, context)?
        .filter(|p| aterm_digest::Sha256::digest(&p.packet) == digest)
        .ok_or_else(|| Error::new("acknowledged creation bytes are missing or changed"))?;
    log.finish_open()?;
    drop(log);
    if !pending.complete {
        finish(paths, context, pending)?;
    } else {
        save_claim(paths, &pending.intent, digest, Phase::Complete)?;
    }
    Ok(())
}

pub(super) fn pending_remote(paths: &Paths) -> crate::Result<bool> {
    let Some(claim) = live(paths)? else {
        return Ok(false);
    };
    if claim.phase == Phase::Complete || !matches!(claim.intent.source, Source::Url(_)) {
        return Ok(false);
    }
    if claim.phase == Phase::Preparing {
        return Ok(true);
    }
    let log = open(paths, claim.context, true)?;
    let pending = replay(paths, &log, claim.context)?
        .filter(|p| aterm_digest::Sha256::digest(&p.packet) == claim.digest)
        .ok_or_else(|| Error::new("acknowledged creation bytes are missing or changed"))?;
    Ok(!pending.complete)
}

/// Called under StateLock before a join can change this slot. Completed
/// creation is historical; only unfinished intent reserves the current slot.
pub(super) fn permit_join(paths: &Paths) -> crate::Result<()> {
    if live(paths)?.is_some_and(|claim| claim.phase != Phase::Complete) {
        return Err(Error::new(
            "an unfinished vault creation reserves this slot; resume it before joining another vault",
        ));
    }
    Ok(())
}

pub(super) fn requires_device(paths: &Paths) -> crate::Result<bool> {
    Ok(live(paths)?.is_some_and(|claim| claim.phase != Phase::Preparing))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbCreationOutbox",
        action = "RefusedAfterSent",
        project = "aterm_secrets::client::creation::tests::creation_transition"
    )
)]
fn finish(paths: &Paths, context: [u8; 32], pending: Pending) -> crate::Result<Created> {
    if pending.complete {
        return pending.receipt(paths, None);
    }
    // A prepared roster has already named this device. Its loss is not
    // permission to generate a new writer, even before live bootstrap pins.
    crate::commons::bootstrap::Device::open_existing(paths)?;
    let initial = pending.config()?;
    // Readers and ordinary vault edits remain unblocked during HTTP. Check the
    // configured lineage again after it, before applying any fetched bytes.
    let (current, state, cached) = {
        let _lock = StateLock::take(paths)?;
        let current = config(paths)?;
        if let Some(current) = &current {
            compatible(current, &initial)?;
        }
        let state = load_state(paths)?;
        let cached = if state.seq > 0 {
            let current = current
                .as_ref()
                .ok_or_else(|| Error::new("creation lost its configuration"))?;
            if state.source_identity.is_some() && !source_bound(current, &state) {
                return Err(Error::new("creation state belongs to another source"));
            }
            let first = envelope::parse(&pending.envelope)?;
            let held = state
                .digest
                .ok_or_else(|| Error::new("creation state has no version digest"))?;
            let cached = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)?;
            let verified = cached.and_then(|bytes| {
                let (env, _, _) = envelope::verify_for_reader(
                    bytes.as_slice(),
                    &current.name,
                    &current.signer.unwrap(),
                    &current.followed,
                    Some(&Floor::below(&first)),
                )
                .ok()?;
                (env.seq == state.seq && env.digest() == held).then(|| bytes.as_slice().to_vec())
            });
            verified.or_else(|| {
                (state.seq == 1 && held == first.digest()).then(|| pending.envelope.clone())
            })
        } else {
            None
        };
        (current, state, cached)
    };
    let bytes = if let Some(bytes) = cached {
        bytes
    } else {
        let pin = current.as_ref().unwrap_or(&initial);
        let first = envelope::parse(&pending.envelope)?;
        let floor = if state.seq > 0 {
            Floor::version(
                state.seq,
                state
                    .digest
                    .ok_or_else(|| Error::new("creation state has no version digest"))?,
            )
        } else {
            Floor::below(&first)
        };
        match transport::fetch(&initial.source, None, Duration::from_secs(30))? {
            Fetched::Body(bytes) => {
                envelope::verify_for_reader(
                    &bytes,
                    &pin.name,
                    &pin.signer.unwrap(),
                    &pin.followed,
                    Some(&floor),
                )?;
                bytes
            }
            Fetched::Missing => {
                if state.seq > 0 {
                    return Err(Error::new(
                        "creation's newer cache and remote version are unavailable; refusing a rollback",
                    ));
                }
                // Whether an earlier publication of this intent may have landed somewhere; this
                // one is marked before it is sent, so a crash or a lost reply leaves the mark.
                let digest = aterm_digest::Sha256::digest(&pending.packet);
                let sent_before = claim(paths)?
                    .is_some_and(|claim| claim.context == context && claim.phase == Phase::Sent);
                save_claim(paths, &pending.intent, digest, Phase::Sent)?;
                match transport::publish_initial(&initial.source, &pending.envelope) {
                    Ok(()) => pending.envelope.clone(),
                    // A conflict stored nothing of this attempt: what an earlier one did stands.
                    Err(error) if !sent_before && error.kind() == crate::ErrorKind::NotStored => {
                        save_claim(paths, &pending.intent, digest, Phase::Ready)?;
                        return Err(error);
                    }
                    Err(error) if sent_before && error.kind() == crate::ErrorKind::Refused => {
                        return Err(Error::new(format!(
                            "{error}; an earlier attempt to publish this vault may have been \
                             stored elsewhere, so this Mac keeps the creation for its subscriber \
                             to finish"
                        )));
                    }
                    Err(error) if error.kind() == crate::ErrorKind::Refused => {
                        retire(paths, context)
                            .map_err(|kept| Error::new(format!("{error}; {kept}")))?;
                        return Err(Error::of_kind(
                            crate::ErrorKind::Refused,
                            format!(
                                "{error}; nothing was stored, so this Mac gave up creating it and \
                                 can create or join a vault again"
                            ),
                        ));
                    }
                    Err(error) => return Err(error),
                }
            }
            Fetched::NoNewer(_) => return Err(Error::new("creation source returned no envelope")),
        }
    };
    let config = {
        let _lock = StateLock::take(paths)?;
        match config(paths)? {
            Some(config) => {
                compatible(&config, &initial)?;
                config
            }
            None if current.is_none() && load_state(paths)?.seq == 0 => {
                save_config(paths, &initial)?;
                initial
            }
            None => {
                return Err(Error::new(
                    "creation configuration changed during publication",
                ));
            }
        }
    };
    let local = apply_fetched(paths, config, state, Fetched::Body(bytes))
        .and_then(|_| {
            let lock = StateLock::take(paths)?;
            let current = super::config(paths)?
                .ok_or_else(|| Error::new("creation lost its configuration"))?;
            compatible(&current, &pending.config()?)?;
            let state = load_state(paths)?;
            if state.seq == 0
                || state.membership != Stored::In
                || state.failed
                || !state.pending.is_empty()
                || !source_bound(&current, &state)
                || !cache_intact(paths, &state)
                || !files_intact(paths, &state)
            {
                return Err(Error::new(
                    "creation is published; local installation remains incomplete",
                ));
            }
            drop(lock);
            // Establish the immutable live Commons claim only after the
            // signed vault is installed. Existing pins and history are never
            // replaced, and incomplete bootstrap keeps the outbox pending.
            let snapshot = crate::commons::cached(paths)?
                .ok_or_else(|| Error::new("creation Commons observation is unavailable"))?;
            install_bootstrap(paths, &pending, &snapshot)
        })
        .err()
        .map(|error| error.to_string());
    if local.is_none() {
        let _state_lock = StateLock::take(paths)?;
        let current =
            super::config(paths)?.ok_or_else(|| Error::new("creation lost its configuration"))?;
        compatible(&current, &pending.config()?)?;
        let state = load_state(paths)?;
        if state.failed || !state.pending.is_empty() || !source_bound(&current, &state) {
            return Err(Error::new("creation changed before completion"));
        }
        let digest = aterm_digest::Sha256::digest(&pending.packet);
        let mut log = open(paths, context, true)?;
        let retained = replay(paths, &log, context)?
            .filter(|p| p.packet == pending.packet)
            .ok_or_else(|| Error::new("creation preparation changed before completion"))?;
        log.finish_open()?;
        if !retained.complete {
            log.append(COMPLETED, digest.to_vec())?;
        }
        drop(log);
        save_claim(paths, &pending.intent, digest, Phase::Complete)?;
    }
    pending.receipt(paths, local)
}

fn install_bootstrap(
    paths: &Paths,
    pending: &Pending,
    snapshot: &crate::commons::Snapshot,
) -> crate::Result<()> {
    // A join may have changed the slot after the snapshot was observed. Pin
    // only while that exact authenticated observation still describes it.
    // from_snapshot does local custody I/O only and never retakes StateLock.
    let _lock = StateLock::take(paths)?;
    let current = config(paths)?.ok_or_else(|| Error::new("creation lost its configuration"))?;
    compatible(&current, &pending.config()?)?;
    let state = load_state(paths)?;
    if state.seq != snapshot.sequence()
        || state.digest != Some(*snapshot.digest())
        || !source_bound(&current, &state)
    {
        return Err(Error::new(
            "creation observation changed before Commons bootstrap",
        ));
    }
    let expected = admin_keys::control_descriptor(&pending.admin(paths)?.doc)?
        .ok_or_else(|| Error::new("creation has no Commons descriptor"))?;
    let actual = crate::commons::bootstrap::Descriptor::decode(snapshot.control())?;
    if actual.trust() != expected.trust() {
        return Err(Error::new(
            "creation Commons observation belongs to another network",
        ));
    }
    let bootstrap = crate::commons::bootstrap::Bootstrap::from_creation_snapshot(paths, snapshot)?;
    let journal = crate::commons::journal::Journal::open_existing(
        bootstrap.journal_dir(),
        bootstrap.trust(),
    )?;
    if !matches!(
        journal.state(),
        crate::commons::journal::State::Ready { .. }
    ) {
        return Err(Error::new("creation Commons control history is not ready"));
    }
    drop(journal);
    if snapshot.sequence() == 1 && bootstrap.access()?.is_none() {
        return Err(Error::new("creation founder membership is not ready"));
    }
    Ok(())
}

fn compatible(current: &Config, initial: &Config) -> crate::Result<()> {
    if current.source != initial.source
        || current.name != initial.name
        || current.host != initial.host
        || !pin_admitted(current, initial.signer)
    {
        return Err(Error::new(
            "creation will not replace a different configured vault",
        ));
    }
    Ok(())
}

pub(super) fn enroll_founder(
    paths: &Paths,
    doc: &Document,
    host: &str,
    seed: &[u8; 32],
    descriptor: crate::commons::bootstrap::Descriptor,
) -> crate::Result<crate::commons::bootstrap::Descriptor> {
    use crate::commons::{
        binding::Pending,
        bootstrap::{Descriptor, Device},
        journal::Journal,
        membership::{self, Eligibility},
        node::Node,
        roster::{self, Grant, Role},
    };
    let trust = descriptor.trust();
    let scratch = aterm_tempfile::Builder::new()
        .prefix("creation-control-")
        .tempdir_in(&paths.dir)?;
    let mut journal = Journal::open(scratch.path(), trust)?;
    journal.ingest(Node::foundation(trust.authority)?.bytes())?;
    let device =
        Device::open(paths)?.ok_or_else(|| Error::new("founder has no device identity"))?;
    let request = device.request(trust.network, trust.authority)?;
    let (mut pending, challenge) = Pending::issue(&request, trust.network, trust.authority)?;
    let possession = pending.verify(&device.answer(&request, &challenge)?)?;
    let eligibility = Eligibility::from_document(doc);
    let certificate = membership::approve(seed, &journal, &eligibility, host, possession)?;
    let node = roster::rotate(
        seed,
        &eligibility,
        &journal,
        vec![Grant::certified(host, certificate, Role::Writer)],
    )?;
    journal.ingest(node.bytes())?;
    Descriptor::from_journal(&journal)
}

#[cfg(test)]
mod tests;
