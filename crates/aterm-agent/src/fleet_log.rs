// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet admission as this Mac's membership log says it (`docs/DESIGN-fluid-swarm-2026-10-28.md`
//! §4.1, F17): which node keys the log admits and which it holds out, for the fleet listener's
//! handshake, a join it answers, the signed book entries it merges and a drive it admits.
//!
//! The log is the record store F13 fills (the secrets directory's `records`), read as authority
//! reads it (`aterm_secrets::client::load_acknowledged_and_ahead`): it grants from the membership
//! history this Mac acknowledged, and holds out every key that history holds out or the records
//! ahead of it do, records a writer made durable that no writer has acknowledged yet (one cut
//! short between the two), so a removal or a rotation stands before its acknowledgement and an
//! admission waits for one. This reads it back, evaluates it, and keeps the verdict until the
//! store or a file beside it changes, that history's included. A Mac with no vault, or whose
//! store holds no genesis yet while the version of its vault it applied names no log, has no
//! verdict (`None`), and admission falls back to its book's keyring.
//!
//! A vault that exists but cannot be read now is no such Mac: a vault apply holding the state lock,
//! a layout transition cut before its pin, the fenced layout's data file missing, the flat
//! layout's records file gone or an older snapshot put back behind the history this Mac
//! acknowledged (the file gone reads as an empty store, which that history, holding the vault's
//! genesis, tells from a store with no genesis yet), or an older aterm whose reader holds fewer
//! records than the store's pinned capacity. A reader that read a verdict before keeps it through
//! such a read, and through a read that finds no log, while the vault it read it under stays
//! configured: its `source.toml` unchanged, or rewritten for the same vault, its name kept and its
//! key line keeping a key of the one before (a mirror added, a host renamed, a signer rotation;
//! `Vault`). A vault's store reads no log only where that history holds no genesis for it under
//! its configured keys (one no key signs keeps none, and none is left once its files go with the
//! records): there the flat layout's records file gone still reads as no genesis yet. A store
//! that reads no log while the version of the vault this Mac applied was written at one
//! (`aterm_secrets::client::applied_version_names_log`: its locker names the log's heads) fails
//! its read instead, as that vault's records are not here: after a join of it, before they
//! arrive, or the records file gone with that history. So a reader with no verdict before falls
//! back to the book only where the version applied names no log. Once the
//! configuration names another vault or none (this Mac left, or joined another vault, or its
//! vault was replaced onto a key line that keeps no key of the one before), the verdict is kept
//! through neither: a read that fails then answers as one with no verdict to keep, and one that
//! finds no log where the version applied names none (no vault; a join that waits to be let in,
//! whose version applied holds no locker for this Mac yet; or a version whose locker carries no
//! heads) falls back to the book. Once the new vault's log reads, a key it does not bind is
//! judged as with no log (F17). Neither fallback lets a Mac of the vault left drive this one by a
//! grant made there: the grants of a fleet taken under another vault than the one configured
//! admit no Mac (`crate::fleet_vault`), until this Mac takes a fleet under the vault configured
//! (a join, or a sync of that vault's default fleet code). A reader
//! with no verdict to keep through a read that fails answers [`MemberKeys::unreadable`], under
//! which every key stands out, so no node the book alone would admit gets in until the log reads
//! again. What this Mac may still do under it: its listener keeps running and refuses every
//! proved dialer, as with no deny list read (`aterm_net::fleet::Denied::under`); its heartbeat and
//! its survey facts push dial no member; it removes no stored facts, book entry or grant for the
//! verdict's sake, and its census lists no peer the verdict cannot judge and denies none for it; it
//! authors no `Node` record for itself ([`ensure_local_node_key_in_log`]); and its census does not
//! count this Mac itself out.

