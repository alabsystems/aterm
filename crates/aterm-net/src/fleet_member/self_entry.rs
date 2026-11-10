// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Durable versions of this node's signed fleet descriptor, independent of clock steps.

use super::*;

/// This identity's last published or observed self-signed descriptor. A leave keeps this
/// beside the node key: rejoining a fleet must not reuse an earlier descriptor version.
pub(super) const SELF_ENTRY_FILE: &str = "fleet-self-entry";

/// This node's descriptor for `endpoint`, committed before it can be published. Unchanged
/// descriptors keep their version; a changed endpoint, version or optional primary name gets
/// a version above every locally known valid descriptor for this node and key. The clock is
/// only an initial floor. `seen_ms` is the current observation time, never that version.
///
/// A descriptor published before this record existed can only supply a migration floor if a
/// local book or a later authenticated book read still holds it.
/// # Errors
/// The persisted identity no longer matches `me`, private state cannot be read or committed,
/// the endpoint is invalid, or a changed descriptor would exhaust its version counter.
pub fn self_entry(dir: &Path, me: &NodeIdentity, endpoint: (&str, u16, &str)) -> io::Result<Peer> {
    self_entry_at(dir, me, endpoint, unix_ms_now().unwrap_or_default())
}

/// [`self_entry`] for a listener generation until its stop (`ended`) is set. The stop is checked
/// under the book lock before allocating or committing a descriptor, including after waiting
/// for another writer. An ended generation returns `None` without changing the durable record.
/// # Errors
/// As [`self_entry`].
pub fn self_entry_while(
    dir: &Path,
    me: &NodeIdentity,
    endpoint: (&str, u16, &str),
    ended: &Stop,
) -> io::Result<Option<Peer>> {
    self_entry_if(dir, me, endpoint, unix_ms_now().unwrap_or_default(), || {
        !ended.is_set()
    })
}

pub(super) fn self_entry_at(
    dir: &Path,
    me: &NodeIdentity,
    (host, port, version): (&str, u16, &str),
    now_ms: u64,
) -> io::Result<Peer> {
    self_entry_if(dir, me, (host, port, version), now_ms, || true)?
        .ok_or_else(|| invalid("local fleet publication was canceled"))
}

fn self_entry_if(
    dir: &Path,
    me: &NodeIdentity,
    (host, port, version): (&str, u16, &str),
    now_ms: u64,
    current: impl FnOnce() -> bool,
) -> io::Result<Option<Peer>> {
    let _lock = BookLock::take(dir, lock_wait())?;
    // The generation may end while this worker waits for the book. Checking here orders
    // publication against the next generation's version allocation under this same lock.
    if !current() {
        return Ok(None);
    }
    check_identity(dir, me)?;
    if !is_host(host) || port == 0 || version_word(version).is_none() {
        return Err(invalid("invalid local fleet endpoint"));
    }
    let judged = aterm_uds::private_file::JudgedDir::judge(dir)
        .map_err(|why| io::Error::other(format!("{SELF_ENTRY_FILE}: {why}")))?;
    let cached = read_self_entry(&judged)?;
    let latest = local_floor(&judged, me, cached.as_ref())?;
    // Optional naming remains optional even when its file is unavailable or malformed.
    let name = read_primary_name(dir).ok().flatten();
    let mut peer = if let Some(known) = latest.as_ref().filter(|known| {
        known.host == host
            && known.port == port
            && known.version.as_deref() == Some(version)
            && known.primary_name() == name.as_deref()
    }) {
        canonical(known.clone())
    } else {
        let seq = match latest.as_ref().and_then(|peer| peer.signed) {
            Some(signed) => now_ms.max(
                signed
                    .seq
                    .checked_add(1)
                    .ok_or_else(|| invalid("local fleet descriptor version exhausted"))?,
            ),
            None => now_ms,
        };
        canonical(Peer::own(me, (host, port, version), seq).with_primary_name(me, name.as_deref()))
    };
    commit_if_changed(dir, cached.as_ref(), &peer)?;
    peer.seen_ms = Some(now_ms);
    Ok(Some(peer))
}

impl Registration {
    /// Sign this registration through the durable local descriptor writer ([`self_entry`]).
    /// # Errors
    /// The registration names another node, or [`self_entry`] cannot commit its descriptor.
    pub fn signed_in(self, dir: &Path, me: &NodeIdentity) -> io::Result<Self> {
        if self.node != me.node() {
            return Err(invalid("local fleet registration names another node"));
        }
        let peer = self_entry(dir, me, (&self.host, self.port, &self.version))?;
        Ok(Self {
            key: peer.key,
            signed: peer.signed,
            ..self
        })
    }

