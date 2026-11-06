// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One-mesh M4's waiting writes (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.2 M4, §11 A3 and
//! W3): the `[everyone]` writes a writer that holds a `Remove` keeps unsigned until the new key
//! arrives, the sets a publish keeps pending on its own version, the admin view's overlay of what
//! waits, the edit a later save drafts against that overlay, the pass that signs them, and the
//! journal of the ones a later change overtook.

use super::*;

/// One `[everyone]` write a writer keeps unsigned while it waits for the new key ([`Waiting`]).
#[derive(Clone)]
pub(in super::super) struct WaitingWrite {
    /// What the name becomes: an entry, or `None` where an edit takes back a name only this Mac's
    /// waiting writes added, which then stays absent, as the log holds it (where the log holds it
    /// after all, the take-back is overtaken, never signed as a `Delete`, [`sign_waiting_writes`]). An
    /// edit that takes out of `[everyone]` a name the log holds never waits: it mints its fresh
    /// epoch at once (`leaves_everyone` in [`draft_document_diff_with`]).
    to: Option<Entry>,
    /// What the log may hold at the name for this write to be signed over it, each as its
    /// [`entry_digest`] (`None`: no value): the log's value the draft read, the waiting write the
    /// view the edit was made from showed there, and the one that stood there when the draft read
    /// the log, and their own bases ([`everyone_edit`]). A name the log holds as none of these was changed by another
    /// write since, which this one would revert: it is overtaken ([`sign_waiting_writes`]).
    bases: BTreeSet<Option<[u8; 32]>>,
    /// A person's save of a name the log holds concurrent values for: a `Delete` goes first, so the
    /// `Put` replaces every sibling ([`Base::resolve`]).
    resolve: bool,
}

impl WaitingWrite {
    fn same(&self, other: &Self) -> bool {
        self.bases == other.bases
            && self.resolve == other.resolve
            && match (&self.to, &other.to) {
                (Some(a), Some(b)) => same_entry(a, b),
                (None, None) => true,
                _ => false,
            }
    }

    /// Whether the log holds the name as this write leaves it: `read` is its winning value there,
    /// `conflicted` whether concurrent values stand beside it (a person's save settles those).
    fn done(&self, read: Option<&Entry>, conflicted: bool) -> bool {
        match (&self.to, read) {
            (None, None) => true,
            (Some(to), Some(read)) => same_entry(read, to) && !(self.resolve && conflicted),
            _ => false,
        }
    }

    /// Whether the log's value `read` is one this write may be signed over ([`WaitingWrite::bases`]).
    fn stands_over(&self, read: Option<&Entry>) -> bool {
        self.bases.contains(&read.map(entry_digest))
    }
}

/// The `[everyone]` writes a writer that holds a `Remove` keeps unsigned until the new key arrives
/// (one-mesh M4: "writers wait for the new key"), by name. While a newest `[everyone]` epoch still
/// wraps a removed Mac ([`rotation_due`]), a writer signs no `[everyone]` `Put`: sealed under that
/// epoch the value would reach the removed Mac, and a fresh epoch of its own would cost the log
/// an epoch and a `Wrap` a seat beside the rotation already due. An edit that takes a name out of
/// `[everyone]`, or moves one into it from a host list, mints that fresh epoch anyway, and so
/// writes at once. The remover's own batch, or the ranked tender's rotation or coalesce, brings
/// the new epoch, which the 8,500 stop never holds; a later vault pass then signs each write under
/// it ([`sign_waiting_writes`]).
///
/// A publish keeps them pending, bound to the digest of the envelope that carries them, from
/// before that envelope goes out ([`wait_for_key`]), and they wait in this Mac's
/// `everyone.waiting` only once a server took that version ([`land_waiting`]): the publish's own
/// answer, or a sync, a mirror, an admin's open or a later publish that reads that version, or one
/// naming it as a parent, from a server later (a reply lost after the store). A version no server
/// took never lands them, so a publish the person saw fail never lands its edit afterwards; one
/// every server refused, or every server it may have reached now refuses for good, drops them
/// ([`forget_waiting`], [`land_published`]). Every file is sealed to this Mac's own age
/// recipient, as its lockers are. The envelope still carries the edit to the Macs that read its
/// lockers; it is stamped with the log's heads, which hold none of the waiting writes, so a Mac
/// that reads the log reads them once they are signed, and no import of that version's older text
/// ever reaches the log.
///
/// The admin view shows the ones that still stand over the log's value ([`standing`]), and a
/// later save drafts against that view, so a person can change or take back a write that waits
/// ([`everyone_edit`]).
#[derive(Clone, Default)]
pub(in super::super) struct Waiting(BTreeMap<String, WaitingWrite>);

impl Waiting {
    /// Whether nothing waits.
    pub(in super::super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The names, in order.
    pub(in super::super) fn names(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    /// Whether the write of `name` settles its conflict ([`WaitingWrite::resolve`]).
    pub(in super::super) fn resolves(&self, name: &str) -> bool {
        self.0.get(name).is_some_and(|write| write.resolve)
    }

    /// The plain text of these writes, one line each: `put <name> <resolve> <bases> <if-absent>
    /// <anchor> v<value>`, or `keep <name> <bases>` for a name taken back to absent; `<bases>` is
    /// each base, `-` for none or its hex digest, joined by `+`, a missing anchor `-`, the value in unpadded base64. Names and anchors hold no whitespace.
    fn encode(&self) -> zeroize::Zeroizing<Vec<u8>> {
        let mut text = zeroize::Zeroizing::new(String::new());
        for (name, write) in &self.0 {
            let bases = write
                .bases
                .iter()
                .map(|base| {
                    base.map_or_else(|| "-".to_owned(), |d| aterm_codec::hex::encode_lower(&d))
                })
                .collect::<Vec<_>>()
                .join("+");
            let Some(entry) = &write.to else {
                text.push_str(&format!("keep {name} {bases}\n"));
                continue;
            };
            let resolve = u8::from(write.resolve);
            text.push_str(&format!(
                "put {name} {resolve} {bases} {} {} v",
                u8::from(entry.if_absent),
                entry.anchor.as_deref().unwrap_or("-"),
            ));
            text.push_str(&crate::b64::encode_nopad(entry.value.as_slice()));
            text.push('\n');
        }
        zeroize::Zeroizing::new(text.as_bytes().to_vec())
    }

    /// [`Waiting::encode`]'s text read back; a line of another shape is an error.
    fn decode(plain: &[u8]) -> crate::Result<Self> {
        let bad = || Error::new("everyone.waiting holds a line of no known shape");
        let text = std::str::from_utf8(plain).map_err(|_| bad())?;
        let flag = |word: &str| match word {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(bad()),
        };
        let bases = |word: &str| -> crate::Result<BTreeSet<Option<[u8; 32]>>> {
            word.split('+')
                .map(|base| {
                    if base == "-" {
                        return Ok(None);
                    }
                    let mut digest = [0u8; 32];
                    (base.len() == 64
                        && aterm_codec::hex::decode_into(base.as_bytes(), &mut digest))
                    .then_some(Some(digest))
                    .ok_or_else(bad)
                })
                .collect()
        };
        let mut writes = BTreeMap::new();
        for line in text.lines() {
            let words: Vec<&str> = line.split_ascii_whitespace().collect();
            let (name, write) = match words.as_slice() {
                ["put", name, resolve, base, if_absent, anchor, value] => {
                    let value = value
                        .strip_prefix('v')
                        .and_then(crate::b64::decode_nopad)
                        .ok_or_else(bad)?;
                    let entry = Entry {
                        name: (*name).to_owned(),
                        value: SecretBytes::from(value),
                        audience: Audience::Everyone,
                        if_absent: flag(if_absent)?,
                        anchor: (*anchor != "-").then(|| (*anchor).to_owned()),
                    };
                    let write = WaitingWrite {
                        to: Some(entry),
                        bases: bases(base)?,
                        resolve: flag(resolve)?,
                    };
                    (*name, write)
                }
                ["keep", name, base] => {
                    let write = WaitingWrite {
                        to: None,
                        bases: bases(base)?,
                        resolve: false,
                    };
                    (*name, write)
                }
                _ => return Err(bad()),
            };
            writes.insert(name.to_owned(), write);
        }
        Ok(Self(writes))
    }
}

/// A digest of `entry` as [`same_entry`] compares two of one name: its flags, anchor and value.
fn entry_digest(entry: &Entry) -> [u8; 32] {
    let mut buf = zeroize::Zeroizing::new(b"aterm/everyone-waiting/from/1\0".to_vec());
    buf.push(u8::from(entry.if_absent));
    let anchor = entry.anchor.as_deref().unwrap_or_default();
    buf.extend_from_slice(
        &u64::try_from(anchor.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    buf.extend_from_slice(anchor.as_bytes());
    buf.extend_from_slice(entry.value.as_slice());
    Sha256::digest(&buf)
}

/// Where [`Waiting`] keeps its writes, sealed to this Mac's own age recipient.
pub(in super::super) fn waiting_file(paths: &Paths) -> PathBuf {
    paths.dir.join("everyone.waiting")
}

/// The writes waiting in [`waiting_file`], opened with `identity`; none where there is no file.
///
/// # Errors
/// The file cannot be read, does not open with `identity`, or holds a line of no known shape.
pub(super) fn read_waiting(paths: &Paths, identity: &Identity) -> crate::Result<Waiting> {
    let Some(sealed) = fsx::read_private(
        &waiting_file(paths),
        crate::membership_log::MAX_RECORDS_STREAM,
    )?
    else {
        return Ok(Waiting::default());
    };
    let plain = age::decrypt(std::slice::from_ref(identity), sealed.as_slice())
        .map_err(|error| Error::new(format!("everyone.waiting: {error}")))?;
    Waiting::decode(plain.as_slice())
}

/// Write `waiting` to [`waiting_file`], sealed to `identity`'s recipient; remove the file where
/// nothing waits.
///
/// # Errors
/// The writes cannot be sealed, or the file cannot be written or removed.
fn write_waiting(paths: &Paths, identity: &Identity, waiting: &Waiting) -> crate::Result<()> {
    let path = waiting_file(paths);
    if waiting.is_empty() {
        return remove_if_there(&path);
    }
    let sealed = age::encrypt(&[identity.recipient()], waiting.encode().as_slice())?;
    fsx::write_atomic(&path, &sealed, 0o600)
}

/// Remove the file at `path`; one that is not there is removed already.
fn remove_if_there(path: &Path) -> crate::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(Error::new(format!("{}: {error}", path.display())))
        }
        _ => Ok(()),
    }
}

/// The state lock a change of the waiting or pending writes runs under, taken by `deadline` where
/// one is given.
fn waiting_lock(paths: &Paths, deadline: Option<std::time::Instant>) -> crate::Result<StateLock> {
    match deadline {
        Some(deadline) => StateLock::take_at_until(paths, &paths.lock_file(), deadline),
        None => StateLock::take(paths),
    }
}

/// The lock a publish of this Mac holds shared from before its draft reads the log until its
/// pending set is written ([`claim_draft`]), and a vault pass's signing takes exclusive
/// ([`sign_waiting_writes`]).
fn claim_file(paths: &Paths) -> PathBuf {
    paths.dir.join(".everyone.claim")
}

/// A publish's claim on this Mac's waiting writes ([`claim_draft`]), released on drop, the
/// process's end included.
pub(in super::super) struct DraftClaim(Option<std::fs::File>);

impl Drop for DraftClaim {
    fn drop(&mut self) {
        if let Some(file) = &self.0 {
            let _ = file.unlock();
        }
    }
}

/// Claim this Mac's waiting writes for a publish, from before its draft reads the log until its
/// pending set is written (one-mesh W3): no vault pass signs one in between
/// ([`sign_waiting_writes`]). Several publishes claim at once; a pass that holds the claim lock
/// finishes first, within `deadline` (a minute where none is given).
///
/// # Errors
/// The lock cannot be opened, or a pass still holds it at the deadline.
pub(in super::super) fn claim_draft(
    paths: &Paths,
    deadline: Option<std::time::Instant>,
) -> crate::Result<DraftClaim> {
    if draft_unclaimed() {
        return Ok(DraftClaim(None));
    }
    let until =
        deadline.unwrap_or_else(|| std::time::Instant::now() + std::time::Duration::from_secs(60));
    let path = claim_file(paths);
    fsx::ensure_private_dir(&paths.dir)?;
    let file = fsx::open_lock(&path)?;
    let mut backoff = std::time::Duration::from_millis(1);
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "the shared take of a publish's claim, released by LOCK_UN (DraftClaim's Drop)"
        )]
        match file.try_lock_shared() {
            Ok(()) => return Ok(DraftClaim(Some(file))),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(Error::new(format!("{}: {error}", path.display())));
            }
        }
        let remaining = until.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(Error::new(
                "a vault pass still signed this Mac's waiting writes at the deadline: run it again",
            ));
        }
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(std::time::Duration::from_millis(50));
    }
}