use crate::fleet_vault::Vault;
use aterm_net::fleet::NodeKey;
use aterm_net::fleet_member::{MemberKeys, Standing};
use aterm_secrets::membership_log::{Log, Membership, NodeStanding};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Each node key `membership` binds, as admission reads it: a member's current key is a
/// member's; the key of a Mac the log removed, one its member replaced and one two members claim
/// are out. A key no record binds is not listed.
#[must_use]
pub fn member_keys(membership: &Membership) -> MemberKeys {
    membership
        .nodes()
        .iter()
        .filter_map(|(key, standing)| {
            let standing = match standing {
                NodeStanding::Member(_) => Standing::Member,
                NodeStanding::Removed(_) => {
                    Standing::Out("the membership log removed the Mac that holds it".to_owned())
                }
                NodeStanding::Retired(_) => {
                    Standing::Out("its member bound another node key since".to_owned())
                }
                NodeStanding::Contested => {
                    Standing::Out("two members of the membership log claim it".to_owned())
                }
                NodeStanding::Unknown => return None,
            };
            Some((NodeKey::from_bytes(*key), standing))
        })
        .collect()
}

/// Each node key `log` binds, as admission reads it: a member's current key is a member's unless
/// the newest `[everyone]` epoch leaves that member out; the key of a Mac the log removed, one
/// its member replaced, one two members claim, and one left out of `[everyone]` are out.
#[must_use]
pub fn log_member_keys(log: &Log) -> MemberKeys {
    log_member_keys_with_roster(log, None)
}

#[must_use]
pub fn log_member_keys_with_roster(
    log: &Log,
    roster: Option<&aterm_secrets::swarm_roster::SwarmRoster>,
) -> MemberKeys {
    log_member_standings(log, roster).into_iter().collect()
}

