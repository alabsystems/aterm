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
/// but the heartbeat a vault round rewrites about every 25 s
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
        self.paths.as_ref().and_then(Vault::configured)
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
mod tests {
    use super::*;
    use aterm_net::fleet::NodeIdentity;

    /// A write to any file the reader reads, the store's or one beside it, alone reads the
    /// store again; nothing moved serves the verdict held. A lab configures no vault, so a read
    /// that fails keeps no verdict (`a_read_that_fails_keeps_no_verdict_read_under_another_vault`
    /// keeps one under the same vault).
    #[test]
    fn a_change_to_a_file_beside_the_store_alone_reads_it_again() {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = root.path().join("records");
        let beside = root.path().join("vault");
        aterm_secrets::fsx::write_atomic(&store, b"store", 0o600).unwrap();
        aterm_secrets::fsx::write_atomic(&beside, b"admit", 0o600).unwrap();
        let key = NodeIdentity::from_seed(&"e5".repeat(16), &[0x77; 32])
            .unwrap()
            .key();
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let log = LocalLog::reading(store.clone(), vec![beside.clone()], {
            let (beside, reads) = (beside.clone(), Arc::clone(&reads));
            Box::new(move || {
                reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let standing = match std::fs::read(&beside).unwrap().as_slice() {
                    b"admit" => Standing::Member,
                    b"deny" => Standing::Out("denied".to_owned()),
                    _ => return Err(aterm_secrets::Error::new("unreadable")),
                };
                Ok(Some([(key, standing)].into_iter().collect()))
            })
        });
        let reads = || reads.load(std::sync::atomic::Ordering::SeqCst);

        assert!(log.keys().unwrap().admits(&key));
        assert!(log.keys().unwrap().admits(&key));
        assert_eq!(reads(), 1, "nothing moved: the verdict held answers");

        aterm_secrets::fsx::write_atomic(&beside, b"deny", 0o600).unwrap();
        assert!(
            log.keys().unwrap().holds_out(&key),
            "the store's own file did not move"
        );
        assert_eq!(reads(), 2);

        aterm_secrets::fsx::write_atomic(&beside, b"broken", 0o600).unwrap();
        let failed = log.keys().unwrap();
        assert!(
            failed.holds_out(&key) && failed.unread().is_some(),
            "a read that fails, with no vault configured to keep a verdict under, admits no key"
        );
        assert_eq!(reads(), 3);
    }

    /// A reader over `store` and `beside` whose first read replaces the file `during` names
    /// (as every writer of these files does, [`aterm_secrets::fsx::write_atomic`]): how many
    /// reads its first three calls take, nothing written after that first read.
    fn reads_after_a_write_during_the_first_read(during: fn(&Path, &Path) -> PathBuf) -> usize {
        let root = aterm_tempfile::tempdir().unwrap();
        let store = root.path().join("records");
        let beside = root.path().join("vault");
        aterm_secrets::fsx::write_atomic(&store, b"store", 0o600).unwrap();
        aterm_secrets::fsx::write_atomic(&beside, b"beside", 0o600).unwrap();
        let written = during(&store, &beside);
        let key = NodeIdentity::from_seed(&"e5".repeat(16), &[0x77; 32])
            .unwrap()
            .key();
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let log = LocalLog::reading(store.clone(), vec![beside.clone()], {
            let reads = Arc::clone(&reads);
            Box::new(move || {
                if reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    aterm_secrets::fsx::write_atomic(&written, b"written during the read", 0o600)
                        .unwrap();
                }
                Ok(Some([(key, Standing::Member)].into_iter().collect()))
            })
        });
        for _ in 0..3 {
            assert!(log.keys().unwrap().admits(&key));
        }
        reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The stamps are taken before the read: a write to the store's file during a read leaves
    /// the verdict stamped as the file was before it, so the next call reads the store again,
    /// and the one after, nothing moved since, serves the verdict held.
    #[test]
    fn a_write_to_the_store_during_a_read_reads_it_again() {
        assert_eq!(
            reads_after_a_write_during_the_first_read(|store, _| store.to_path_buf()),
            2
        );
    }

    /// As `a_write_to_the_store_during_a_read_reads_it_again`, for a file beside the store.
    #[test]
    fn a_write_beside_the_store_during_a_read_reads_it_again() {
        assert_eq!(
            reads_after_a_write_during_the_first_read(|_, beside| beside.to_path_buf()),
            2
        );
    }

    /// `source.toml` for the vault `name` pinned to the key `[signer; 32]`, reached from the
    /// keys `followed` names, as this Mac's `host`.
    fn configured(name: &str, host: &str, signer: u8, followed: &[u8]) -> String {
        let key = |byte: u8| format!("\"{}\"", aterm_secrets::b64::encode_padded(&[byte; 32]));
        let followed: Vec<String> = followed.iter().map(|byte| key(*byte)).collect();
        format!(
            "url = \"http://127.0.0.1:9/v1/{name}\"\nname = \"{name}\"\nsigner = {}\n\
             followed = [{}]\nhost = \"{host}\"\n",
            key(signer),
            followed.join(", ")
        )
    }