/// A vault pass's take of the claim lock ([`claim_draft`]), at once: `None` while a publish holds
/// its claim, and the pass signs nothing now (a later pass does).
///
/// # Errors
/// The lock cannot be opened.
fn pass_claim(paths: &Paths) -> crate::Result<Option<DraftClaim>> {
    let path = claim_file(paths);
    fsx::ensure_private_dir(&paths.dir)?;
    let file = fsx::open_lock(&path)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "a pass's nonblocking take of the claim lock, released by LOCK_UN (DraftClaim's Drop)"
    )]
    match file.try_lock() {
        Ok(()) => Ok(Some(DraftClaim(Some(file)))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => {
            Err(Error::new(format!("{}: {error}", path.display())))
        }
    }
}

/// Where publishes keep their waiting writes until what became of the version that carries them
/// is known ([`wait_for_key`], [`land_waiting`]): one file per version, sealed to this Mac's own age
/// recipient, named `<order>-<seq>-<envelope digest>` (`order` puts a later publish's set after an
/// earlier one's, two publishes from one opened version included), holding the servers the
/// publish may have sent that version to and its writes.
///
/// A set goes only on proof, as a held draft does (one-mesh W2): it lands once a server is seen to
/// hold its version, or a descendant of it; it is dropped once every server refused its version,
/// or once each server it may have reached took a version of its sequence or above that does not
/// descend from it, and so refuses it as not newer for good ([`land_published`]). Nothing goes by
/// count. The bound that remains: one small file per publish of this Mac whose version no answer,
/// read head or later stored head has settled, each one edit's `[everyone]` writes (at most the
/// vault's text). A publish whose deadline ends before its version goes out drops its set
/// ([`forget_unsent`]). What can stay for
/// good is the set of a publish whose every answer was lost and whose servers are the ALab name,
/// which moves between the Macs that serve it, or a server never heard again: it stays until a
/// head read back lands it, as such a held draft stays, and while it stays it holds back this
/// Mac's earlier waiting write of each name it writes ([`sign_waiting_writes`]), which a view
/// shows waiting, until a later edit of the name lands over it.
fn pending_dir(paths: &Paths) -> PathBuf {
    paths.dir.join("everyone.pending")
}

/// What a pending set's plain text starts with. Then come the servers its publish may have sent
/// its version to, one per line, then an empty line, then its writes ([`Waiting::encode`]).
const PENDING_MAGIC: &[u8] = b"aterm/everyone-pending/1\n";

/// One pending set ([`pending_dir`]).
struct Pending {
    /// Its place among the sets: a later publish's set has a higher one.
    order: u64,
    /// The sequence of its version.
    seq: u64,
    /// The digest of the envelope that carries it.
    version: [u8; 32],
    /// The servers its publish may have sent that version to, or [`REACH_UNKNOWN`] alone.
    reach: Vec<String>,
    writes: Waiting,
}

impl Pending {
    fn file(&self, paths: &Paths) -> PathBuf {
        pending_dir(paths).join(format!(
            "{:020}-{:020}-{}",
            self.order,
            self.seq,
            aterm_codec::hex::encode_lower(&self.version)
        ))
    }
}

/// Whether a publish left writes pending ([`wait_for_key`]) that nothing has settled yet.
pub(in super::super) fn waiting_pending(paths: &Paths) -> bool {
    std::fs::read_dir(pending_dir(paths)).is_ok_and(|mut dir| dir.next().is_some())
}

/// A pending set's file as its name reads ([`pending_files`]): its order, sequence, version and
/// path.
type PendingFile = (u64, u64, [u8; 32], PathBuf);

/// Every pending set's file ([`pending_dir`]) by its name alone, in no order. A name of another
/// shape (an atomic write's temporary) is no set.
///
/// # Errors
/// The directory cannot be read.
fn pending_files(paths: &Paths) -> crate::Result<Vec<PendingFile>> {
    let dir = pending_dir(paths);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::new(format!("{}: {error}", dir.display()))),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| Error::new(format!("{}: {error}", dir.display())))?;
        if let Some((order, seq, version)) = entry.file_name().to_str().and_then(pending_name) {
            files.push((order, seq, version, entry.path()));
        }
    }
    Ok(files)
}

/// Why a pending set's file at `path` is refused: it does not open with this Mac's key, or is of
/// no known shape. It is never read as no set: it may be the writes of a version a server stored.
fn unopened(path: &Path) -> Error {
    Error::new(format!(
        "{}: a pending set of this Mac's [everyone] writes does not open with its key; while it \
         is there nothing waiting is signed or landed, and no publish that keeps an [everyone] \
         write goes (moving it out of everyone.pending drops the writes it holds)",
        path.display()
    ))
}