/// [`log_member_keys_with_roster`]'s standing of each key it names.
fn log_member_standings(
    log: &Log,
    roster: Option<&aterm_secrets::swarm_roster::SwarmRoster>,
) -> std::collections::BTreeMap<NodeKey, Standing> {
    let membership = log.membership();
    let member_denied_in_roster =
        |r: &aterm_secrets::swarm_roster::SwarmRoster, rec: &str, label: &str, name: &str| {
            if let Some(rm) = r.member_by_recipient(rec) {
                r.is_member_denied(rm)
            } else {
                aterm_secrets::swarm_roster::leading_m_label(label)
                    .or_else(|| aterm_secrets::swarm_roster::leading_m_label(name))
                    .is_some_and(|(_, c)| r.is_label_denied(&c))
            }
        };
    let mut entries: std::collections::BTreeMap<NodeKey, Standing> = membership
        .nodes()
        .iter()
        .filter_map(|(key, standing)| {
            let standing = match standing {
                NodeStanding::Member(actor)
                    if membership
                        .get(actor)
                        .is_some_and(|m| log.recipient_is_removed(&m.binding.recipient())) =>
                {
                    Standing::Out("the membership log removed the Mac that holds it".to_owned())
                }
                NodeStanding::Member(actor)
                    if log.audience_left_out(
                        &aterm_secrets::membership_log::EpochAudience::Everyone,
                        actor,
                    ) || membership.get(actor).is_some_and(|m| {
                        log.audience_left_out_recipient(
                            &aterm_secrets::membership_log::EpochAudience::Everyone,
                            &m.binding.recipient(),
                        )
                    }) =>
                {
                    Standing::Out(
                        "the vault's newest key leaves the Mac that holds it out".to_owned(),
                    )
                }
                NodeStanding::Member(actor)
                    if membership.get(actor).is_some_and(|m| {
                        roster.is_some_and(|r| {
                            r.is_node_denied(&aterm_codec::hex::encode_lower(key))
                                || member_denied_in_roster(
                                    r,
                                    &m.binding.recipient().to_bech32(),
                                    &m.label,
                                    &m.name,
                                )
                        })
                    }) =>
                {
                    Standing::Out("the swarm roster denies the Mac that holds it".to_owned())
                }
                NodeStanding::Member(_) => Standing::Member,
                NodeStanding::Removed(_) => {
                    Standing::Out("the membership log removed the Mac that holds it".to_owned())
                }
                NodeStanding::Retired(_) => {
                    Standing::Out("its member bound another node key since".to_owned())
                }
                NodeStanding::Contested => {
                    Standing::Out("two members of the membership log claim it".to_owned())
                }
                NodeStanding::Unknown => return None,
            };
            Some((NodeKey::from_bytes(*key), standing))
        })
        .collect();
    if let aterm_secrets::membership_log::Body::Genesis { founders, .. } = log.genesis().body() {
        let founder_out = |founder: &aterm_secrets::membership_log::Founder,
                           nk: [u8; 32]|
         -> Option<&'static str> {
            if log.recipient_is_removed(&founder.recipient) {
                Some("the membership log removed the Mac that holds it")
            } else if log.audience_left_out_recipient(
                &aterm_secrets::membership_log::EpochAudience::Everyone,
                &founder.recipient,
            ) {
                Some("the vault's newest key leaves the Mac that holds it out")
            } else if membership.members().values().any(|lm| {
                lm.binding.recipient() == founder.recipient
                    && lm.node_key.is_some_and(|mk| mk != [0; 32] && mk != nk)
            }) {
                Some("its member bound another node key since")
            } else if roster.is_some_and(|r| {
                r.is_node_denied(&aterm_codec::hex::encode_lower(&nk))
                    || member_denied_in_roster(
                        r,
                        &founder.recipient.to_bech32(),
                        &founder.label,
                        &founder.name,
                    )
            }) {
                Some("the swarm roster denies the Mac that holds it")
            } else {
                None
            }
        };
        for founder in founders {
            let Some(nk) = founder.node_key.filter(|k| *k != [0; 32]) else {
                continue;
            };
            let key = NodeKey::from_bytes(nk);
            if entries.contains_key(&key) {
                continue;
            }
            if let Some(why) = founder_out(founder, nk)
                && !founders
                    .iter()
                    .any(|f| f.node_key == Some(nk) && founder_out(f, nk).is_none())
            {
                entries.insert(key, Standing::Out(why.to_owned()));
            }
        }
    }
    if let Some(r) = roster {
        let roster_member_out =
            |m: &aterm_secrets::swarm_roster::SwarmMember, key: NodeKey| -> Option<&'static str> {
                let rec = aterm_secrets::age::Recipient::parse(m.recipient.trim()).ok();
                if rec
                    .as_ref()
                    .is_some_and(|rec| log.recipient_is_removed(rec))
                {
                    Some("the membership log removed the Mac that holds it")
                } else if rec.as_ref().is_some_and(|rec| {
                    log.audience_left_out_recipient(
                        &aterm_secrets::membership_log::EpochAudience::Everyone,
                        rec,
                    )
                }) {
                    Some("the vault's newest key leaves the Mac that holds it out")
                } else if rec.as_ref().is_some_and(|rec| {
                    membership.members().values().any(|lm| {
                        lm.binding.recipient() == *rec
                            && lm
                                .node_key
                                .is_some_and(|nk| nk != [0; 32] && nk != *key.bytes())
                    })
                }) {
                    Some("its member bound another node key since")
                } else if r.is_member_denied(m) {
                    Some("the swarm roster denies the Mac that holds it")
                } else {
                    None
                }
            };
        for rm in &r.members {
            if let Some(key) = rm
                .node_key
                .as_deref()
                .and_then(|k| NodeKey::from_hex(&k.trim().to_ascii_lowercase()))
                && let Some(why) = roster_member_out(rm, key)
            {
                let key_hex = key.hex();
                let claimed_by_active = !r.is_node_denied(&key_hex)
                    && r.members.iter().any(|m| {
                        roster_member_out(m, key).is_none()
                            && m.node_key
                                .as_deref()
                                .is_some_and(|k| k.trim().eq_ignore_ascii_case(&key_hex))
                    });
                if !claimed_by_active {
                    entries
                        .entry(key)
                        .or_insert_with(|| Standing::Out(why.to_owned()));
                }
            }
        }
        for denied in &r.deny_nodes {
            if let Some(key) = NodeKey::from_hex(&denied.trim().to_ascii_lowercase()) {
                entries.entry(key).or_insert_with(|| {
                    Standing::Out("the swarm roster denies the Mac that holds it".to_owned())
                });
            }
        }
    }
    entries
}