    /// A [`LocalLog`] of the store `paths` keeps whose read admits `key` while the store's file
    /// says `admit` and fails on any other content; with each read it first runs `during`.
    fn admitting(
        paths: &aterm_secrets::client::Paths,
        key: NodeKey,
        during: impl Fn() + Send + Sync + 'static,
    ) -> LocalLog {
        let store = paths.records_file();
        let mut log = LocalLog::reading(store.clone(), vec![paths.dir.join("source.toml")], {
            Box::new(move || {
                during();
                match std::fs::read(&store).unwrap().as_slice() {
                    b"admit" => Ok(Some([(key, Standing::Member)].into_iter().collect())),
                    _ => Err(aterm_secrets::Error::new("unreadable")),
                }
            })
        });
        log.paths = Some(paths.clone());
        log
    }

    /// A read that fails keeps the verdict last read only while the vault it was read under
    /// stays configured: its `source.toml` unchanged, or rewritten for the same vault (a host
    /// renamed, a signer rotation, the old signer among the keys followed). Once that file names
    /// another vault (another name, or another key line under the same name, as a replacement
    /// pins) or none, a read that fails, or a data file that cannot be resolved, admits no key,
    /// so no member of the vault read before gets in by the verdict read under it.
    #[test]
    fn a_read_that_fails_keeps_no_verdict_read_under_another_vault() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(root.path());
        aterm_secrets::fsx::ensure_private_dir(&paths.dir).unwrap();
        let store = paths.records_file();
        let source = paths.dir.join("source.toml");
        let write = |file: &Path, bytes: &[u8]| {
            aterm_secrets::fsx::write_atomic(file, bytes, 0o600).unwrap();
        };
        let configure = |name: &str, host: &str, signer: u8, followed: &[u8]| {
            write(&source, configured(name, host, signer, followed).as_bytes());
            let now = aterm_secrets::client::config(&paths).unwrap().unwrap();
            assert_eq!(
                (now.name.as_str(), now.signer, now.followed.len()),
                (name, Some([signer; 32]), followed.len()),
                "PRECONDITION: the vault configured"
            );
        };
        let unresolved = || {
            std::fs::remove_file(&store).unwrap();
            std::fs::create_dir(&store).unwrap();
            assert!(
                paths.records_data_file().is_err(),
                "PRECONDITION: a layout transition cut before its pin"
            );
        };
        let broken = || {
            std::fs::remove_dir(&store).unwrap();
            write(&store, b"broken");
        };
        write(&store, b"admit");
        configure("a", "m22", 1, &[]);
        let key = NodeIdentity::from_seed(&"e5".repeat(16), &[0x77; 32])
            .unwrap()
            .key();
        let log = admitting(&paths, key, || {});
        let kept = |when: &str| {
            assert!(
                log.keys().is_some_and(|keys| keys.admits(&key)),
                "{when}: the verdict of the vault still configured is dropped"
            );
        };
        let unread = |when: &str| {
            let keys = log.keys().unwrap_or_else(|| panic!("{when}: a verdict"));
            assert!(
                keys.unread().is_some() && !keys.admits(&key),
                "{when}: the verdict read under the vault before admits its member"
            );
        };

        kept("read");
        write(&store, b"broken");
        kept("CONTROL: a read that fails under the same configuration");
        unresolved();
        kept("CONTROL: a data file unresolved under the same configuration");
        configure("a", "m22-renamed", 1, &[]);
        kept("a data file unresolved, the same vault's host renamed");
        broken();
        kept("a read that fails, the same vault's host renamed");
        configure("a", "m22", 2, &[1]);
        kept("a read that fails after a signer rotation");