/// Every pending set ([`pending_dir`]), oldest first. The caller holds the state lock.
///
/// # Errors
/// The directory or a file in it cannot be read, or a file does not open with `identity` or is
/// of no known shape ([`unopened`]): what this Mac cannot read of its pending writes is never
/// read as none (one-mesh W3).
fn read_pending(paths: &Paths, identity: &Identity) -> crate::Result<Vec<Pending>> {
    let mut sets = Vec::new();
    for (order, seq, version, path) in pending_files(paths)? {
        let Some(sealed) = fsx::read_private(&path, crate::membership_log::MAX_RECORDS_STREAM)?
        else {
            continue;
        };
        match open_pending(identity, sealed.as_slice()) {
            Some((reach, writes)) => sets.push(Pending {
                order,
                seq,
                version,
                reach,
                writes,
            }),
            // The negative control: a set that does not open reads as none, as before W3.
            None if unread_as_none() => remove_if_there(&path)?,
            None => return Err(unopened(&path)),
        }
    }
    sets.sort_by_key(|set| (set.order, set.seq, set.version));
    Ok(sets)
}

/// The order, sequence and version a pending set's file name holds ([`Pending::file`]).
fn pending_name(name: &str) -> Option<(u64, u64, [u8; 32])> {
    let mut parts = name.splitn(3, '-');
    let (order, seq, digest) = (parts.next()?, parts.next()?, parts.next()?);
    let number = |word: &str| {
        (word.len() == 20 && word.bytes().all(|b| b.is_ascii_digit()))
            .then(|| word.parse().ok())
            .flatten()
    };
    let mut version = [0u8; 32];
    (digest.len() == 64 && aterm_codec::hex::decode_into(digest.as_bytes(), &mut version))
        .then_some((number(order)?, number(seq)?, version))
}

/// A pending set's servers and writes, opened from its sealed file with `identity`; `None` where it
/// does not open or is of no known shape.
fn open_pending(identity: &Identity, sealed: &[u8]) -> Option<(Vec<String>, Waiting)> {
    let plain = age::decrypt(std::slice::from_ref(identity), sealed).ok()?;
    let rest = plain.as_slice().strip_prefix(PENDING_MAGIC)?;
    let end = rest.windows(2).position(|pair| pair == b"\n\n")?;
    let head = std::str::from_utf8(&rest[..end]).ok()?;
    let reach = head.lines().map(str::to_owned).collect();
    let writes = Waiting::decode(&rest[end + 2..]).ok()?;
    Some((reach, writes))
}

/// Write `set` to its file ([`Pending::file`]), sealed to `identity`'s recipient; remove it where
/// it holds no write. The caller holds the state lock. The first set flushes the vault folder
/// that holds the new directory, as a held draft's does (`hold_drafted`): a power loss after the
/// PUT never leaves the set in a directory the folder lost.
///
/// # Errors
/// The directory or the file cannot be written or flushed, or the set cannot be sealed.
fn write_pending(paths: &Paths, identity: &Identity, set: &Pending) -> crate::Result<()> {
    if set.writes.is_empty() {
        return remove_if_there(&set.file(paths));
    }
    let unwritable = |url: &String| url.is_empty() || url == REACH_UNKNOWN || url.contains('\n');
    let unknown = [REACH_UNKNOWN.to_owned()];
    let reach = if set.reach.is_empty() || set.reach.iter().any(unwritable) {
        &unknown[..]
    } else {
        &set.reach[..]
    };
    let mut plain = zeroize::Zeroizing::new(PENDING_MAGIC.to_vec());
    for url in reach {
        plain.extend_from_slice(url.as_bytes());
        plain.push(b'\n');
    }
    plain.push(b'\n');
    plain.extend_from_slice(set.writes.encode().as_slice());
    let sealed = age::encrypt(&[identity.recipient()], plain.as_slice())?;
    let made = !pending_dir(paths).exists();
    fsx::ensure_private_dir(&pending_dir(paths))?;
    if made {
        aterm_durable::durable_dir(&paths.dir).map_err(|e| Error::new(e.to_string()))?;
    }
    fsx::write_atomic(&set.file(paths), &sealed, 0o600)
}

/// The names the pending sets write ([`pending_dir`]), read without changing a file, so under a
/// shared observation of the state: a waiting write of one of them waits for that set
/// ([`sign_waiting_writes`]). `None` where a set cannot be read: any name may be one it writes.
fn pending_names(paths: &Paths, identity: &Identity) -> Option<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for (_, _, _, path) in pending_files(paths).ok()? {
        let sealed = match fsx::read_private(&path, crate::membership_log::MAX_RECORDS_STREAM) {
            Ok(Some(sealed)) => sealed,
            Ok(None) => continue,
            Err(_) if unread_as_none() => continue,
            Err(_) => return None,
        };
        match open_pending(identity, sealed.as_slice()) {
            Some((_, writes)) => names.extend(writes.0.into_keys()),
            None if unread_as_none() => {}
            None => return None,
        }
    }
    Some(names)
}

/// The files of this Mac's waiting and pending writes that it cannot read, as their paths: the
/// waiting writes ([`waiting_file`]), and each pending set ([`pending_dir`]). While one is there,
/// nothing waiting is signed or landed over it. While the waiting writes do not read, no edit that
/// writes `[everyone]` is drafted; while a set does not read, no publish keeps a write pending.
pub(in super::super) fn unread_waiting(
    paths: &Paths,
    identity: &Identity,
) -> (Option<String>, Vec<String>) {
    let waiting = read_waiting(paths, identity)
        .is_err()
        .then(|| waiting_file(paths).display().to_string());
    let mut unread = Vec::new();
    for (_, _, _, path) in pending_files(paths).unwrap_or_default() {
        let opens = fsx::read_private(&path, crate::membership_log::MAX_RECORDS_STREAM).is_ok_and(
            |sealed| {
                sealed.is_none_or(|sealed| open_pending(identity, sealed.as_slice()).is_some())
            },
        );
        if !opens {
            unread.push(path.display().to_string());
        }
    }
    (waiting, unread)
}

/// Keep a publish's `[everyone]` writes pending for its version, of sequence `seq` and envelope
/// digest `version`, before that envelope goes out, beside `reach`: every server the publish may
/// send it to (the vault's file, for a vault on a path). A version a server stores with them is
/// never left without them, whatever becomes of the reply ([`land_waiting_held`]).
///
/// First, each pending set of a version among `stored` lands: the version this publish builds on
/// and its parents, which a server holds. That covers an earlier publish whose answer said it
/// was stored but whose landing missed the lock, and an earlier one a later version, read in its
/// place, builds on.
///
/// Every other earlier set stays beside this one, whatever their number ([`pending_dir`]'s
/// bound): one of them may be on a server after all, its reply lost. That server then refuses
/// this version as not newer, and a later sync lands that set.
///
/// # Errors
/// The state lock is not taken by `deadline`, or the pending or waiting writes cannot be read,
/// sealed or written; where `waiting` holds a write, a pending set that does not open
/// ([`read_pending`]) refuses too.
#[expect(
    clippy::too_many_arguments,
    reason = "the version's sequence, digest and servers beside the writes and what is stored"
)]
pub(in super::super) fn wait_for_key(
    paths: &Paths,
    identity: &Identity,
    waiting: &Waiting,
    seq: u64,
    version: &[u8; 32],
    reach: &[String],
    stored: &[[u8; 32]],
    deadline: Option<std::time::Instant>,
) -> crate::Result<()> {
    if waiting.is_empty() && !waiting_pending(paths) {
        return Ok(());
    }
    let _lock = waiting_lock(paths, deadline)?;
    let mut sets = match read_pending(paths, identity) {
        Ok(sets) => sets,
        // A publish that keeps nothing goes (a removal never waits on this): the sets land at a
        // later read, once each one opens.
        Err(_) if waiting.is_empty() => return Ok(()),
        // One that keeps writes refuses before its version goes out: a set this Mac cannot read
        // may be a stored version's edit, and the person sees why rather than lose it.
        Err(error) => return Err(error),
    };
    land_sets(paths, identity, &mut sets, stored)?;
    if waiting.is_empty() {
        return Ok(());
    }
    for set in sets.iter().filter(|set| set.version == *version) {
        remove_if_there(&set.file(paths))?;
    }
    let order = sets
        .iter()
        .map(|set| set.order.saturating_add(1))
        .max()
        .unwrap_or(0);
    #[cfg(any(test, debug_assertions))]
    if pending_by_count() {
        // The negative control: A3's bound, the oldest past eight dropped whatever its version.
        let kept: Vec<&Pending> = sets.iter().filter(|set| set.version != *version).collect();
        for set in kept.iter().take((kept.len() + 1).saturating_sub(8)) {
            remove_if_there(&set.file(paths))?;
        }
    }
    let set = Pending {
        order,
        seq,
        version: *version,
        reach: reach.to_vec(),
        writes: waiting.clone(),
    };
    write_pending(paths, identity, &set)
}