/// The admission of the store `paths` keeps, as authority reads it
/// ([`aterm_secrets::client::load_acknowledged_and_ahead`]): what the acknowledged log admits
/// and holds out, and every key the records ahead of it hold out besides. Those records verify
/// and this Mac made them durable, so a removal or a rotation they hold stands at once; they
/// grant nothing until a writer acknowledges them. `None` with no log, where the vault has none
/// this Mac knows of either.
///
/// # Errors
/// The store's read fails; or, of kind [`aterm_secrets::ErrorKind::MembershipHistoryUnavailable`],
/// it holds no log while the version of the vault this Mac applied was written at one
/// ([`aterm_secrets::client::applied_version_names_log`]): that vault's records are not here (as
/// after a join of it, before they arrive), so its removals cannot be read, and the book does not
/// stand in for them.
pub(crate) fn acknowledged_member_keys(
    paths: &aterm_secrets::client::Paths,
) -> aterm_secrets::Result<Option<MemberKeys>> {
    let read = aterm_secrets::client::load_acknowledged_and_ahead(paths)?;
    let Some(log) = read.acknowledged.log() else {
        if aterm_secrets::client::applied_version_names_log(paths)? {
            return Err(aterm_secrets::Error::of_kind(
                aterm_secrets::ErrorKind::MembershipHistoryUnavailable,
                "the vault's membership log is not here yet",
            ));
        }
        return Ok(None);
    };
    let roster = aterm_secrets::client::local_swarm_roster_for_log(paths, log)?;
    let mut standings = log_member_standings(log, roster.as_ref());
    if let Some(ahead) = read.ahead.as_ref().and_then(|store| store.log()) {
        let roster = aterm_secrets::client::local_swarm_roster_for_log(paths, ahead)?;
        for (key, standing) in log_member_standings(ahead, roster.as_ref()) {
            if matches!(standing, Standing::Out(_))
                && !matches!(standings.get(&key), Some(Standing::Out(_)))
            {
                standings.insert(key, standing);
            }
        }
    }
    Ok(Some(standings.into_iter().collect()))
}

/// How a [`LocalLog`] reads its store: the member keys it evaluates, `None` before a genesis.
type Reader = Box<dyn Fn() -> aterm_secrets::Result<Option<MemberKeys>> + Send + Sync>;

/// Why a [`MemberKeys::unreadable`] verdict refuses, in the words a refusal quotes.
pub const UNREAD: &str =
    "this Mac's membership log cannot be read now (`aterm secrets sync` says why)";

/// What the store file's metadata says of it. A write replaces the whole file
/// (`aterm_secrets::fsx::write_atomic`), so each one moves it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    inode: u64,
}

/// `file`'s [`Stamp`], `None` where its metadata cannot be read (the file missing included).
/// Named apart from the lock-taking `stamp`s the lock-order census, which resolves a call by its
/// name, would take a call to it for (`aterm_uds::exitwatch`'s, `aterm-gui`'s `app_nudge`'s):
/// [`LocalLog::keys`] calls it with its verdict's lock held.
fn metadata_stamp(file: &Path) -> Option<Stamp> {
    let meta = std::fs::symlink_metadata(file).ok()?;
    Some(Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        inode: meta.ino(),
    })
}