        configure("a", "m22", 3, &[]);
        unread("a read that fails under another key line of the vault's name");
        configure("b", "m22", 1, &[]);
        unread("a read that fails under another vault's name");
        unresolved();
        unread("the data file unresolved under another vault");
        broken();
        write(&store, b"admit");
        kept("the vault configured now reads a verdict of its own");
        std::fs::remove_file(&source).unwrap();
        write(&store, b"broken");
        unread("a read that fails under no vault");
    }

    /// A configuration that moves during a read leaves unknown which vault the verdict was read
    /// under: here another vault's, written during the read, then the vault before written back
    /// once it is done. A read that fails after that keeps no verdict, though the vault
    /// configured is the one configured when the read began.
    #[test]
    fn a_configuration_that_moves_during_a_read_keeps_no_verdict() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(root.path());
        aterm_secrets::fsx::ensure_private_dir(&paths.dir).unwrap();
        let store = paths.records_file();
        let source = paths.dir.join("source.toml");
        aterm_secrets::fsx::write_atomic(&store, b"admit", 0o600).unwrap();
        aterm_secrets::fsx::write_atomic(&source, configured("a", "m22", 1, &[]).as_bytes(), 0o600)
            .unwrap();
        let key = NodeIdentity::from_seed(&"e5".repeat(16), &[0x77; 32])
            .unwrap()
            .key();
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let log = admitting(&paths, key, {
            let reads = Arc::clone(&reads);
            let source = source.clone();
            move || {
                if reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    aterm_secrets::fsx::write_atomic(
                        &source,
                        configured("b", "m22", 9, &[]).as_bytes(),
                        0o600,
                    )
                    .unwrap();
                }
            }
        });
        let moved = log.keys().expect("a conservative verdict");
        assert!(
            moved.unread().is_some() && !moved.admits(&key),
            "a configuration moved during the read grants no fresh verdict"
        );
        aterm_secrets::fsx::write_atomic(&source, configured("a", "m22", 1, &[]).as_bytes(), 0o600)
            .unwrap();
        aterm_secrets::fsx::write_atomic(&store, b"broken", 0o600).unwrap();
        let keys = log.keys().expect("a verdict");
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "PRECONDITION: the second call reads the store again"
        );
        assert!(
            keys.unread().is_some() && !keys.admits(&key),
            "a verdict read while the configuration moved is kept as the vault's before it"
        );
    }

    /// A vault's round, and the heartbeat a round rewrites `state.toml` for (when the vault last
    /// heard this Mac), serve the verdict held: no store, history or envelope is read again for
    /// them ([`StateStamp`]). A change of what else the file holds reads the store again, as a
    /// change of any file the reader reads does.
    ///
    /// NEGATIVE CONTROLS (2026-11-09): with `state.toml` back among the files stamped by their
    /// metadata, the round reads the store again (`a round serves the verdict held`); with the
    /// state's stamp never compared (`stands` always true), the change is not read (`a change of
    /// the state reads the store again`).
    #[test]
    fn a_heartbeat_of_the_vault_state_reads_no_store_again() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (running, paths) = seeded_vault(root.path());

        let log = LocalLog::of(&paths);
        let read = log.keys().expect("the seeded log");
        assert!(read.unread().is_none(), "PRECONDITION: a log read");
        let held = |when: &str| {
            assert!(
                Arc::ptr_eq(&read, &log.keys().expect("a verdict")),
                "{when} serves the verdict held"
            );
        };
        held("nothing moved");
        aterm_secrets::client::sync(&paths, None).expect("a round that finds nothing new");
        held("a round");
        bump_state(&paths, "heard");
        held("a heartbeat's rewrite");
        bump_state(&paths, "requested");
        assert!(
            !Arc::ptr_eq(&read, &log.keys().expect("a verdict")),
            "a change of the state reads the store again"
        );
        running.stop();
    }

    /// A vault served from under `root` whose one member, m22 (the returned paths), seeded its
    /// roster, genesis and log and synced them.
    fn seeded_vault(root: &Path) -> (aterm_secrets::server::Running, aterm_secrets::client::Paths) {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let running = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.join("server"),
                advertise: None,
                pins: Vec::new(),
                hold: std::time::Duration::from_millis(50),
                keep_awake: false,
            },
        )
        .unwrap();
        let url = format!("http://{}/v1/test", running.addr);
        let paths = aterm_secrets::client::Paths::for_home(&root.join("m22-home"));
        aterm_secrets::client::create(&paths, &url, None, Some("m22")).unwrap();
        aterm_secrets::client::admin_seed_swarm_roster(&paths, &[]).unwrap();
        aterm_secrets::client::seed_vault_genesis(&paths, &root.join("test.genesis")).unwrap();
        aterm_secrets::client::admin_mirror_log(&paths, &[]).unwrap();
        aterm_secrets::client::sync(&paths, None).unwrap();
        (running, paths)
    }

    /// Rewrite `paths`' `state.toml` (by rename, as every writer of it does) with its `key` line
    /// 60 higher.
    fn bump_state(paths: &aterm_secrets::client::Paths, key: &str) {
        let state = paths.dir.join("state.toml");
        let text = std::fs::read_to_string(&state).unwrap();
        let line = text
            .lines()
            .find(|line| line.starts_with(&format!("{key} = ")))
            .unwrap_or_else(|| panic!("state.toml holds {key}"));
        let value: i64 = line[key.len() + 3..].parse().unwrap();
        let next = text.replace(line, &format!("{key} = {}", value + 60));
        aterm_secrets::fsx::write_atomic(&state, next.as_bytes(), 0o600).unwrap();
    }

    /// A cold read whose state moved supplies no fresh verdict, including A -> B -> A wholly
    /// inside its reader and a state file that appears and disappears. The next stable read is
    /// kept, and a further call serves that verdict without reading the store again.
    ///
    /// HISTORICAL NEGATIVE CONTROL (2026-11-09, before [`StateRead`]): using only the semantic
    /// stamp from before the read (no [`StateStamp::across`]) kept that read on the next call
    /// (`a verdict read while the state moved is read again`, `left: 1, right: 2`). The physical
    /// cold fence now rejects that interval too.
    #[test]
    fn a_verdict_read_while_the_state_moved_and_came_back_is_read_again() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (running, paths) = seeded_vault(root.path());
        let state = paths.dir.join("state.toml");
        let before = std::fs::read(&state).unwrap();
        let standing = aterm_secrets::client::state_standing(&paths).unwrap();
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut log = LocalLog::of(&paths);
        log.read = Box::new({
            let (paths, reads) = (paths.clone(), Arc::clone(&reads));
            move || {
                let keys = acknowledged_member_keys(&paths);
                assert!(
                    matches!(&keys, Ok(Some(keys)) if keys.unread().is_none()),
                    "PRECONDITION: the native reader answered before the state moved"
                );
                if reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    bump_state(&paths, "requested");
                }
                keys
            }
        });
        let reads = || reads.load(std::sync::atomic::Ordering::SeqCst);

        let partial = log
            .keys()
            .expect("a conservative verdict after the moved read");
        assert_ne!(
            aterm_secrets::client::state_standing(&paths).unwrap(),
            standing,
            "PRECONDITION: the read moved the state"
        );
        aterm_secrets::fsx::write_atomic(&state, &before, 0o600).unwrap();
        assert_eq!(
            aterm_secrets::client::state_standing(&paths).unwrap(),
            standing,
            "PRECONDITION: the state holds what it held before the read"
        );
        log.keys().expect("a verdict");
        assert_eq!(
            reads(),
            2,
            "a verdict read while the state moved is read again"
        );
        log.keys().expect("a verdict");
        assert_eq!(reads(), 2, "CONTROL: a read nothing moved during stands");

        // The whole A -> B -> A interval can also fit inside the reader: neither outer
        // semantic stamp sees B. The next call must still read A, rather than keep B's read.
        let before = std::fs::read(&state).unwrap();
        let standing = aterm_secrets::client::state_standing(&paths).unwrap();
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut log = LocalLog::of(&paths);
        log.read = Box::new({
            let (paths, before, reads) = (paths.clone(), before.clone(), Arc::clone(&reads));
            move || {
                if reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    bump_state(&paths, "requested");
                    assert_ne!(
                        aterm_secrets::client::state_standing(&paths).unwrap(),
                        standing,
                        "PRECONDITION: B is held while the actual reader runs"
                    );
                    let keys = acknowledged_member_keys(&paths);
                    aterm_secrets::fsx::write_atomic(&paths.dir.join("state.toml"), &before, 0o600)
                        .unwrap();
                    assert_eq!(
                        aterm_secrets::client::state_standing(&paths).unwrap(),
                        standing,
                        "PRECONDITION: A is restored before the reader returns"
                    );
                    assert!(
                        matches!(&keys, Ok(Some(keys)) if keys.unread().is_none()),
                        "PRECONDITION: the native reader answered under B"
                    );
                    keys
                } else {
                    acknowledged_member_keys(&paths)
                }
            }
        });
        let full = log
            .keys()
            .expect("a conservative verdict after the moved read");
        assert_eq!(std::fs::read(&state).unwrap(), before);
        let read = log.keys().expect("A read again");
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a full A to B to A inside the reader is read again"
        );
        assert!(
            partial.unread().is_some(),
            "a moved read grants no new verdict"
        );
        assert!(full.unread().is_some(), "a full ABA grants no new verdict");
        assert!(read.unread().is_none(), "a settled native read answered");
        assert!(Arc::ptr_eq(&read, &log.keys().expect("the held verdict")));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);

        // Absence also has a physical interval: a state file can come and go while its
        // semantic stamps both say None. Once the absent state is read without a write,
        // that healthy absence can be kept too.
        std::fs::remove_file(&state).unwrap();
        assert!(
            aterm_secrets::client::state_standing(&paths)
                .unwrap()
                .is_none()
        );
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut log = LocalLog::of(&paths);
        log.read = Box::new({
            let (paths, before, reads) = (paths.clone(), before.clone(), Arc::clone(&reads));
            move || {
                if reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    let state = paths.dir.join("state.toml");
                    aterm_secrets::fsx::write_atomic(&state, &before, 0o600).unwrap();
                    let keys = acknowledged_member_keys(&paths);
                    std::fs::remove_file(&state).unwrap();
                    assert!(
                        matches!(&keys, Ok(Some(keys)) if keys.unread().is_none()),
                        "PRECONDITION: the native reader answered while the file existed"
                    );
                    keys
                } else {
                    acknowledged_member_keys(&paths)
                }
            }
        });
        let appeared = log
            .keys()
            .expect("a conservative verdict after the file came and went");
        assert!(
            aterm_secrets::client::state_standing(&paths)
                .unwrap()
                .is_none()
        );
        let absent = log.keys().expect("the absent state read again");
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a state file that came and went is read again"
        );
        assert!(
            appeared.unread().is_some(),
            "the moved read grants no new verdict"
        );
        assert!(
            absent.unread().is_none(),
            "a settled absent-state read answered"
        );
        assert!(Arc::ptr_eq(&absent, &log.keys().expect("the held verdict")));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
        aterm_secrets::fsx::write_atomic(&state, &before, 0o600).unwrap();
        running.stop();
    }

    #[test]
    fn ensure_local_node_key_in_log_binds_founder_and_retires_rotated_key() {
        let root = aterm_tempfile::tempdir().unwrap();
        let server_dir = root.path().join("server");
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let running = aterm_secrets::server::start(
            socket,
            &aterm_secrets::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: server_dir,
                advertise: None,
                pins: Vec::new(),
                hold: std::time::Duration::from_millis(50),
                keep_awake: false,
            },
        )
        .unwrap();
        let url = format!("http://{}/v1/test", running.addr);
        let home = root.path().join("m22-home");
        let paths = aterm_secrets::client::Paths::for_home(&home);
        let created = aterm_secrets::client::create(&paths, &url, None, Some("m22")).unwrap();
        aterm_secrets::client::admin_seed_swarm_roster(&paths, &[]).unwrap();
        aterm_secrets::client::seed_vault_genesis(&paths, &root.path().join("test.genesis"))
            .unwrap();
        aterm_secrets::client::admin_mirror_log(&paths, &[]).unwrap();

        let local_log = LocalLog::of(&paths);
        let node_1 = NodeIdentity::from_seed(&"a1".repeat(16), &[0x11; 32]).unwrap();
        let node_2 = NodeIdentity::from_seed(&"a1".repeat(16), &[0x22; 32]).unwrap();

        // 1. Founder starts in Genesis v2 with `node_key: None`.
        let initial_keys = local_log.keys().expect("seeded log");
        assert!(!initial_keys.admits(&node_1.key()));

        // 2. First call publishes `Body::Node` for `node_1`; second call is a no-op.
        assert!(ensure_local_node_key_in_log(
            &paths,
            &node_1,
            Some(&initial_keys)
        ));
        let keys_after_1 = local_log.keys().expect("keys after node_1");
        assert!(keys_after_1.admits(&node_1.key()));
        assert!(!ensure_local_node_key_in_log(
            &paths,
            &node_1,
            Some(&keys_after_1)
        ));

        let roster_1 = aterm_secrets::client::swarm_roster_until(
            &paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            roster_1
                .member_by_label_or_alias("m22")
                .and_then(|m| m.node_key.as_deref()),
            Some(node_1.key().hex().as_str())
        );

        // 3. Rotating to `node_2` admits `node_2` and retires `node_1` (`Standing::Out`).
        assert!(ensure_local_node_key_in_log(
            &paths,
            &node_2,
            Some(&keys_after_1)
        ));
        let keys_after_2 = local_log.keys().expect("keys after node_2");
        assert!(keys_after_2.admits(&node_2.key()));
        assert!(keys_after_2.holds_out(&node_1.key()));

        let roster_2 = aterm_secrets::client::swarm_roster_until(
            &paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            roster_2
                .member_by_label_or_alias("m22")
                .and_then(|m| m.node_key.as_deref()),
            Some(node_2.key().hex().as_str())
        );

        // Independent members keep roster denial from pre-removing the Everyone-removal case.
        let admit_member = |label: &str, node: &NodeIdentity| {
            let member_home = root.path().join(format!("{label}-home"));
            let member_paths = aterm_secrets::client::Paths::for_home(&member_home);
            let _ = aterm_secrets::client::machine_identity(&member_paths).unwrap();
            let device = aterm_secrets::commons::bootstrap::Device::open(&member_paths)
                .unwrap()
                .unwrap();
            let store = aterm_secrets::client::load_records(&paths).unwrap();
            let log = store.log().unwrap();
            let aterm_secrets::membership_log::Body::Genesis { root: log_root, .. } =
                log.genesis().body()
            else {
                panic!("expected Genesis");
            };
            let request = device
                .request(log_root.network, log_root.authority)
                .unwrap();
            let admitted = aterm_secrets::client::admit_by_phrase(
                &paths,
                &request,
                *node.key().bytes(),
                label,
                1_000,
            )
            .unwrap();
            assert!(matches!(
                admitted,
                aterm_secrets::client::ByLog::Admitted(_)
            ));
            let _ = aterm_secrets::client::post_records_to_servers(&paths).unwrap();
            member_paths
        };
        let node_7 = NodeIdentity::from_seed(&"b2".repeat(16), &[0x33; 32]).unwrap();
        let m7_paths = admit_member("m7", &node_7);
        let keys_with_m7 = local_log.keys().expect("keys with m7");
        assert!(keys_with_m7.admits(&node_7.key()));

        // Changing the control entry's audience is not a membership revocation.
        // Omitted wraps can be repaired; LocalLog must still admit m7.
        let admin = aterm_secrets::client::admin_open(&paths).unwrap();
        let mut doc_commons_only = admin.doc.clone();
        let ctrl = doc_commons_only
            .entries()
            .into_iter()
            .find(|e| e.name == aterm_secrets::commons::ENTRY)
            .expect("commons control entry");
        doc_commons_only
            .unset_entry(&ctrl.audience, aterm_secrets::commons::ENTRY)
            .unwrap();
        doc_commons_only
            .set_entry(
                &aterm_secrets::text::Audience::Hosts(vec!["m22".to_owned()]),
                aterm_secrets::commons::ENTRY,
                ctrl.value.as_slice(),
            )
            .unwrap();
        aterm_secrets::client::admin_publish(&paths, &admin, &doc_commons_only).unwrap();
        assert!(
            local_log
                .keys()
                .expect("keys after commons narrowed")
                .admits(&node_7.key()),
            "a control-entry audience change does not revoke a fleet member"
        );

        // Roster denial is independent: it may already prune that member's machine line.
        let node_8 = NodeIdentity::from_seed(&"d4".repeat(16), &[0x66; 32]).unwrap();
        let _m8_paths = admit_member("m8", &node_8);
        assert!(
            local_log
                .keys()
                .expect("keys with m8")
                .admits(&node_8.key()),
            "roster-denial control starts admitted"
        );
        let records_with_m8 = std::fs::read(paths.records_file()).unwrap();
        let admitted_store = aterm_secrets::client::load_records(&paths).unwrap();
        let vault_with_m8 = std::fs::read(paths.dir.join("vault")).unwrap();
        let state_with_m8 = std::fs::read(paths.dir.join("state.toml")).unwrap();
        let admin = aterm_secrets::client::admin_open(&paths).unwrap();
        let mut doc_roster_deny = admin.doc.clone();
        let mut r = aterm_secrets::swarm_roster::SwarmRoster::from_document(&doc_roster_deny)
            .unwrap()
            .unwrap_or_default();
        r.deny_labels.push("m8".to_owned());
        doc_roster_deny
            .set_entry(
                &aterm_secrets::text::Audience::Everyone,
                aterm_secrets::SWARM_ROSTER_ENTRY,
                r.to_toml().as_bytes(),
            )
            .unwrap();
        aterm_secrets::client::admin_publish(&paths, &admin, &doc_roster_deny).unwrap();
        assert!(
            local_log
                .keys()
                .expect("keys after roster deny")
                .holds_out(&node_8.key()),
            "member denied in aterm-swarm-roster must be held out by LocalLog"
        );

        // Putting m8's admitted snapshot back is a rollback: the history this Mac acknowledged
        // is ahead of it, so the store's reader refuses it, and LocalLog keeps the verdict it
        // last read rather than admitting the denied member again.
        let records_after_deny = std::fs::read(paths.records_file()).unwrap();
        let vault_with_deny = std::fs::read(paths.dir.join("vault")).unwrap();
        let state_with_deny = std::fs::read(paths.dir.join("state.toml")).unwrap();
        aterm_secrets::fsx::write_atomic(&paths.records_file(), &records_with_m8, 0o600).unwrap();
        aterm_secrets::fsx::write_atomic(&paths.dir.join("vault"), &vault_with_m8, 0o600).unwrap();
        aterm_secrets::fsx::write_atomic(&paths.dir.join("state.toml"), &state_with_m8, 0o600)
            .unwrap();
        let error = aterm_secrets::client::local_swarm_roster_for_log(
            &paths,
            admitted_store.log().unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            aterm_secrets::ErrorKind::MembershipHistoryUnavailable
        );
        assert!(
            local_log
                .keys()
                .expect("keys over the older snapshot")
                .holds_out(&node_8.key()),
            "an older snapshot put back never admits a member the log held out"
        );
        aterm_secrets::fsx::write_atomic(&paths.records_file(), &records_after_deny, 0o600)
            .unwrap();
        aterm_secrets::fsx::write_atomic(&paths.dir.join("vault"), &vault_with_deny, 0o600)
            .unwrap();
        aterm_secrets::fsx::write_atomic(&paths.dir.join("state.toml"), &state_with_deny, 0o600)
            .unwrap();
        assert!(
            local_log
                .keys()
                .expect("keys after restoring the deny")
                .holds_out(&node_8.key())
        );

        // Explicitly retire m7 before removing its vault recipient. An omitted
        // wrap is repairable, while a signed retirement must hold node_7 out.
        let admin = aterm_secrets::client::admin_open(&paths).unwrap();
        let doc = &admin.doc;
        assert!(
            doc.machines().iter().any(|m| m.name == "m7"),
            "the revocation control starts with its machine present"
        );
        let roster = aterm_secrets::swarm_roster::SwarmRoster::from_document(doc)
            .unwrap()
            .unwrap();
        assert!(
            !roster.deny_labels.iter().any(|label| label == "m7"),
            "the independent roster-denial case must not deny m7"
        );
        assert!(
            local_log
                .keys()
                .expect("m7 before removal")
                .admits(&node_7.key()),
            "the revocation control starts admitted"
        );
        let revoked = aterm_secrets::client::admin_revoke(&paths, &admin, "m7", false).unwrap();
        assert!(revoked.retirement.is_some_and(|retired| retired.changed));
        let keys_after_m7_out = local_log.keys().expect("keys after m7 excluded");
        assert!(
            keys_after_m7_out.holds_out(&node_7.key()),
            "a retired member must be held out by LocalLog"
        );
        let pinned_url = format!("{url}#{}", created.signer);
        aterm_secrets::client::configure_log_member(&m7_paths, &pinned_url, "m7").unwrap();
        aterm_secrets::client::sync_records(&m7_paths, None).unwrap();
        assert!(
            !ensure_local_node_key_in_log(&m7_paths, &node_7, None),
            "a retired member must not author Body::Node"
        );

        let now = 1_760_000_000_000_u64;
        let node_unproved = NodeIdentity::from_seed(&"c3".repeat(16), &[0x44; 32]).unwrap();
        let mut book = Vec::new();
        let served = [
            aterm_net::fleet_member::Peer::own(&node_2, ("m22.local", 7100, "0.146.0"), now),
            aterm_net::fleet_member::Peer::own(&node_unproved, ("m3.local", 7100, "0.146.0"), now),
            aterm_net::fleet_member::Peer::own(&node_1, ("m22-old.local", 7100, "0.146.0"), now),
            aterm_net::fleet_member::Peer::own(&node_7, ("m7.local", 7100, "0.146.0"), now),
        ];
        assert_eq!(
            aterm_net::fleet_member::merge_signed(
                &mut book,
                &served,
                (None, now),
                (
                    Some(&keys_after_m7_out),
                    &[],
                    &aterm_net::fleet_member::Keyring::default(),
                ),
            ),
            2
        );
        assert_eq!(
            book.iter().filter_map(|p| p.key).collect::<Vec<_>>(),
            vec![node_2.key(), node_unproved.key()]
        );

        // A pre-log/unproved member whose 64-hex node key appears only in `aterm-swarm-roster`
        // (and is denied there) is also held out by `log_member_keys_with_roster`.
        let store_now = aterm_secrets::client::load_records(&paths).unwrap();
        let log_now = store_now.log().unwrap();
        let mut r_unproved = aterm_secrets::swarm_roster::SwarmRoster::default();
        r_unproved.deny_nodes.push(node_unproved.key().hex());
        let keys_with_roster_deny = log_member_keys_with_roster(log_now, Some(&r_unproved));
        assert!(keys_with_roster_deny.holds_out(&node_unproved.key()));

        // A stale denied row in `r.members` for the same recipient must not shadow a newer active
        // row for that recipient (either for a log-bound member `m22` or a pre-log member).
        let m22_rec = aterm_secrets::client::machine_identity(&paths)
            .unwrap()
            .recipient()
            .to_bech32();
        let mut r_readmitted = aterm_secrets::swarm_roster::SwarmRoster {
            deny_labels: vec!["m22-old".to_owned(), "m3-old".to_owned()],
            deny_nodes: Vec::new(),
            members: vec![
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m22-old".to_owned(),
                    vault_name: "m22-old".to_owned(),
                    recipient: m22_rec.clone(),
                    node_id: Some("a1".repeat(16)),
                    node_key: Some(node_2.key().hex()),
                    admitted_ms: 100,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m22".to_owned(),
                    vault_name: "m22".to_owned(),
                    recipient: m22_rec,
                    node_id: Some("a1".repeat(16)),
                    node_key: Some(node_2.key().hex()),
                    admitted_ms: 200,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m3-old".to_owned(),
                    vault_name: "m3-old".to_owned(),
                    recipient: "age1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq96l3dp"
                        .to_owned(),
                    node_id: Some("c3".repeat(16)),
                    node_key: Some(node_unproved.key().hex()),
                    admitted_ms: 100,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m3".to_owned(),
                    vault_name: "m3".to_owned(),
                    recipient: "age1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq96l3dp"
                        .to_owned(),
                    node_id: Some("c3".repeat(16)),
                    node_key: Some(node_unproved.key().hex()),
                    admitted_ms: 200,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
            ],
        };
        let keys_readmitted = log_member_keys_with_roster(log_now, Some(&r_readmitted));
        assert!(
            keys_readmitted.admits(&node_2.key()),
            "active row for m22 must keep node_2 admitted despite stale denied row"
        );
        assert!(
            !keys_readmitted.holds_out(&node_unproved.key()),
            "active pre-log row for m3 must prevent stale denied row from holding out node_unproved"
        );
        r_readmitted.deny_labels.push("m22".to_owned());
        r_readmitted.deny_labels.push("m3".to_owned());
        let keys_all_denied = log_member_keys_with_roster(log_now, Some(&r_readmitted));
        assert!(keys_all_denied.holds_out(&node_2.key()));
        assert!(keys_all_denied.holds_out(&node_unproved.key()));

        // A member renumbered in the roster (e.g. `label = "m70"`) whose original log `label`/`name`
        // (`"m22"`) is in `deny_labels` must still be admitted while its roster row `m70` is active.
        r_readmitted.members[1].label = "m70".to_owned();
        r_readmitted.members[1].vault_name = "m22-2".to_owned();
        let keys_renumbered = log_member_keys_with_roster(log_now, Some(&r_readmitted));
        assert!(
            keys_renumbered.admits(&node_2.key()),
            "renumbered active roster row m70 must not be held out by denied log label/name m22"
        );
        r_readmitted.deny_labels.push("m70".to_owned());
        let keys_renumbered_denied = log_member_keys_with_roster(log_now, Some(&r_readmitted));
        assert!(keys_renumbered_denied.holds_out(&node_2.key()));

        // A roster row whose recipient bound a newer node key in the log or was left out of
        // `EpochAudience::Everyone` holds out its stale/pre-log `node_key` even without `deny_labels`.
        let node_0 = NodeIdentity::from_seed(&"a1".repeat(16), &[0x05; 32]).unwrap();
        let node_7_pre = NodeIdentity::from_seed(&"b2".repeat(16), &[0x55; 32]).unwrap();
        assert!(
            ![
                node_1.key(),
                node_2.key(),
                node_7.key(),
                node_unproved.key(),
                node_0.key(),
                node_7_pre.key(),
            ]
            .contains(&node_8.key()),
            "roster denial must have a key distinct from every other control"
        );
        let m7_rec = aterm_secrets::client::machine_identity(&m7_paths)
            .unwrap()
            .recipient()
            .to_bech32();
        let r_stale_and_left_out = aterm_secrets::swarm_roster::SwarmRoster {
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
            members: vec![
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m22".to_owned(),
                    vault_name: "m22".to_owned(),
                    recipient: aterm_secrets::client::machine_identity(&paths)
                        .unwrap()
                        .recipient()
                        .to_bech32(),
                    node_id: Some("a1".repeat(16)),
                    node_key: Some(node_0.key().hex()),
                    admitted_ms: 50,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
                aterm_secrets::swarm_roster::SwarmMember {
                    label: "m7".to_owned(),
                    vault_name: "m7".to_owned(),
                    recipient: m7_rec,
                    node_id: Some("b2".repeat(16)),
                    node_key: Some(node_7_pre.key().hex()),
                    admitted_ms: 50,
                    hint: None,
                    hw_hint: None,
                    claimed_label: None,
                    claimed_at_ms: None,
                    aliases: Vec::new(),
                },
            ],
        };
        let keys_stale_and_left_out =
            log_member_keys_with_roster(log_now, Some(&r_stale_and_left_out));
        assert!(keys_stale_and_left_out.holds_out(&node_0.key()));
        assert!(keys_stale_and_left_out.holds_out(&node_7_pre.key()));

        // A crossed source also fences this small local file. The same warm reader must
        // follow the selected data, observe another rotation, and keep its verdict through
        // missing custody, under which a fresh reader admits no key at all.
        aterm_secrets::fsx::write_atomic(
            &paths.records_file().with_extension("crossed"),
            b"crossed\n",
            0o600,
        )
        .unwrap();
        let node_3 = NodeIdentity::from_seed(&"a1".repeat(16), &[0x77; 32]).unwrap();
        assert!(ensure_local_node_key_in_log(
            &paths,
            &node_3,
            Some(&keys_after_m7_out)
        ));
        assert!(paths.records_file().is_dir());
        let keys_after_3 = local_log.keys().expect("keys in the fenced layout");
        assert!(keys_after_3.admits(&node_3.key()));
        assert!(keys_after_3.holds_out(&node_2.key()));
        let node_4 = NodeIdentity::from_seed(&"a1".repeat(16), &[0x88; 32]).unwrap();
        assert!(ensure_local_node_key_in_log(
            &paths,
            &node_4,
            Some(&keys_after_3)
        ));
        let keys_after_4 = local_log.keys().expect("keys after fenced rotation");
        assert!(keys_after_4.admits(&node_4.key()));
        assert!(keys_after_4.holds_out(&node_3.key()));
        std::fs::rename(
            paths.records_data_file().unwrap(),
            paths.records_file().join("saved-current"),
        )
        .unwrap();
        let kept = local_log
            .keys()
            .expect("a store that cannot be read keeps the verdict last read from it");
        assert!(kept.admits(&node_4.key()) && kept.holds_out(&node_3.key()));
        let fresh = LocalLog::of(&paths)
            .keys()
            .expect("a vault that cannot be read now is no Mac without a log");
        assert!(
            !fresh.admits(&node_4.key()) && fresh.holds_out(&node_4.key()),
            "with no verdict to keep, the book admits no key the log might hold out"
        );

        running.stop();
    }
}