/// [`land_waiting_held`], taking this Mac's state lock by `deadline` where one is given.
///
/// # Errors
/// As [`land_waiting_held`], or the state lock is not taken by `deadline`.
pub(in super::super) fn land_waiting(
    paths: &Paths,
    identity: &Identity,
    versions: &[[u8; 32]],
    deadline: Option<std::time::Instant>,
) -> crate::Result<()> {
    if !waiting_pending(paths) {
        return Ok(());
    }
    let _lock = waiting_lock(paths, deadline)?;
    land_waiting_held(paths, identity, versions)
}

/// A server holds each version whose envelope digest is in `versions`: a version it took or
/// served, and the parents that version names. The writes pending for each ([`wait_for_key`])
/// now wait in [`waiting_file`] for the new key ([`land_sets`]). Sets of other versions stay
/// pending. The caller holds the state lock.
///
/// # Errors
/// The pending or waiting writes cannot be read or written.
pub(in super::super) fn land_waiting_held(
    paths: &Paths,
    identity: &Identity,
    versions: &[[u8; 32]],
) -> crate::Result<()> {
    if !waiting_pending(paths) {
        return Ok(());
    }
    let mut sets = read_pending(paths, identity)?;
    land_sets(paths, identity, &mut sets, versions)
}

/// A publish's version of sequence `seq` and envelope digest `version` landed: `took` names every
/// server that stored it, and `parents` its parent spine. Its set and its parents' land
/// ([`land_sets`]); then, as a landed head settles the held drafts (one-mesh W2), every other set
/// of sequence `seq` or below loses from its servers each server of `took` that names one server,
/// which holds this version or a newer one from now on and so refuses that set's version as not
/// newer for good, and a set with no server left is dropped: its version is on no server and never
/// will be. A set of a higher sequence stays, and so does one whose servers are unknown or name the
/// ALab name ([`names_one_server`]). Best effort, under the state lock by `deadline`: where it is
/// not taken, a later read of this version lands the set and nothing is dropped.
pub(in super::super) fn land_published(
    paths: &Paths,
    identity: &Identity,
    seq: u64,
    version: &[u8; 32],
    parents: &[[u8; 32]],
    took: &[String],
    deadline: Option<std::time::Instant>,
) {
    if !waiting_pending(paths) {
        return;
    }
    let Ok(_lock) = waiting_lock(paths, deadline) else {
        return;
    };
    let Ok(mut sets) = read_pending(paths, identity) else {
        return;
    };
    let stored: Vec<[u8; 32]> = std::iter::once(*version)
        .chain(parents.iter().copied())
        .collect();
    if land_sets(paths, identity, &mut sets, &stored).is_err() {
        return;
    }
    for set in &mut sets {
        // The held drafts' gate and reach rule, no_lost_edit's kernels (one-mesh P3): the sets the
        // spine names landed above, so none descends here.
        if nle::Gate::of(false, set.seq, seq, !took.is_empty()) != nle::Gate::Reach {
            continue;
        }
        let reach: Option<Vec<(bool, bool)>> = (!set.reach.iter().any(|url| url == REACH_UNKNOWN))
            .then(|| {
                set.reach
                    .iter()
                    .map(|url| (names_one_server(url), took.contains(url)))
                    .collect()
            });
        match nle::settle_reach(reach.as_deref()) {
            nle::Settle::Goes => {
                let _ = remove_if_there(&set.file(paths));
            }
            nle::Settle::Stays => {}
            nle::Settle::Narrows(left) => {
                set.reach = left.iter().map(|&i| set.reach[i].clone()).collect();
                let _ = write_pending(paths, identity, set);
            }
        }
    }
}

/// Land each of `sets` (oldest first) whose version is in `stored`, in order: its writes wait in
/// [`waiting_file`] over what waits already. Each name it writes also leaves every earlier
/// pending set. A person's later write of a name replaces an earlier one's whichever version is
/// read first, and two servers that took different versions of one sequence still land the
/// later edit. A set left with no write is dropped. The caller holds the state lock; `sets` is
/// left as the files now stand.
///
/// # Errors
/// The waiting or pending writes cannot be read or written.
fn land_sets(
    paths: &Paths,
    identity: &Identity,
    sets: &mut Vec<Pending>,
    stored: &[[u8; 32]],
) -> crate::Result<()> {
    // Which sets land, which pending writes stay and which set's write of each name waits is
    // no_lost_edit's `land` (one-mesh P3), whose island proves no write of a stored version lost.
    let mut names: Vec<String> = Vec::new();
    let shape: Vec<(bool, Vec<usize>)> = sets
        .iter()
        .map(|set| {
            let writes = set
                .writes
                .0
                .keys()
                .map(|name| match names.iter().position(|known| known == name) {
                    Some(at) => at,
                    None => {
                        names.push(name.clone());
                        names.len() - 1
                    }
                })
                .collect();
            (stored.contains(&set.version), writes)
        })
        .collect();
    let shape_view: Vec<(bool, &[usize])> = shape
        .iter()
        .map(|(lands, writes)| (*lands, writes.as_slice()))
        .collect();
    let landing = nle::land(&shape_view, names.len());
    for (at, set) in sets.iter_mut().enumerate() {
        if landing.lands[at] {
            continue;
        }
        let leaves: Vec<&String> = shape[at]
            .1
            .iter()
            .zip(&landing.keeps[at])
            .filter(|&(_, &keeps)| !keeps)
            .map(|(&name, _)| &names[name])
            .collect();
        if !leaves.is_empty() {
            set.writes.0.retain(|name, _| !leaves.contains(&name));
            write_pending(paths, identity, set)?;
        }
    }
    if landing.lands.iter().any(|&lands| lands) {
        let mut held = read_waiting(paths, identity)?;
        for (name, at) in names.iter().zip(&landing.waits) {
            if let Some(at) = at
                && let Some(write) = sets[*at].writes.0.get(name)
            {
                held.0.insert(name.clone(), write.clone());
            }
        }
        write_waiting(paths, identity, &held)?;
    }
    let mut landed = Vec::new();
    for (set, &lands) in std::mem::take(sets).into_iter().zip(&landing.lands) {
        if lands {
            landed.push(set);
        } else if !set.writes.is_empty() {
            sets.push(set);
        }
    }
    for set in &landed {
        remove_if_there(&set.file(paths))?;
    }
    Ok(())
}

/// Drop the writes pending for `version` ([`wait_for_key`]): every server refused that version,
/// so, as its records, they never enter the log. Other versions' sets stay pending. Found by file
/// name, so a set that does not open goes too: every server refused it. Best effort, as
/// [`forget_held_draft`].
pub(in super::super) fn forget_waiting(paths: &Paths, version: &[u8; 32]) {
    if !waiting_pending(paths) {
        return;
    }
    let Ok(_lock) = StateLock::take(paths) else {
        return;
    };
    for (_, _, held, path) in pending_files(paths).unwrap_or_default() {
        if held == *version {
            let _ = remove_if_there(&path);
        }
    }
}

/// Drop the writes pending for `version`, a version no server was sent (its publish's deadline
/// ended before its first PUT), where the state lock is free now: a deadline that ended waits on
/// nothing more. Left in place, a set whose servers are the ALab name would never settle, and
/// would hold back this Mac's earlier waiting write of each name it writes ([`pending_dir`]).
pub(in super::super) fn forget_unsent(paths: &Paths, version: &[u8; 32]) {
    if !waiting_pending(paths) {
        return;
    }
    let Ok(Some(_lock)) = StateLock::try_take(paths) else {
        return;
    };
    for (_, _, held, path) in pending_files(paths).unwrap_or_default() {
        if held == *version {
            let _ = remove_if_there(&path);
        }
    }
}

/// The digests of `envelope` and of the parents it names: versions a server holds once it took
/// or served `envelope` ([`land_waiting_held`]).
pub(in super::super) fn stored_versions(envelope: &crate::envelope::Envelope) -> Vec<[u8; 32]> {
    std::iter::once(envelope.digest())
        .chain(envelope.parents.iter().copied())
        .collect()
}

/// The writes of `held` that still stand over `log`, the log's `[everyone]` entries as this Mac
/// reads them now (`conflicts` its conflicted registers): each whose bases hold the log's value
/// and that the log does not hold yet. A write the log no longer holds as any of its bases was
/// overtaken and goes at the next pass ([`sign_waiting_writes`]); one it holds is done.
pub(in super::super) fn standing(
    held: &Waiting,
    log: Option<&BTreeMap<String, Entry>>,
    conflicts: &BTreeSet<(Audience, String)>,
) -> Waiting {
    Waiting(
        held.0
            .iter()
            .filter(|(name, write)| {
                let read = log.and_then(|log| log.get(*name));
                let conflicted = conflicts.contains(&(Audience::Everyone, (*name).clone()));
                write.stands_over(read) && !write.done(read, conflicted)
            })
            .map(|(name, write)| (name.clone(), write.clone()))
            .collect(),
    )
}