/// A cold read's physical identity. The handle prevents inode reuse while the read runs;
/// change time catches an in-place write even when the original bytes are restored.
struct ReadIdentity {
    _file: std::fs::File,
    stamp: ReadStamp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ReadStamp {
    device: u64,
    inode: u64,
    changed: (i64, i64),
}

impl ReadStamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

impl ReadIdentity {
    fn open(path: &Path, directory: bool) -> std::io::Result<Self> {
        let flags =
            libc::O_NOFOLLOW | libc::O_NONBLOCK | if directory { libc::O_DIRECTORY } else { 0 };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(path)?;
        let metadata = file.metadata()?;
        if !(if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        }) {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        let identity = Self {
            _file: file,
            stamp: ReadStamp::of(&metadata),
        };
        if !identity.stands(path, directory) {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        Ok(identity)
    }

    fn stands(&self, path: &Path, directory: bool) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| {
            (if directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            }) && self.stamp == ReadStamp::of(&metadata)
        })
    }
}

/// Only a cold read holds these identities. Warm semantic equality may still ignore a
/// heartbeat's atomic state rewrite. A held ancestor witnesses a not-yet-created vault too.
struct StateRead {
    directory_path: PathBuf,
    directory: ReadIdentity,
    state: Option<ReadIdentity>,
}

impl StateRead {
    fn of(paths: &aterm_secrets::client::Paths) -> Option<Self> {
        let mut directory_path = paths.dir.clone();
        let directory = loop {
            match ReadIdentity::open(&directory_path, true) {
                Ok(directory) => break directory,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    directory_path = directory_path.parent()?.to_path_buf();
                }
                Err(_) => return None,
            }
        };
        let state_path = paths.dir.join("state.toml");
        let state = match ReadIdentity::open(&state_path, false) {
            Ok(state) => Some(state),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };
        Some(Self {
            directory_path,
            directory,
            state,
        })
    }

    fn stands(&self, paths: &aterm_secrets::client::Paths) -> bool {
        let state_path = paths.dir.join("state.toml");
        self.directory.stands(&self.directory_path, true)
            && match &self.state {
                Some(state) => state.stands(&state_path, false),
                None => matches!(
                    std::fs::symlink_metadata(&state_path),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                ),
            }
    }
}

/// The membership store one process reads, with the verdict it last evaluated and the stamp it
/// read it at.
pub struct LocalLog {
    file: PathBuf,
    paths: Option<aterm_secrets::client::Paths>,
    /// The other files the reader reads, stamped by their metadata; for a store `paths` keeps,
    /// the vault's configuration (`source.toml`) first. Its `state.toml` is stamped by what it
    /// holds ([`StateStamp`]).
    aux_files: Vec<PathBuf>,
    read: Reader,
    held: Mutex<Option<Held>>,
    /// The verdict while the store cannot be read and none was read before: one allocation, so a
    /// watcher comparing verdicts sees no change until the store reads again.
    unread: Arc<MemberKeys>,
}

struct Held {
    /// `None` while the store's file is missing.
    stamp: Option<Stamp>,
    aux_stamps: Vec<Option<Stamp>>,
    state: StateStamp,
    /// The vault the verdict was read under; `None` with none pinned, or where its
    /// configuration moved during the read, so which one the verdict is of is unknown.
    vault: Option<Vault>,
    keys: Option<Arc<MemberKeys>>,
}

/// What a [`LocalLog`] a store's `paths` keep stamps that vault's `state.toml` by: what it holds
/// but the heartbeat the vault's rounds rewrite about once a minute
/// ([`aterm_secrets::client::state_standing`]: when the vault last heard this Mac, the receipts
/// of its rounds), which no verdict is read from. Stamped by its metadata, each round read the
/// store, the history and the envelope again: 16.5 MB a round on a Linux arm64 member
/// (2026-11-09). A semantic digest can return to its old value. [`StateRead`] fences each cold
/// read with physical state and namespace identities; [`StateStamp::across`] additionally
/// rejects different semantic stamps at the read's endpoints.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StateStamp {
    /// A store no `paths` keeps (a test's lab), whose reader reads no vault state.
    Unkept,
    /// What it holds but the heartbeat, digested; `None` with no file.
    Standing(Option<[u8; 32]>),
    /// It could not be read, or it moved during the read the verdict was held from: a stamp no
    /// later one matches, so the next call reads again.
    Unread,
}