    pub(super) fn signed_in_while(
        self,
        dir: &Path,
        me: &NodeIdentity,
        ended: &Stop,
    ) -> io::Result<Option<Self>> {
        if self.node != me.node() {
            return Err(invalid("local fleet registration names another node"));
        }
        let Some(peer) = self_entry_while(dir, me, (&self.host, self.port, &self.version), ended)?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            key: peer.key,
            signed: peer.signed,
            ..self
        }))
    }
}

impl HeldBook<'_> {
    /// Remember a valid higher version of this identity returned in an authenticated book,
    /// before normal merging drops the reader's own row. This already holds the book lock.
    pub(super) fn observe_self_entries(
        &self,
        me: &NodeIdentity,
        entries: impl Iterator<Item = Peer>,
    ) -> io::Result<()> {
        check_identity(self.dir, me)?;
        let judged = aterm_uds::private_file::JudgedDir::judge(self.dir)
            .map_err(|why| io::Error::other(format!("{SELF_ENTRY_FILE}: {why}")))?;
        let cached = read_self_entry(&judged)?;
        let latest = newest(me, cached.as_ref().into_iter().cloned().chain(entries));
        if let Some(latest) = latest {
            commit_if_changed(self.dir, cached.as_ref(), &canonical(latest))?;
        }
        Ok(())
    }
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

fn check_identity(dir: &Path, me: &NodeIdentity) -> io::Result<()> {
    if read_node_identity(dir)?
        .is_some_and(|stored| stored.node() == me.node() && stored.key() == me.key())
    {
        Ok(())
    } else {
        Err(invalid("local fleet identity changed before publication"))
    }
}

fn read_self_entry(judged: &aterm_uds::private_file::JudgedDir<'_>) -> io::Result<Option<Peer>> {
    let bytes = match judged.read(SELF_ENTRY_FILE, RECORD_MAX) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(None),
        Err(why) => return Err(io::Error::other(format!("{SELF_ENTRY_FILE}: {why}"))),
    };
    let peer = std::str::from_utf8(&bytes)
        .ok()
        .filter(|text| text.lines().count() == 1)
        .and_then(|text| Peer::parse(text.trim()))
        .filter(Peer::signature_holds)
        .ok_or_else(|| invalid("fleet-self-entry holds no valid signed descriptor"))?;
    Ok(Some(canonical(peer)))
}

fn local_floor(
    judged: &aterm_uds::private_file::JudgedDir<'_>,
    me: &NodeIdentity,
    cached: Option<&Peer>,
) -> io::Result<Option<Peer>> {
    let mut candidates: Vec<Peer> = cached.into_iter().cloned().collect();
    for file in [PEERS_FILE, ISSUER_BOOK_FILE] {
        let bytes = match judged.read(file, PEERS_FILE_MAX) {
            Ok(bytes) => bytes,
            Err(PrivateFileError::Missing) => continue,
            Err(why) => return Err(io::Error::other(format!("{file}: {why}"))),
        };
        candidates.extend(
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter_map(Peer::parse),
        );
    }
    Ok(newest(me, candidates.into_iter()))
}

fn newest(me: &NodeIdentity, candidates: impl Iterator<Item = Peer>) -> Option<Peer> {
    candidates
        .filter(|peer| {
            peer.node.as_deref() == Some(me.node())
                && peer.key == Some(me.key())
                && peer.signature_holds()
        })
        .max_by_key(|peer| peer.signed.map(|signed| signed.seq))
}

/// The descriptor as this node keeps it: what a copy observed of it cleared, and no name record.
/// Neither the entry signature nor the record's covers the other, so any member can append
/// record words to a copy of this node's entry at its current seq; the record this node
/// publishes comes from its own state alone (`docs/DESIGN-machine-names-2026-11-04.md` §3.2).
fn canonical(mut peer: Peer) -> Peer {
    peer.addr = None;
    peer.key_from = None;
    peer.seen_ms = None;
    peer.via = Via::Joined;
    peer.fleet = None;
    peer.signed = peer.signed.map(|signed| signed.with_record(None));
    peer
}

fn commit_if_changed(dir: &Path, cached: Option<&Peer>, peer: &Peer) -> io::Result<()> {
    if cached != Some(peer) {
        write_private(
            dir,
            SELF_ENTRY_FILE,
            format!("{}\n", peer.line()).as_bytes(),
        )?;
    }
    Ok(())
}