/// What the admin view overlays: this Mac's waiting writes ([`waiting_file`]) that still stand
/// over `log` as `identity` opens it ([`standing`]), after the sets pending on `stored` (the
/// version an admin opened and its parents, which a server holds) land, where `land` and the
/// state lock is free now. A cached open passes no `land`: its callers hold the state lock, or an
/// author's own observation of it, which an exclusive take on this thread would wait on for good;
/// the sets land at a later open or sync. None where nothing waits, the writes cannot be read, or
/// `log` does not open.
pub(in super::super) fn view_waiting(
    paths: &Paths,
    identity: &Identity,
    log: &Log,
    stored: &[[u8; 32]],
    land: bool,
) -> Waiting {
    #[cfg(any(test, debug_assertions))]
    if view_without_waiting() {
        return Waiting::default();
    }
    if land
        && waiting_pending(paths)
        && let Ok(Some(_lock)) = StateLock::try_take(paths)
    {
        let _ = land_waiting_held(paths, identity, stored);
    }
    let Ok(held) = read_waiting(paths, identity) else {
        return Waiting::default();
    };
    if held.is_empty() {
        return held;
    }
    let Ok(Some(opened)) = log.open_audience_entries(identity) else {
        return Waiting::default();
    };
    standing(
        &held,
        opened.by_audience.get(&Audience::Everyone),
        &opened.conflicts,
    )
}