impl StateStamp {
    fn of(paths: Option<&aterm_secrets::client::Paths>) -> Self {
        match paths.map(aterm_secrets::client::state_standing) {
            None => Self::Unkept,
            Some(Ok(standing)) => Self::Standing(standing),
            Some(Err(_)) => Self::Unread,
        }
    }

    /// Whether a verdict stamped so stands where the state is stamped `now`.
    fn stands(self, now: Self) -> bool {
        self == now && now != Self::Unread
    }

    /// The semantic stamp a cold verdict is held under: this one when `after` is equal,
    /// [`Self::Unread`] otherwise. Equal semantic endpoints alone do not detect a full ABA;
    /// a kept vault's [`StateRead`] must also stand across the cold read.
    fn across(self, after: Self) -> Self {
        if self == after { self } else { Self::Unread }
    }
}

impl LocalLog {
    /// The store `paths` keeps (`Paths::records_file`), as authority reads it: granting from
    /// the history this Mac acknowledged, and holding out what the records ahead of it hold out
    /// too (`aterm_secrets::client::load_acknowledged_and_ahead`).
    #[must_use]
    pub fn of(paths: &aterm_secrets::client::Paths) -> Self {
        let from = paths.clone();
        // The vault's configuration first ([`LocalLog::same_vault`]); then the acknowledged
        // history, which an acknowledgement alone moves. `state.toml` is stamped by what it holds
        // ([`StateStamp`]).
        let aux_files = [
            paths.dir.join("source.toml"),
            paths.dir.join("vault"),
            paths.dir.join("identity"),
        ]
        .into_iter()
        .chain(paths.acknowledged_history_files())
        .collect();
        let mut log = Self::reading(
            paths.records_file(),
            aux_files,
            Box::new(move || acknowledged_member_keys(&from)),
        );
        log.paths = Some(paths.clone());
        log
    }

    /// The store file `file`, founded on the genesis `network` names: a test's lab of its own.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn founded_at(file: PathBuf, network: aterm_secrets::membership_log::RecordId) -> Self {
        let from = file.clone();
        Self::reading(
            file,
            Vec::new(),
            Box::new(move || {
                use std::io::Read as _;
                const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;
                if std::fs::symlink_metadata(&from)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                {
                    return Ok(None);
                }
                let (file, meta) = aterm_uds::regular_file::open(&from)
                    .map_err(|e| aterm_secrets::Error::new(format!("the record store: {e}")))?;
                if meta.len() > MAX_STORE_BYTES {
                    return Err(aterm_secrets::Error::new(
                        "the record store: file exceeds size limit",
                    ));
                }
                let cap = usize::try_from(meta.len().min(MAX_STORE_BYTES))
                    .unwrap_or(0)
                    .saturating_add(1);
                let mut bytes = Vec::with_capacity(cap);
                file.take(MAX_STORE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| aterm_secrets::Error::new(format!("the record store: {e}")))?;
                if bytes.len() as u64 > MAX_STORE_BYTES {
                    return Err(aterm_secrets::Error::new(
                        "the record store: file exceeds size limit",
                    ));
                }
                let mut store = aterm_secrets::membership_log::Store::with_compiled(Some(network));
                store.ingest(&bytes)?;
                Ok(store.log().map(log_member_keys))
            }),
        )
    }

    fn reading(file: PathBuf, aux_files: Vec<PathBuf>, read: Reader) -> Self {
        Self {
            file,
            paths: None,
            aux_files,
            read,
            held: Mutex::new(None),
            unread: Arc::new(MemberKeys::unreadable(UNREAD)),
        }
    }

    /// The store's file.
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// The log's admission now: reuse the last verdict while its file stamps and semantic state
    /// (`state.toml` without heartbeat telemetry, [`StateStamp`]) stand. A cold read whose physical
    /// state or namespace identity moved ([`StateRead`]) supplies no fresh verdict. `None` only
    /// where a read found no log: no vault, or a store with no genesis while the version of the
    /// vault this Mac applied names no log (one that names one fails the read:
    /// `acknowledged_member_keys`).
    /// A store that cannot be read, its data file unresolved or missing included, and the flat
    /// layout's records file gone while the history this Mac acknowledged holds its genesis,
    /// keeps the verdict last read from it while the vault it was read under stays configured
    /// ([`Self::same_vault`]: its configuration unchanged, or rewritten for the same vault), so a
    /// moment's fault never lets in a key the log held out; with none to keep, or one read under
    /// another vault or none configured now (this Mac left, joined another vault, or its vault
    /// was replaced onto another key line), it answers [`MemberKeys::unreadable`], which admits
    /// no key, so no member of the vault left gets in by the verdict read under it. A store that
    /// held a genesis does not lose it while the same vault stays configured: one that reads no
    /// log after a verdict (that file gone where the history holds no genesis for the vault)
    /// keeps that verdict too, until the store reads a log again or the vault configured is
    /// another or none.
    pub fn keys(&self) -> Option<Arc<MemberKeys>> {
        // Snapshot the inputs for hot reuse. Cold reads take the state's semantic stamp again
        // inside StateRead's physical fence below. The data file's is `None` where that file
        // cannot even be resolved.
        let file_stamp = match &self.paths {
            Some(paths) => paths.records_data_file().ok(),
            None => Some(self.file.clone()),
        }
        .map(|file| metadata_stamp(&file));
        let aux_stamps: Vec<Option<Stamp>> =
            self.aux_files.iter().map(|p| metadata_stamp(p)).collect();
        let state = StateStamp::of(self.paths.as_ref());
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(file_stamp) = file_stamp else {
            let vault = self.vault();
            return Some(self.kept_or_unread(held.as_ref(), &aux_stamps, vault.as_ref()));
        };
        if let Some(held) = held.as_ref()
            && held.stamp == file_stamp
            && held.aux_stamps == aux_stamps
            && held.state.stands(state)
        {
            return held.keys.clone();
        }
        // A cold read must not give a verdict read under B after A -> B -> A: semantic
        // equality alone cannot see that interval. Hold fresh physical identities only here,
        // after acquiring `held`, then re-take the semantic stamp inside that fence.
        let window = self.paths.as_ref().map(StateRead::of);
        let state = StateStamp::of(self.paths.as_ref());
        // Read before the store, as the stamps are.
        let vault = self.vault();
        let read = (self.read)();
        let after_state = StateStamp::of(self.paths.as_ref());
        if let Some(paths) = &self.paths
            && !window
                .as_ref()
                .and_then(Option::as_ref)
                .is_some_and(|window| window.stands(paths))
        {
            return Some(self.kept_now_or_unread(held.as_ref()));
        }
        let Ok(keys) = read else {
            return Some(self.kept_now_or_unread(held.as_ref()));
        };
        if keys.is_none()
            && let Some(kept) = self.verdict_to_keep(held.as_ref(), &aux_stamps, vault.as_ref())
        {
            return Some(kept);
        }
        // A configuration that moved since its stamp leaves unknown which vault the read was of.
        let vault = vault.filter(|_| {
            self.aux_files.first().map(|file| metadata_stamp(file)) == aux_stamps.first().copied()
        });
        // A state that moved during the read can move back: its stamp stands for no verdict.
        let state = state.across(after_state);
        let keys = keys.map(Arc::new);
        *held = Some(Held {
            stamp: file_stamp,
            aux_stamps,
            state,
            vault,
            keys: keys.clone(),
        });
        keys
    }

    /// The vault configured now, for a store `paths` keeps; a test's lab
    /// (`LocalLog::founded_at`) configures none.
    fn vault(&self) -> Option<Vault> {
        self.paths
            .as_ref()
            .and_then(|paths| Vault::configured(paths).ok().flatten())
    }

    /// Whether the vault `held` was read under is configured still (`aux_stamps`, the aux
    /// files' stamps now, and `now`, the vault configured now): a store `paths` keeps, its
    /// `source.toml` present and unmoved, or rewritten for the same vault ([`Vault::is`]). A
    /// test's lab (`LocalLog::founded_at`) configures no vault, so it is never one.
    fn same_vault(&self, held: &Held, aux_stamps: &[Option<Stamp>], now: Option<&Vault>) -> bool {
        let unmoved = aux_stamps.first().is_some_and(Option::is_some)
            && held.aux_stamps.first() == aux_stamps.first();
        self.paths.is_some()
            && (unmoved
                || held
                    .vault
                    .as_ref()
                    .zip(now)
                    .is_some_and(|(then, now)| then.is(now)))
    }

    /// The verdict last read from the store where it was read under the vault configured now
    /// ([`Self::same_vault`]): what a read that fails, or one that finds no log, keeps.
    fn verdict_to_keep(
        &self,
        held: Option<&Held>,
        aux_stamps: &[Option<Stamp>],
        now: Option<&Vault>,
    ) -> Option<Arc<MemberKeys>> {
        held.filter(|held| self.same_vault(held, aux_stamps, now))
            .and_then(|held| held.keys.clone())
    }

    /// What a read that fails answers: the verdict [`Self::verdict_to_keep`], or with none, the
    /// unread one. A last read that found no log keeps none either: a store that read empty and
    /// now cannot be read may hold a genesis since. Nor does a verdict read under another vault,
    /// or under none configured now: its members are not this vault's.
    fn kept_or_unread(
        &self,
        held: Option<&Held>,
        aux_stamps: &[Option<Stamp>],
        now: Option<&Vault>,
    ) -> Arc<MemberKeys> {
        self.verdict_to_keep(held, aux_stamps, now)
            .unwrap_or_else(|| Arc::clone(&self.unread))
    }

    /// Fallback after a cold read uses the vault configured now, never the descriptor taken
    /// before a writer moved it. A descriptor whose auxiliary inputs moved is no witness.
    fn kept_now_or_unread(&self, held: Option<&Held>) -> Arc<MemberKeys> {
        let before: Vec<_> = self
            .aux_files
            .iter()
            .map(|path| metadata_stamp(path))
            .collect();
        let vault = self.vault();
        let after: Vec<_> = self
            .aux_files
            .iter()
            .map(|path| metadata_stamp(path))
            .collect();
        if before != after {
            return Arc::clone(&self.unread);
        }
        self.kept_or_unread(held, &after, vault.as_ref())
    }
}

/// Ensure this Mac's fleet `node` identity is bound in `paths`'s membership log via a signed
/// [`aterm_secrets::membership_log::Body::Node`] record when this Mac is an active bound member
/// whose `node.key()` is not yet bound in `log_keys` (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// §3.3, F17).
pub fn ensure_local_node_key_in_log(
    paths: &aterm_secrets::client::Paths,
    node: &aterm_net::fleet::NodeIdentity,
    log_keys: Option<&MemberKeys>,
) -> bool {
    // An unread verdict cannot say whether the key is bound: nothing is authored for its sake.
    if log_keys.is_some_and(|keys| keys.admits(&node.key()) || keys.unread().is_some()) {
        return false;
    }
    let key_bytes = *node.key().bytes();
    aterm_secrets::client::publish_local_node_key(paths, key_bytes, |network, actor| {
        let mut body = Vec::with_capacity(network.len() + actor.len());
        body.extend_from_slice(network);
        body.extend_from_slice(actor);
        node.sign_record("membership node", &body)
    })
    .ok()
    .flatten()
    .is_some()
}

#[cfg(test)]
#[path = "fleet_log_tests.rs"]
mod tests;