/// Overlay `waiting` onto `doc`'s `[everyone]`: each name as its write leaves it.
///
/// # Errors
/// The text refuses an entry.
pub(in super::super) fn overlay_waiting_onto_document(
    doc: &mut Document,
    waiting: &Waiting,
) -> crate::Result<()> {
    // Unanchored entries first, so an anchor exists before an entry names it.
    for anchored in [false, true] {
        for (name, write) in &waiting.0 {
            match &write.to {
                Some(entry) if entry.anchor.is_some() == anchored => doc
                    .set_entry_with(
                        &Audience::Everyone,
                        name,
                        entry.value.as_slice(),
                        entry.if_absent,
                        entry.anchor.as_deref(),
                    )
                    .map_err(|e| Error::new(e.to_string()))?,
                None if !anchored => {
                    doc.unset_entry(&Audience::Everyone, name)
                        .map_err(|e| Error::new(e.to_string()))?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// What an edit makes of `[everyone]`, against the view it was made from ([`everyone_edit`]).
pub(in super::super) struct EveryoneEdit {
    /// The entries the edit leaves there: its own, except that a name the view showed with a
    /// waiting write, which the edit left as the view showed it, follows what stands there now:
    /// the write that waits now, or the log's value where that write was signed or overtaken
    /// since the view was read.
    pub(in super::super) entries: BTreeMap<String, Entry>,
    /// Each name the edit writes, as a [`WaitingWrite`]: what waits for the new key where a
    /// `Remove` here waits for its rotation.
    pub(in super::super) writes: Waiting,
    /// Of `writes`, those that settle a waiting write of this Mac where nothing waits: each name
    /// the view showed one for, and each the edit changes over one that stands now. They land with
    /// the version, which signs their values itself, so the write each replaces is done there
    /// rather than read as overtaken by this Mac's own later edit.
    pub(in super::super) settles: Waiting,
    /// Of `writes`, the names each changes from the log's value: what a view shows waiting while
    /// they wait. A take-back is done once it lands and is never shown.
    pub(in super::super) shows: Vec<String>,
}

/// The `[everyone]` edit a draft makes (one-mesh M4, W3): `log` is the log's entries there as the
/// draft reads them (the edit's base text, where this Mac opens none of the newest epochs),
/// `view` the waiting writes the admin view the edit was made from showed (`None` for an edit made
/// from no such view: a mirror, or an import of a version's text), `now` the ones that stand now
/// ([`standing`], read with `log` under one observation of the state), and `doc` the edit's
/// entries there.
///
/// The edit is drafted against the view, the log's value overlaid with what waited: a name it
/// leaves as the view showed it keeps what stands now. Every other name whose value it leaves
/// otherwise than the log holds it (or whose conflict a person's save settles, `resolving`) is
/// written, and so is each name whose waiting write the view showed and the edit takes back to
/// the log's value, as a write of that value (a `keep` where the name was absent). A name left as
/// the view showed it keeps the meaning of the write that stands now: its `resolve`, so a save of
/// another name never turns a waiting settle of a conflict into a plain write that is done. A
/// write's bases are the log's value, the waiting write the view showed and the one that stands
/// now, and their own bases: it is signed over any of them, so neither this Mac's own earlier
/// write, signed after the view or the draft read it, nor a pass racing the draft reads the newer
/// edit as overtaken, and a write over the text's value, where this Mac opens none of the newest
/// epochs and reads that text as the log's, still stands over the value its earlier write did.
pub(in super::super) fn everyone_edit(
    log: Option<&BTreeMap<String, Entry>>,
    view: Option<&Waiting>,
    now: &Waiting,
    doc: Option<&BTreeMap<String, Entry>>,
    resolving: impl Fn(&String) -> bool,
) -> EveryoneEdit {
    let none = BTreeMap::new();
    let log = log.unwrap_or(&none);
    let unseen = Waiting::default();
    let shown = view.unwrap_or(&unseen);
    let mut entries = doc.cloned().unwrap_or_default();
    // The write each name left as the view showed it takes its value from.
    let mut kept: BTreeMap<&String, &WaitingWrite> = BTreeMap::new();
    for (name, was) in &shown.0 {
        let left = match (entries.get(name), &was.to) {
            (Some(have), Some(to)) => same_entry(have, to),
            (None, None) => true,
            _ => false,
        };
        if !left {
            continue;
        }
        let stands = match now.0.get(name) {
            Some(write) => {
                kept.insert(name, write);
                write.to.clone()
            }
            None => log.get(name).cloned(),
        };
        match stands {
            Some(entry) => {
                entries.insert(name.clone(), entry);
            }
            None => {
                entries.remove(name);
            }
        }
    }
    let mut writes = BTreeMap::new();
    let mut settles = BTreeMap::new();
    let mut shows = Vec::new();
    let names: BTreeSet<&String> = if names_unmerged() {
        entries.keys().collect()
    } else {
        entries
            .keys()
            .chain(shown.0.keys())
            .chain(now.0.keys())
            .collect()
    };
    for name in names {
        let to = entries.get(name);
        let read = log.get(name);
        let resolve =
            to.is_some() && (resolving(name) || kept.get(name).is_some_and(|write| write.resolve));
        let changed = resolve
            || match (to, read) {
                (Some(to), Some(read)) => !same_entry(to, read),
                (None, None) => false,
                _ => true,
            };
        let was = shown.0.get(name);
        let stands = now.0.get(name);
        // A take-back: the view showed a write here that stands now, and the edit leaves the name
        // as the log holds it.
        let taken_back = !changed && was.is_some() && stands.is_some();
        if !changed && !taken_back {
            continue;
        }
        let mut bases: BTreeSet<Option<[u8; 32]>> = BTreeSet::from([read.map(entry_digest)]);
        if !bases_log_only() {
            for write in was.into_iter().chain(stands) {
                bases.insert(write.to.as_ref().map(entry_digest));
                bases.extend(write.bases.iter().copied());
            }
        }
        if changed {
            shows.push(name.clone());
        }
        let write = WaitingWrite {
            to: to.cloned(),
            bases,
            resolve,
        };
        if was.is_some() || (view.is_some() && stands.is_some()) {
            settles.insert(name.clone(), write.clone());
        }
        writes.insert(name.clone(), write);
    }
    EveryoneEdit {
        entries,
        writes: Waiting(writes),
        settles: Waiting(settles),
        shows,
    }
}

/// Remove from `held` each write of `done` that still waits there as `done` names it.
fn settle(held: &mut Waiting, done: &Waiting) {
    for (name, write) in &done.0 {
        if held.0.get(name).is_some_and(|kept| kept.same(write)) {
            held.0.remove(name);
        }
    }
}

/// One vault pass's waiting writes ([`Waiting`]): once no newest `[everyone]` epoch wraps a Mac
/// the log removed, sign each `[everyone]` write waiting here, as the edit would have signed it,
/// under the epoch a writer seals under ([`sealing_epoch`]), and forget it. A write whose name
/// the log holds just as it would leave it is done already (a pass cut short after its commit
/// signs nothing twice). A write whose name another write changed since the edit read it, to a
/// value none of its bases hold (the rotation's own cascade among them, a removed Mac's deny rows
/// or a new fleet code), is dropped, so a value the edit never saw stands: signed now, after it,
/// it would revert it. A take-back (a `keep`) that finds the name held, by any write, is dropped
/// too: a pass signs no `Delete`, since a name that leaves `[everyone]` takes a fresh epoch that
/// reseals what stays (F15), which only an edit's draft mints, and the person's next edit removes
/// it that way. A publish's claim ([`claim_draft`]) keeps this Mac's own earlier write from being
/// signed between the draft of a take-back and its pending set. Each dropped write leaves a record
/// in this Mac's journal of overtaken writes ([`overtaken_file`]): the entry, the op that holds it
/// now and when the write was dropped, which the admin view shows once. A record, never an alert.
/// A write whose name a pending set of this Mac writes ([`pending_names`]) is left as it is: that
/// set is this Mac's own later edit of the name, which lands over it or goes and leaves it, and
/// is never read as a change that overtook it. Nothing is signed while a publish of this Mac holds
/// its claim, from before its draft reads the log until its pending set is written
/// ([`claim_draft`]): a write signed there would land under a set the pass could not see yet, a
/// value the person took back would be published, and a change whose records sign its own value
/// would meet a concurrent `Put` of the one it replaced. Nothing is signed where this Mac does not open the newest
/// epoch. Where a newest epoch was minted by, or wraps, a Mac the log or roster denies, the writes
/// go under a fresh epoch, as a writer's departure does: minted after every head, it is wrapped to
/// the Macs the newest epochs wrap and to the current seats (each seat a batch still arriving may
/// wrap among them), less each one the log removed or the log or roster denies, so neither
/// concurrent heads nor a batch still arriving hold it. Otherwise the writes reuse the epoch a
/// writer seals under ([`sealing_epoch`]): the one newest epoch, or the lowest of secure twins.
/// Nothing is signed where there is none: newest epochs concurrent with distinct recipient sets
/// (the tender coalesces them), or a batch still arriving that may wrap a Mac outside the current
/// seats and outside those only that batch can decide (a Mac with no seat it may still wrap).
/// Returns what it signed, already in this Mac's records, nothing posted.
///
/// # Errors
/// The waiting writes or local state cannot be read or written, or a record fails to sign.
pub(in super::super) fn sign_waiting_writes(
    paths: &Paths,
    now_ms: u64,
) -> crate::Result<Vec<Record>> {
    Ok(sign_waiting_writes_by(paths, now_ms, None)?.unwrap_or_default())
}

/// What a pass's draft settles ([`sign_waiting_writes_by`]): each waiting write it read, and of
/// them the ones it found overtaken.
type PassSettles = (Waiting, Vec<Overtaken>);

/// [`sign_waiting_writes`], its waits for the vault's locks ending at `deadline` where one is
/// given: `None` where a writer still held one then, which says nothing of what waits. Where the
/// records were committed and the state lock is not taken by then to settle what they signed,
/// the writes stay, done or overtaken, and the next pass settles and records them.
///
/// # Errors
/// As [`sign_waiting_writes`].
pub(in super::super) fn sign_waiting_writes_by(
    paths: &Paths,
    now_ms: u64,
    deadline: Option<std::time::Instant>,
) -> crate::Result<Option<Vec<Record>>> {
    let Some(identity) = super::super::read_identity(paths)? else {
        return Ok(Some(Vec::new()));
    };
    if read_waiting(paths, &identity)?.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let Some(_claim) = pass_claim(paths)? else {
        return Ok(Some(Vec::new()));
    };
    let draft = |store: &Store| -> crate::Result<(Vec<Record>, PassSettles)> {
        let none = || Ok((Vec::new(), (Waiting::default(), Vec::new())));
        let Some(mut log) = store.log().cloned() else {
            return none();
        };
        let Some((actor, device, incarnation)) = open_local_member_signer(paths, &log)? else {
            return none();
        };
        let Some(opened) = log.open_audience_entries(&identity)? else {
            return none();
        };
        let due = rotation_due(&log, &opened, &EpochAudience::Everyone);
        let waiting = read_waiting(paths, &identity)?;
        let newest = opened
            .maximal_epochs
            .get(&EpochAudience::Everyone)
            .cloned()
            .unwrap_or_default();
        let opens = !newest.is_empty() && opens_newest(&opened, &EpochAudience::Everyone);
        // A name a pending set of this Mac writes is this Mac's own later edit of it, in flight
        // or of a version whose fate is not known yet: its write here waits for that set, which
        // lands over it or goes and leaves it. Signed now, it would land under the later edit;
        // read as overtaken by it, the person's own newer edit would read as another's. A set
        // this Mac cannot read may write any name: then no write is signed or dropped while it is
        // there (one-mesh W3).
        let deferred = if race_undeferred() {
            Some(BTreeSet::new())
        } else {
            pending_names(paths, &identity)
        };
        // Whether the pass signs at all is no_lost_edit's kernel (one-mesh P3).
        if !nle::pass_signs(due, opens, deferred.is_some()) {
            return none();
        }
        let Some(deferred) = deferred else {
            return none();
        };
        let roster = roster_from_doc_or_opened(None, Some(&opened), Some(&log));
        let denied = |r: &Recipient| recipient_denied_by_roster_or_log(roster.as_ref(), &log, r);
        let membership = log.membership();
        let departed = newest.iter().any(|epoch| {
            log.get(epoch).is_some_and(|record| {
                membership.removal(record.actor()).is_some()
                    || membership
                        .get(record.actor())
                        .is_some_and(|minter| denied(&minter.binding.recipient()))
            }) || opened
                .wrapped_recipients
                .get(epoch)
                .is_some_and(|wrapped| wrapped.iter().any(&denied))
        });
        let current = log.current_recipients_where(&EpochAudience::Everyone, |seat| {
            roster
                .as_ref()
                .is_some_and(|r| member_denied_by_roster(r, seat))
        });
        let flight = Flight::of_reader(&log, &identity.recipient());
        // A departure that is no removal: a fresh epoch for the rest, as a writer's.
        let fresh_to: BTreeSet<Recipient> = if departed {
            newest
                .iter()
                .filter_map(|epoch| opened.wrapped_recipients.get(epoch))
                .flatten()
                .chain(&current)
                .filter(|r| !log.recipient_is_removed(r) && !denied(r))
                .copied()
                .collect()
        } else {
            BTreeSet::new()
        };
        let reuse = if departed {
            None
        } else {
            let Some(epoch) = sealing_epoch(
                &log,
                &opened,
                &newest,
                &identity,
                &current,
                flight.undecided(&EpochAudience::Everyone),
            ) else {
                return none();
            };
            let Some(key) = opened.unwrapped_keys.get(&epoch) else {
                return none();
            };
            Some((epoch, zeroize::Zeroizing::new(**key)))
        };
        let view = opened.by_audience.get(&Audience::Everyone);
        let mut settled = Waiting::default();
        let mut overtaken = Vec::new();
        let mut puts: Vec<&Entry> = Vec::new();
        let mut deletes: Vec<&String> = Vec::new();
        for (name, write) in &waiting.0 {
            let read = view.and_then(|view| view.get(name));
            let conflicted = opened
                .conflicts
                .contains(&(Audience::Everyone, name.clone()));
            // Each write's verdict is no_lost_edit's kernel (one-mesh P3). A take-back not done
            // here finds the name held: never signed as a `Delete` (F15).
            let verdict = nle::Verdict::of(
                deferred.contains(name),
                write.done(read, conflicted),
                write.stands_over(read),
                write.to.is_none(),
            );
            if verdict == nle::Verdict::Leaves {
                continue;
            }
            settled.0.insert(name.clone(), write.clone());
            match (verdict, &write.to) {
                (nle::Verdict::Overtaken, _) => overtaken.push(Overtaken {
                    name: name.clone(),
                    by: overtaker(&log, &opened, name),
                    dropped_ms: now_ms,
                    shown: false,
                }),
                (nle::Verdict::Signs, Some(to)) => {
                    if write.resolve {
                        deletes.push(name);
                    }
                    puts.push(to);
                }
                _ => {}
            }
        }
        let authored_ms = now_ms;
        let mut minted = Vec::new();
        device.with_seed(|writer_seed| -> crate::Result<()> {
            if puts.is_empty() && deletes.is_empty() {
                return Ok(());
            }
            let (epoch, key) = match reuse {
                Some(reused) => reused,
                None => {
                    let generation = opened
                        .max_generation
                        .get(&EpochAudience::Everyone)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(1)
                        .max(1);
                    let (draft, key) =
                        log.draft_epoch(&actor, authored_ms, EpochAudience::Everyone, generation)?;
                    let record = draft.sign(writer_seed, incarnation)?;
                    let epoch = *record.id();
                    let _ = log.insert(record.clone())?;
                    minted.push(record);
                    for &recipient in &fresh_to {
                        let record = log
                            .draft_wrap(&actor, authored_ms, epoch, &key, recipient)?
                            .sign(writer_seed, incarnation)?;
                        let _ = log.insert(record.clone())?;
                        minted.push(record);
                    }
                    (epoch, key)
                }
            };
            // A `Delete` before its `Put`: a person's save replaces every sibling.
            for name in &deletes {
                let record = log
                    .draft_delete(&actor, authored_ms, epoch, &key, name)?
                    .sign(writer_seed, incarnation)?;
                let _ = log.insert(record.clone())?;
                minted.push(record);
            }
            for entry in &puts {
                let record = log
                    .draft_put(&actor, authored_ms, epoch, &key, entry)?
                    .sign(writer_seed, incarnation)?;
                let _ = log.insert(record.clone())?;
                minted.push(record);
            }
            Ok(())
        })?;
        Ok((minted, (settled, overtaken)))
    };
    let authored = match deadline {
        Some(deadline) => author_and_commit_records_until(paths, deadline, draft)?,
        None => Some(author_and_commit_records(paths, draft)?),
    };
    let Some((minted, _, (settled, overtaken))) = authored else {
        return Ok(None);
    };
    if !settled.is_empty() {
        let _lock = match waiting_lock(paths, deadline) {
            Ok(lock) => lock,
            Err(_) if deadline.is_some() => return Ok(Some(minted)),
            Err(error) => return Err(error),
        };
        let mut held = read_waiting(paths, &identity)?;
        // Only a write still waiting as the pass read it was dropped: one a later landed set put
        // in its place was replaced by this Mac's own later edit, not overtaken.
        let dropped: Vec<Overtaken> = overtaken
            .into_iter()
            .filter(|record| {
                held.0
                    .get(&record.name)
                    .zip(settled.0.get(&record.name))
                    .is_some_and(|(kept, read)| kept.same(read))
            })
            .collect();
        settle(&mut held, &settled);
        write_waiting(paths, &identity, &held)?;
        #[cfg(any(test, debug_assertions))]
        if overtaken_unrecorded() {
            return Ok(Some(minted));
        }
        record_overtaken(paths, &dropped)?;
    }
    Ok(Some(minted))
}

/// The op that holds `name` in `[everyone]` now, as `opened` reads `log`: its winning `Put`, or the
/// `Delete` that removed it; `None` where the log names neither.
fn overtaker(
    log: &Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    name: &str,
) -> Option<Overtaker> {
    let key = (Audience::Everyone, name.to_owned());
    let (record, deleted) = match opened.winning_ops.get(&key) {
        Some(id) => (*id, false),
        None => (*opened.winning_deletes.get(&key)?, true),
    };
    let signed = log.get(&record)?;
    let label = log
        .membership()
        .label(signed.actor())
        .map_or_else(|| "a Mac".to_owned(), str::to_owned);
    Some(Overtaker {
        record,
        label,
        authored_ms: signed.authored_ms(),
        deleted,
    })
}

/// A waiting write of this Mac that a vault pass dropped because a later change overtook it
/// ([`sign_waiting_writes`]): one record of the journal ([`overtaken_file`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overtaken {
    /// The `[everyone]` entry the write was of.
    pub name: String,
    /// The op that holds the entry now; `None` where the log names none.
    pub by: Option<Overtaker>,
    /// When this Mac dropped the write (Unix milliseconds).
    pub dropped_ms: u64,
    /// Whether an admin view showed this record already ([`overtaken_seen`]).
    pub shown: bool,
}

/// The change that overtook a waiting write ([`Overtaken::by`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overtaker {
    /// Its record in the membership log.
    pub record: RecordId,
    /// Its signer's label, or `a Mac` where the log names none.
    pub label: String,
    /// When its signer authored it (Unix milliseconds).
    pub authored_ms: u64,
    /// Whether it removed the entry (a `Delete`) rather than wrote it.
    pub deleted: bool,
}

impl Overtaken {
    /// The record as a line a person reads, never a value: "shared: m7 wrote it at <when>, so this
    /// Mac's write of it that waited for a removal's new key was dropped at <when> (signed after,
    /// it would have reverted that)". `when` renders a Unix-millisecond time.
    pub fn line(&self, when: impl Fn(u64) -> String) -> String {
        let change = match &self.by {
            Some(by) => format!(
                "{} {} it {}",
                by.label,
                if by.deleted { "deleted" } else { "wrote" },
                when(by.authored_ms)
            ),
            None => "a later change took it".to_owned(),
        };
        format!(
            "{}: {change}, so this Mac's write of it that waited for a removal's new key was \
             dropped {} (signed after, it would have reverted that)",
            self.name,
            when(self.dropped_ms)
        )
    }
}

/// This Mac's journal of waiting writes a later change overtook ([`Overtaken`]), one per line,
/// tab-separated: when it was dropped, whether a view showed it, the entry, then the change
/// (`put` or `delete`, its record, its signer's label, when it was authored) or `-`. Names only,
/// never a value. The newest [`MAX_OVERTAKEN`] stay: a record a person may read, never an alert.
fn overtaken_file(paths: &Paths) -> PathBuf {
    paths.dir.join("everyone.overtaken")
}

/// How many records [`overtaken_file`] keeps, the newest.
const MAX_OVERTAKEN: usize = 64;

/// The journal of overtaken writes ([`overtaken_file`]), oldest first; a line of no known shape is
/// skipped. Empty where there is no journal.
///
/// # Errors
/// The journal exists and cannot be read.
pub(in super::super) fn read_overtaken(paths: &Paths) -> crate::Result<Vec<Overtaken>> {
    let Some(bytes) = fsx::read_private(&overtaken_file(paths), 1 << 20)? else {
        return Ok(Vec::new());
    };
    let text = String::from_utf8_lossy(bytes.as_slice()).into_owned();
    Ok(text.lines().filter_map(parse_overtaken).collect())
}

/// One line of [`overtaken_file`].
fn parse_overtaken(line: &str) -> Option<Overtaken> {
    let fields: Vec<&str> = line.split('\t').collect();
    let (dropped_ms, shown, name, change) = match fields.as_slice() {
        [dropped, shown, name, change @ ..] => {
            (dropped.parse().ok()?, *shown == "1", *name, change)
        }
        _ => return None,
    };
    let by = match change {
        ["-"] => None,
        [kind, record, label, authored] => {
            let mut id = [0u8; 32];
            if !aterm_codec::hex::decode_into(record.as_bytes(), &mut id) || record.len() != 64 {
                return None;
            }
            Some(Overtaker {
                record: id,
                label: (*label).to_owned(),
                authored_ms: authored.parse().ok()?,
                deleted: match *kind {
                    "delete" => true,
                    "put" => false,
                    _ => return None,
                },
            })
        }
        _ => return None,
    };
    Some(Overtaken {
        name: name.to_owned(),
        by,
        dropped_ms,
        shown,
    })
}

/// Write `records` as [`overtaken_file`], keeping the newest [`MAX_OVERTAKEN`]. The caller holds
/// the state lock.
fn write_overtaken(paths: &Paths, records: &[Overtaken]) -> crate::Result<()> {
    let keep = &records[records.len().saturating_sub(MAX_OVERTAKEN)..];
    if keep.is_empty() {
        return remove_if_there(&overtaken_file(paths));
    }
    let clean = |field: &str| field.replace(['\t', '\n'], " ");
    let mut text = String::new();
    for record in keep {
        let change = record.by.as_ref().map_or_else(
            || "-".to_owned(),
            |by| {
                format!(
                    "{}\t{}\t{}\t{}",
                    if by.deleted { "delete" } else { "put" },
                    aterm_codec::hex::encode_lower(&by.record),
                    clean(&by.label),
                    by.authored_ms
                )
            },
        );
        text.push_str(&format!(
            "{}\t{}\t{}\t{change}\n",
            record.dropped_ms,
            u8::from(record.shown),
            clean(&record.name)
        ));
    }
    fsx::write_atomic(&overtaken_file(paths), text.as_bytes(), 0o600)
}

/// Add `dropped` to the journal of overtaken writes. The caller holds the state lock.
///
/// # Errors
/// The journal cannot be read or written.
fn record_overtaken(paths: &Paths, dropped: &[Overtaken]) -> crate::Result<()> {
    if dropped.is_empty() {
        return Ok(());
    }
    let mut records = read_overtaken(paths)?;
    records.extend(dropped.iter().cloned());
    write_overtaken(paths, &records)
}

/// Mark each of `seen` shown in the journal of overtaken writes: an admin view showed it, and
/// shows it no more. Under the state lock.
///
/// # Errors
/// The state lock is not taken, or the journal cannot be read or written.
pub(in super::super) fn overtaken_seen(paths: &Paths, seen: &[Overtaken]) -> crate::Result<()> {
    if seen.is_empty() {
        return Ok(());
    }
    let _lock = StateLock::take(paths)?;
    let mut records = read_overtaken(paths)?;
    let mut changed = false;
    for record in &mut records {
        if !record.shown
            && seen
                .iter()
                .any(|s| s.name == record.name && s.dropped_ms == record.dropped_ms)
        {
            record.shown = true;
            changed = true;
        }
    }
    if changed {
        write_overtaken(paths, &records)?;
    }
    Ok(())
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// One-mesh W3's negative controls, each on for one test thread, or named for a whole lab
    /// process in `ATERM_TEST_NEGATIVE_CONTROL` ([`aterm_types::negative_control::on`]):
    /// `view-without-waiting`, the admin view shows the log alone, as before W3, so a save can
    /// neither see nor take back a write that waits; `bases-log-only`, a write's one base is the
    /// log's value the draft read, as before W3, so this Mac's own earlier write signed first
    /// overtakes the newer edit; `overtaken-unrecorded`, a dropped write leaves no record;
    /// `pending-by-count`, A3's count bound, the oldest pending set past eight dropped whatever
    /// its version; `moves-wait`, a name moved into `[everyone]` from a host list waits for the
    /// new key while the host list's `Delete` is signed at once, as before W3; `race-undeferred`,
    /// a vault pass signs or drops a waiting write whose name a pending set of this Mac writes,
    /// as before W3; `unread-as-none`, a pending set, or the waiting writes, that this Mac cannot
    /// read reads as none (the set's file removed), as before W3; `draft-unclaimed`, a publish
    /// takes no claim ([`claim_draft`]), as before W3, so a pass may sign between its draft and
    /// its pending set; `names-unmerged`, an edit reviews only the names its own entries hold
    /// ([`everyone_edit`]), not those the view showed waiting or that stand now, so it never
    /// takes back a name only a waiting write added; `settle-uncarried`, a save that leaves a
    /// waiting settle of a conflict as its view showed it signs the name without settling it, as
    /// before 24410be588 (`draft_document_diff_with`'s `resolving`).
    pub(crate) static VIEW_WITHOUT_WAITING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static BASES_LOG_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static OVERTAKEN_UNRECORDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static PENDING_BY_COUNT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static MOVES_WAIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static RACE_UNDEFERRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static UNREAD_AS_NONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static DRAFT_UNCLAIMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static NAMES_UNMERGED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static SETTLE_UNCARRIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`DRAFT_UNCLAIMED`] is on; never in a release build, which has neither switch.
fn draft_unclaimed() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        DRAFT_UNCLAIMED.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("draft-unclaimed")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

#[cfg(any(test, debug_assertions))]
fn view_without_waiting() -> bool {
    VIEW_WITHOUT_WAITING.with(std::cell::Cell::get)
        || aterm_types::negative_control::on("view-without-waiting")
}

/// Whether [`NAMES_UNMERGED`] is on; never in a release build, which has neither switch.
fn names_unmerged() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        NAMES_UNMERGED.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("names-unmerged")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether [`SETTLE_UNCARRIED`] is on; never in a release build, which has neither switch.
pub(super) fn settle_uncarried() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        SETTLE_UNCARRIED.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("settle-uncarried")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether [`BASES_LOG_ONLY`] is on; never in a release build, which has neither switch.
fn bases_log_only() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        BASES_LOG_ONLY.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("bases-log-only")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether [`MOVES_WAIT`] is on; never in a release build, which has neither switch.
pub(super) fn moves_wait() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        MOVES_WAIT.with(std::cell::Cell::get) || aterm_types::negative_control::on("moves-wait")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether [`UNREAD_AS_NONE`] is on; never in a release build, which has neither switch.
pub(super) fn unread_as_none() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        UNREAD_AS_NONE.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("unread-as-none")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether [`RACE_UNDEFERRED`] is on; never in a release build, which has neither switch.
fn race_undeferred() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        RACE_UNDEFERRED.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("race-undeferred")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

#[cfg(any(test, debug_assertions))]
fn overtaken_unrecorded() -> bool {
    OVERTAKEN_UNRECORDED.with(std::cell::Cell::get)
        || aterm_types::negative_control::on("overtaken-unrecorded")
}

#[cfg(any(test, debug_assertions))]
fn pending_by_count() -> bool {
    PENDING_BY_COUNT.with(std::cell::Cell::get)
        || aterm_types::negative_control::on("pending-by-count")
}

#[cfg(test)]
impl Waiting {
    /// `[everyone]` `Put`s of `writes` (name, value), each over a name the writer read empty.
    pub(in super::super) fn of_puts(writes: &[(&str, &[u8])]) -> Self {
        Self(
            writes
                .iter()
                .map(|&(name, value)| {
                    let to = Entry {
                        name: name.to_owned(),
                        value: SecretBytes::from(value.to_vec()),
                        audience: Audience::Everyone,
                        if_absent: false,
                        anchor: None,
                    };
                    let write = WaitingWrite {
                        to: Some(to),
                        bases: BTreeSet::from([None]),
                        resolve: false,
                    };
                    (name.to_owned(), write)
                })
                .collect(),
        )
    }

    /// Each name and the value it becomes (empty for a name taken back to absent).
    pub(in super::super) fn values(&self) -> Values {
        self.0
            .iter()
            .map(|(name, write)| {
                let value = write
                    .to
                    .as_ref()
                    .map(|to| to.value.as_slice().to_vec())
                    .unwrap_or_default();
                (name.clone(), value)
            })
            .collect()
    }
}

/// What waits ([`waiting_file`]), as [`Waiting::values`].
#[cfg(test)]
pub(in super::super) fn waiting_values(paths: &Paths, identity: &Identity) -> Values {
    read_waiting(paths, identity).expect("waiting").values()
}

/// Waiting writes as each name and the value it becomes ([`Waiting::values`]).
#[cfg(test)]
pub(in super::super) type Values = BTreeMap<String, Vec<u8>>;

/// What waits ([`waiting_file`]) and what is pending ([`pending_dir`]), by version, oldest first,
/// as [`Waiting::values`].
#[cfg(test)]
pub(in super::super) fn waiting_and_pending(
    paths: &Paths,
    identity: &Identity,
) -> (Values, Vec<([u8; 32], Values)>) {
    let waiting = read_waiting(paths, identity).expect("waiting").values();
    let pending = read_pending(paths, identity)
        .expect("pending")
        .into_iter()
        .map(|set| (set.version, set.writes.values()))
        .collect();
    (waiting, pending)
}

/// The servers each pending set may have reached, by version, oldest first.
#[cfg(test)]
pub(in super::super) fn pending_reach(
    paths: &Paths,
    identity: &Identity,
) -> Vec<([u8; 32], Vec<String>)> {
    read_pending(paths, identity)
        .expect("pending")
        .into_iter()
        .map(|set| (set.version, set.reach))
        .collect()
}

/// One-mesh W3's edit against a view ([`everyone_edit`]), on its own.
#[cfg(test)]
mod edit_tests {
    use super::*;

    /// `[everyone]`'s `name` = `value`.
    fn entry(name: &str, value: &[u8]) -> Entry {
        Entry {
            name: name.to_owned(),
            value: SecretBytes::from(value.to_vec()),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        }
    }

    /// The one write `held` waits with, of `shared`.
    fn held(write: WaitingWrite) -> Waiting {
        Waiting(BTreeMap::from([("shared".to_owned(), write)]))
    }

    /// A save that leaves a waiting write as the view showed it, editing another name, keeps what
    /// that write meant. A settle of a conflict stays one: rewritten with its `resolve`, never
    /// taken back as a plain write the log holds already. A write over the text's value, where
    /// this Mac opens none of the newest epochs and reads that text as the log's, keeps the bases
    /// of the write it replaces, so it still stands over the log's value the earlier write did.
    ///
    /// NEGATIVE CONTROL: `everyone_edit` without the `kept` write's `resolve`, then without
    /// `bases.extend` of each replaced write's bases (each cut by hand and restored with cp from a
    /// backup plus touch, confirmed with cmp): the test fails "a left settle of a conflict stays
    /// one", then "a write over the text's value stands over the log's value its earlier write
    /// did".
    #[test]
    fn a_left_waiting_write_keeps_its_settle_and_its_bases() {
        let other = entry("other", b"x");
        let a = entry("shared", b"a");
        let settle = held(WaitingWrite {
            to: Some(a.clone()),
            bases: BTreeSet::from([Some(entry_digest(&a))]),
            resolve: true,
        });
        let log = BTreeMap::from([("shared".to_owned(), a.clone())]);
        let doc = BTreeMap::from([
            ("shared".to_owned(), a.clone()),
            ("other".to_owned(), other.clone()),
        ]);
        let edit = everyone_edit(Some(&log), Some(&settle), &settle, Some(&doc), |_| false);
        let write = edit
            .writes
            .0
            .get("shared")
            .expect("CONTROL: it is rewritten");
        assert!(write.resolve, "a left settle of a conflict stays one");
        assert!(
            write.stands_over(Some(&a)) && !write.done(Some(&a), true),
            "it stands, not done while the conflict does"
        );
        assert!(
            edit.shows.contains(&"shared".to_owned()),
            "it is named as waiting"
        );

        let (v0, v1) = (entry("shared", b"v0"), entry("shared", b"v1"));
        let earlier = held(WaitingWrite {
            to: Some(v1.clone()),
            bases: BTreeSet::from([Some(entry_digest(&v0))]),
            resolve: false,
        });
        let text = BTreeMap::from([("shared".to_owned(), v1.clone())]);
        let doc = BTreeMap::from([
            ("shared".to_owned(), v1.clone()),
            ("other".to_owned(), other),
        ]);
        let edit = everyone_edit(Some(&text), Some(&earlier), &earlier, Some(&doc), |_| false);
        let write = edit
            .writes
            .0
            .get("shared")
            .expect("CONTROL: it is rewritten");
        assert!(
            write.stands_over(Some(&v0)),
            "a write over the text's value stands over the log's value its earlier write did"
        );
    }
}
