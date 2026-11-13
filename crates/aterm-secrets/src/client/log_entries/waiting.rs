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
    /// the log, and their own bases, and what this Mac's held drafts leave there ([`held_values`],
    /// [`everyone_edit`]). A name the log holds as none of these was changed by another
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

    pub(in super::super) fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
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
    #[expect(
        clippy::disallowed_methods,
        reason = "the shared take of a publish's claim, released by LOCK_UN (DraftClaim's Drop)"
    )]
    let take = |file: &std::fs::File| file.try_lock_shared();
    lock_by(
        paths,
        &claim_file(paths),
        deadline,
        take,
        || {},
        "a vault pass still signed this Mac's waiting writes at the deadline: run it again",
    )
    .map(|file| DraftClaim(Some(file)))
}

/// Take the lock at `path` as `take` does (an exclusive or a shared take that does not block),
/// trying again with a growing backoff until `deadline` (a minute from now where none is given):
/// `busy` runs each time the lock is held, and `late` is the refusal once the deadline passed.
///
/// # Errors
/// The lock cannot be opened or taken, or it is still held at the deadline (`late`).
fn lock_by(
    paths: &Paths,
    path: &Path,
    deadline: Option<std::time::Instant>,
    take: impl Fn(&std::fs::File) -> Result<(), std::fs::TryLockError>,
    mut busy: impl FnMut(),
    late: &str,
) -> crate::Result<std::fs::File> {
    let until =
        deadline.unwrap_or_else(|| std::time::Instant::now() + std::time::Duration::from_secs(60));
    fsx::ensure_private_dir(&paths.dir)?;
    let file = fsx::open_lock(path)?;
    let mut backoff = std::time::Duration::from_millis(1);
    loop {
        match take(&file) {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => busy(),
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(Error::new(format!("{}: {error}", path.display())));
            }
        }
        let remaining = until.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(Error::new(late));
        }
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(std::time::Duration::from_millis(50));
    }
}

/// The lock a publish of this Mac holds exclusive from before its draft reads the log until its
/// hold and pending set are written ([`draft_turn`]).
fn turn_file(paths: &Paths) -> PathBuf {
    paths.dir.join(".everyone.turn")
}

/// A publish's turn to draft ([`draft_turn`]), released on drop, the process's end included.
pub(in super::super) struct DraftTurn(Option<std::fs::File>);

impl Drop for DraftTurn {
    fn drop(&mut self) {
        if let Some(file) = &self.0 {
            let _ = file.unlock();
        }
    }
}

/// Take this Mac's turn to draft for a publish, from before its draft reads the log until its
/// hold and pending set are written (one-mesh P3): one publish drafts at a time. A publish's
/// records are in neither the log nor a hold from its draft until its hold is written
/// ([`hold_drafted`]); a draft that ran meanwhile would read neither, and a waiting write it made
/// of a name the other wrote would be overtaken by this Mac's own earlier value once that publish
/// kept its records. Drafted in turn, the later draft reads them in the log or in that publish's
/// hold, and its waiting write is signed over their value ([`held_values`]): the person's later
/// edit of a name replaces this Mac's own earlier one. Several claims stand at once
/// ([`claim_draft`]), one turn; another publish's turn ends first, within `deadline` (a minute
/// where none is given).
///
/// # Errors
/// The lock cannot be opened, or another publish of this Mac still drafts at the deadline.
pub(in super::super) fn draft_turn(
    paths: &Paths,
    deadline: Option<std::time::Instant>,
) -> crate::Result<DraftTurn> {
    if drafts_at_once() {
        return Ok(DraftTurn(None));
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the exclusive take of a publish's turn, released by LOCK_UN (DraftTurn's Drop)"
    )]
    let take = |file: &std::fs::File| file.try_lock();
    lock_by(
        paths,
        &turn_file(paths),
        deadline,
        take,
        || {
            #[cfg(test)]
            if let Some(hook) = TURN_WAITS.with(|hook| hook.borrow_mut().take()) {
                hook();
            }
        },
        "another publish of this Mac still drafted at the deadline: run it again",
    )
    .map(|file| DraftTurn(Some(file)))
}

#[cfg(test)]
thread_local! {
    /// A test's hook, run once by the next publish on this thread that finds another publish's
    /// turn taken ([`draft_turn`]), before it waits for it.
    pub(crate) static TURN_WAITS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// A vault pass's take of the claim lock ([`claim_draft`]), at once: `None` while a publish holds
/// its claim, and the pass signs nothing now (a later pass does).
///
/// # Errors
/// The lock cannot be opened.
pub(in super::super) fn pass_claim(paths: &Paths) -> crate::Result<Option<DraftClaim>> {
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

/// Overlay `waiting` onto `doc`'s `[everyone]`: each name as its write leaves it, but a Claude
/// Code token's, which no text holds (DESIGN-claude-setup-token §4.4): its write waits for the
/// vault pass, never for an edit.
///
/// # Errors
/// The text refuses an entry.
pub(in super::super) fn overlay_waiting_onto_document(
    doc: &mut Document,
    waiting: &Waiting,
) -> crate::Result<()> {
    // Unanchored entries first, so an anchor exists before an entry names it.
    for anchored in [false, true] {
        for (name, write) in waiting
            .0
            .iter()
            .filter(|(name, _)| !crate::entry_class::claude_token(name))
        {
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
///
/// A Claude Code token's name is none of the edit's ([`crate::entry_class::claude_token`]): no
/// text holds one, its waiting write is the vault pass's to sign, and the draft keeps the log's
/// as it stands.
/// They also hold `held`, what this Mac's own earlier publishes whose records are held and not
/// kept leave at the name ([`held_values`]): such a publish's version may be on a server already,
/// its records entering the log only once it keeps them, after this draft read the log, and its
/// value is this Mac's own earlier write, which the newer edit replaces rather than reverts.
pub(in super::super) fn everyone_edit(
    log: Option<&BTreeMap<String, Entry>>,
    view: Option<&Waiting>,
    now: &Waiting,
    held: &HeldValues,
    doc: Option<&BTreeMap<String, Entry>>,
    resolving: impl Fn(&String) -> bool,
) -> EveryoneEdit {
    let none = BTreeMap::new();
    let log = log.unwrap_or(&none);
    let unseen = Waiting::default();
    let shown = view.unwrap_or(&unseen);
    let ours = |name: &&String| !crate::entry_class::claude_token(name);
    let mut entries = doc.cloned().unwrap_or_default();
    entries.retain(|name, _| ours(&name));
    // The write each name left as the view showed it takes its value from.
    let mut kept: BTreeMap<&String, &WaitingWrite> = BTreeMap::new();
    for (name, was) in shown.0.iter().filter(|(name, _)| ours(name)) {
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
            .chain(shown.0.keys().filter(ours))
            .chain(now.0.keys().filter(ours))
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
            bases.extend(held.get(name).into_iter().flatten().copied());
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

impl WaitingWrite {
    /// A member's `set` of `entry` (`member::member_set`) over `read`, the log's value its draft
    /// read there. It settles a conflict only where that draft read one (`conflicted`), as a
    /// person's save does: a first `set` signs no `Delete` ahead of its `Put`.
    pub(super) fn member(entry: Entry, read: Option<&Entry>, conflicted: bool) -> Self {
        Self {
            to: Some(entry),
            bases: BTreeSet::from([read.map(entry_digest)]),
            resolve: conflicted,
        }
    }

    /// What the name becomes: an entry, or `None` for a take-back.
    pub(super) const fn entry(&self) -> Option<&Entry> {
        self.to.as_ref()
    }
}

/// Whether a write of this Mac's of `name` waits: in [`waiting_file`], or in a pending set
/// ([`pending_dir`]). Where either does not read, it may hold any name, and so it holds this one.
pub(super) fn waits_here(paths: &Paths, identity: &Identity, name: &str) -> bool {
    read_waiting(paths, identity).map_or(true, |held| held.0.contains_key(name))
        || pending_names(paths, identity).is_none_or(|names| names.contains(name))
}

/// Land a member's `write` of `name` (`member::member_set`) in [`waiting_file`] for the next
/// vault pass, under the state lock taken by `deadline` where one is given. It replaces this
/// Mac's earlier writes of the name, the one waiting there and each a pending set holds, and is
/// signed over each value they would have left ([`WaitingWrite::bases`]): the person's later write
/// is the one that stands, so their earlier one is neither signed after it nor read as a change
/// that overtook it. Each pending set loses the name, which would otherwise land over this write
/// once its version lands ([`land_sets`]). Returns the write as it waits.
///
/// # Errors
/// The state lock is not taken by `deadline`, the waiting or pending writes cannot be read
/// (a pending set that does not open included, [`read_pending`]), sealed or written, or this
/// Mac's held drafts do not read ([`held_values`]).
pub(super) fn land_member_write(
    paths: &Paths,
    identity: &Identity,
    name: &str,
    mut write: WaitingWrite,
    deadline: Option<std::time::Instant>,
) -> crate::Result<WaitingWrite> {
    let _lock = waiting_lock(paths, deadline)?;
    let mut held = read_waiting(paths, identity)?;
    let mut sets = if waiting_pending(paths) {
        read_pending(paths, identity)?
    } else {
        Vec::new()
    };
    let earlier = held
        .0
        .get(name)
        .into_iter()
        .chain(sets.iter().filter_map(|set| set.writes.0.get(name)));
    for was in earlier {
        write.bases.insert(was.to.as_ref().map(entry_digest));
        write.bases.extend(was.bases.iter().copied());
    }
    // What this Mac's held drafts leave at the name is its own earlier edit in flight, which the
    // person's later write replaces whichever reaches the log first (one-mesh P3), as an edit's
    // waiting write is signed over it ([`everyone_edit`]).
    if let Some(store) = super::super::remove::trusted_store(paths)?
        && let Some(held) = held_values(paths, &store, identity)?.remove(name)
    {
        write.bases.extend(held);
    }
    for set in &mut sets {
        if set.writes.0.remove(name).is_some() {
            write_pending(paths, identity, set)?;
        }
    }
    held.0.insert(name.to_owned(), write.clone());
    write_waiting(paths, identity, &held)?;
    Ok(write)
}

/// Take back this Mac's own writes of `name` that are still to be signed (`member::member_unset`,
/// DESIGN-claude-setup-token §3.6), under the state lock taken by `deadline` where one is given:
/// the one waiting in [`waiting_file`], and the name in each pending set ([`pending_dir`]). A
/// first `set` that waits is signed over an absent value ([`WaitingWrite::bases`] holds `None`),
/// so left there, the pass after the rotation would sign it over the absence the unset leaves,
/// and the value would come back. Returns whether one was there.
///
/// # Errors
/// The state lock is not taken by `deadline`, or the waiting or pending writes cannot be read
/// (a pending set that does not open included, [`read_pending`]), sealed or written.
pub(super) fn take_back_member_write(
    paths: &Paths,
    identity: &Identity,
    name: &str,
    deadline: Option<std::time::Instant>,
) -> crate::Result<bool> {
    let _lock = waiting_lock(paths, deadline)?;
    let mut held = read_waiting(paths, identity)?;
    let mut sets = if waiting_pending(paths) {
        read_pending(paths, identity)?
    } else {
        Vec::new()
    };
    let mut took = held.0.remove(name).is_some();
    if took {
        write_waiting(paths, identity, &held)?;
    }
    for set in &mut sets {
        if set.writes.0.remove(name).is_some() {
            write_pending(paths, identity, set)?;
            took = true;
        }
    }
    Ok(took)
}

/// Whether `write` still waits at `name` as [`land_member_write`] left it.
pub(super) fn still_waits(
    paths: &Paths,
    identity: &Identity,
    name: &str,
    write: &WaitingWrite,
) -> bool {
    read_waiting(paths, identity)
        .is_ok_and(|held| held.0.get(name).is_some_and(|kept| kept.same(write)))
}

/// The latest record of this Mac's journal of overtaken writes ([`overtaken_file`]) of `name`
/// that a pass dropped at `since_ms` or later.
pub(super) fn overtaken_since(paths: &Paths, name: &str, since_ms: u64) -> Option<Overtaken> {
    read_overtaken(paths)
        .ok()?
        .into_iter()
        .rev()
        .find(|record| record.name == name && record.dropped_ms >= since_ms)
}

/// What this Mac's held drafts leave at each `[everyone]` name they change ([`held_values`]): the
/// [`entry_digest`] of each value the name then holds, or `None` where it holds none.
pub(in super::super) type HeldValues = BTreeMap<String, BTreeSet<Option<[u8; 32]>>>;

/// What each of this Mac's held drafts ([`hold_drafted`]) whose records `store` does not hold yet
/// leaves at each `[everyone]` name it changes, as `identity` opens `store` with that hold's
/// records taken in and without them: the [`entry_digest`] of the value the name then holds, or
/// `None` where it holds none. A hold is the edit of a publish of this Mac whose version a server
/// may hold already, its records entering the log only once that publish keeps them or this Mac
/// adopts them ([`keep_drafted_until`], [`adopt_held_draft`]), so a draft that reads the log
/// meanwhile does not see them. A waiting write drafted now is signed over each
/// ([`everyone_edit`]). Every publish of this Mac drafts in its turn and writes its hold before
/// the turn ends ([`draft_turn`]), so a draft made after it reads its records in the log or in its
/// hold: the person's later edit of a name replaces this Mac's own earlier one, whichever reaches
/// the log first (one-mesh P3).
///
/// # Errors
/// A hold does not read or decode, the store refuses its records, or the store does not open with
/// or without them: what it leaves is unknown, and is never read as nothing (one-mesh W3).
pub(super) fn held_values(
    paths: &Paths,
    store: &Store,
    identity: &Identity,
) -> crate::Result<HeldValues> {
    if bases_without_held() {
        return Ok(BTreeMap::new());
    }
    held_changes(paths, store, identity)
}

/// [`held_values`] as the held drafts read, whatever [`BASES_WITHOUT_HELD`] says: what each hold
/// whose records `store` does not hold yet leaves at each `[everyone]` name it changes.
///
/// # Errors
/// As [`held_values`].
fn held_changes(paths: &Paths, store: &Store, identity: &Identity) -> crate::Result<HeldValues> {
    let mut values = HeldValues::new();
    let holds = held_drafts(paths);
    if holds.is_empty() {
        return Ok(values);
    }
    // `[everyone]` as `identity` opens `store`, each name with its value's digest.
    let everyone = |store: &Store| -> crate::Result<Option<BTreeMap<String, [u8; 32]>>> {
        let Some(log) = store.log() else {
            return Ok(None);
        };
        Ok(log.open_audience_entries(identity)?.map(|opened| {
            opened
                .by_audience
                .get(&Audience::Everyone)
                .into_iter()
                .flatten()
                .map(|(name, entry)| (name.clone(), entry_digest(entry)))
                .collect()
        }))
    };
    let before = everyone(store)?
        .ok_or_else(|| Error::new("this Mac does not open its log for its held drafts"))?;
    for (_, _, path) in holds {
        let unread = |why: &str| Error::new(format!("{}: {why}", path.display()));
        // None: the hold went since the directory was read.
        let Some(held) = read_held(&path)? else {
            continue;
        };
        let records = crate::membership_log::decode_records(held.records())
            .map_err(|error| unread(&error.to_string()))?;
        if records.iter().all(|record| store.holds(record.id())) {
            continue;
        }
        let mut with = store.clone();
        with.ingest_records(records)
            .map_err(|error| unread(&error.to_string()))?;
        let after = everyone(&with)
            .map_err(|error| unread(&error.to_string()))?
            .ok_or_else(|| unread("this Mac does not open its log with it"))?;
        for name in before.keys().chain(after.keys()) {
            let now = after.get(name).copied();
            if before.get(name).copied() != now {
                values.entry(name.clone()).or_default().insert(now);
            }
        }
    }
    Ok(values)
}

/// Whether a member's write of `names` (`member::member_set`) waits for this Mac's held drafts, as
/// a vault pass defers the names they change: one of them changes a name, or one does not read
/// and so may change any (one-mesh P3, W3).
pub(super) fn held_defers(
    paths: &Paths,
    store: &Store,
    identity: &Identity,
    names: &[&str],
) -> bool {
    if held_undeferred() {
        return false;
    }
    match held_changes(paths, store, identity) {
        Ok(held) => names.iter().any(|name| held.contains_key(*name)),
        Err(_) => !unread_as_none(),
    }
}

/// What a draft whose writes wait reads of this Mac's held drafts ([`held_values`]): what each
/// leaves at each name, or none and why they do not read. A hold that does not read is never
/// read as leaving nothing (one-mesh W3): the draft refuses over it ([`nle::draft_refuses`]), as
/// over unread waiting writes. [`UNREAD_AS_NONE`] reads it as nothing, as before.
pub(in super::super) fn held_values_or_unread(
    paths: &Paths,
    store: &Store,
    identity: &Identity,
) -> (HeldValues, Option<Error>) {
    match held_values(paths, store, identity) {
        Ok(values) => (values, None),
        Err(_) if unread_as_none() => (BTreeMap::new(), None),
        Err(error) => (BTreeMap::new(), Some(error)),
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

/// How a writer outside an edit seals its `[everyone]` writes now: a vault pass signing what
/// waits ([`sign_waiting_writes_by`]), and a member's `set` signing its own (`member::member_set`).
pub(super) enum EveryoneSeal {
    /// Under this epoch, the one a writer seals under ([`sealing_epoch`]), with its key.
    Reuse(RecordId, zeroize::Zeroizing<[u8; 32]>),
    /// Under a fresh epoch wrapped to these recipients: a newest epoch was minted by, or wraps, a
    /// Mac the log or roster denies, a departure that is no removal.
    Fresh(BTreeSet<Recipient>),
}

/// How the Mac of `identity` seals its next `[everyone]` writes as `opened` reads `log`
/// ([`EveryoneSeal`]). Where a newest epoch was minted by, or wraps, a Mac the log or roster
/// denies, a fresh epoch, wrapped to the Macs the newest epochs wrap and to the current seats
/// (each seat a batch still arriving may wrap among them), less each one the log removed or the
/// log or roster denies, so neither concurrent heads nor a batch still arriving hold it.
/// Otherwise the epoch a writer seals under ([`sealing_epoch`]). `None` where there is none:
/// newest epochs concurrent with distinct recipient sets (the tender coalesces them), or a batch
/// still arriving that may wrap a Mac outside the current seats and outside those only that batch
/// can decide; or where this Mac holds no key of the epoch it would reuse.
pub(super) fn everyone_seal(
    log: &Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    identity: &Identity,
) -> Option<EveryoneSeal> {
    let newest = opened
        .maximal_epochs
        .get(&EpochAudience::Everyone)
        .cloned()
        .unwrap_or_default();
    let roster = roster_from_doc_or_opened(None, Some(opened), Some(log));
    let denied = |r: &Recipient| recipient_denied_by_roster_or_log(roster.as_ref(), log, r);
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
    if departed {
        return Some(EveryoneSeal::Fresh(everyone_fresh_to(log, opened)));
    }
    let current = log.current_recipients_where(&EpochAudience::Everyone, |seat| {
        roster
            .as_ref()
            .is_some_and(|r| member_denied_by_roster(r, seat))
    });
    let flight = Flight::of_reader(log, &identity.recipient());
    let epoch = sealing_epoch(
        log,
        opened,
        &newest,
        identity,
        &current,
        flight.undecided(&EpochAudience::Everyone),
    )?;
    let key = opened.unwrapped_keys.get(&epoch)?;
    Some(EveryoneSeal::Reuse(epoch, zeroize::Zeroizing::new(**key)))
}

/// Whom a fresh `[everyone]` epoch a writer outside an edit mints is wrapped to, as `opened` reads
/// `log` ([`EveryoneSeal::Fresh`], and a member's unset, `member::member_unset`): the Macs the
/// newest epochs wrap and the current seats (each seat a batch still arriving may wrap among
/// them), less each one the log removed or the log or roster denies, so neither concurrent heads
/// nor a batch still arriving hold it.
pub(super) fn everyone_fresh_to(
    log: &Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
) -> BTreeSet<Recipient> {
    let roster = roster_from_doc_or_opened(None, Some(opened), Some(log));
    let current = log.current_recipients_where(&EpochAudience::Everyone, |seat| {
        roster
            .as_ref()
            .is_some_and(|r| member_denied_by_roster(r, seat))
    });
    opened
        .maximal_epochs
        .get(&EpochAudience::Everyone)
        .into_iter()
        .flatten()
        .filter_map(|epoch| opened.wrapped_recipients.get(epoch))
        .flatten()
        .chain(&current)
        .filter(|r| {
            !log.recipient_is_removed(r)
                && !recipient_denied_by_roster_or_log(roster.as_ref(), log, r)
        })
        .copied()
        .collect()
}

/// Sign `deletes`, then `puts`, of `[everyone]` into `log` under `seal`, as the member `signer`
/// (its actor, device key and incarnation) at `authored_ms`: a fresh epoch and its wraps first
/// where `seal` mints one, and a `Delete` before its `Put`, so a person's save replaces every
/// sibling. Returns what it signed, in order, already in `log`; nothing where there is nothing to
/// write.
///
/// # Errors
/// A record fails to draft, sign or go into `log`.
pub(super) fn sign_everyone(
    log: &mut Log,
    opened: &crate::membership_log::entries::OpenedAudienceEntries,
    signer: (&MemberId, &Device, [u8; 16]),
    authored_ms: u64,
    seal: EveryoneSeal,
    deletes: &[&String],
    puts: &[&Entry],
) -> crate::Result<Vec<Record>> {
    let (actor, device, incarnation) = signer;
    let mut minted = Vec::new();
    if puts.is_empty() && deletes.is_empty() {
        return Ok(minted);
    }
    device.with_seed(|writer_seed| -> crate::Result<()> {
        let (epoch, key) = match seal {
            EveryoneSeal::Reuse(epoch, key) => (epoch, key),
            EveryoneSeal::Fresh(fresh_to) => {
                let generation = opened
                    .max_generation
                    .get(&EpochAudience::Everyone)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1)
                    .max(1);
                let (draft, key) =
                    log.draft_epoch(actor, authored_ms, EpochAudience::Everyone, generation)?;
                let record = draft.sign(writer_seed, incarnation)?;
                let epoch = *record.id();
                let _ = log.insert(record.clone())?;
                minted.push(record);
                for recipient in fresh_to {
                    let record = log
                        .draft_wrap(actor, authored_ms, epoch, &key, recipient)?
                        .sign(writer_seed, incarnation)?;
                    let _ = log.insert(record.clone())?;
                    minted.push(record);
                }
                (epoch, key)
            }
        };
        for name in deletes {
            let record = log
                .draft_delete(actor, authored_ms, epoch, &key, name)?
                .sign(writer_seed, incarnation)?;
            let _ = log.insert(record.clone())?;
            minted.push(record);
        }
        for entry in puts {
            let record = log
                .draft_put(actor, authored_ms, epoch, &key, entry)?
                .sign(writer_seed, incarnation)?;
            let _ = log.insert(record.clone())?;
            minted.push(record);
        }
        Ok(())
    })?;
    Ok(minted)
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
/// reseals what stays (F15), which only an edit's draft or a member's unset mints
/// (`member::member_unset`), and the person's next edit or unset removes it that way. A
/// publish's claim ([`claim_draft`]) keeps this Mac's own earlier write from being signed between
/// the draft of a take-back and its pending set. Each dropped write leaves a record
/// in this Mac's journal of overtaken writes ([`overtaken_file`]): the entry, the op that holds it
/// now and when the write was dropped, which the admin view shows once. A record, never an alert.
/// A write whose name a pending set of this Mac writes ([`pending_names`]) is left as it is: that
/// set is this Mac's own later edit of the name, which lands over it or goes and leaves it, and
/// is never read as a change that overtook it. So is one whose name a held draft of this Mac
/// changes ([`held_changes`]), an edit in flight whose records the log lacks until its keep or
/// adoption, and every write while a held draft does not read. Nothing is signed while a publish
/// of this Mac holds its claim, from before its draft reads the log until its pending set is written
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
        let pending = if race_undeferred() {
            Some(BTreeSet::new())
        } else {
            pending_names(paths, &identity)
        };
        // Whether the pass signs at all is no_lost_edit's kernel (one-mesh P3, the island's
        // `signW`), over whether every pending set reads.
        if !nle::pass_signs(due, opens, pending.is_some()) {
            return none();
        }
        let Some(mut deferred) = pending else {
            return none();
        };
        // A name a held draft of this Mac changes is this Mac's own edit in flight too: its
        // records enter the log only once its publish keeps them or this Mac adopts them, so the
        // log read here does not hold them. Signed now, the write would meet them as a concurrent
        // write of its name once they are kept (a conflict), and an earlier edit's keep would take
        // the place of the later edit signed here (one-mesh P3, LaterKept); its write waits for
        // the keep, or for the hold's going. A hold that does not read may change any name, so
        // every name waits (the island's `deferredW` with `holdUnread`): no write is signed or
        // dropped while it is there (one-mesh W3).
        if !held_undeferred() {
            match held_changes(paths, store, &identity) {
                Ok(held) => deferred.extend(held.into_keys()),
                Err(_) if unread_as_none() => {}
                Err(_) => return none(),
            }
        }
        let Some(seal) = everyone_seal(&log, &opened, &identity) else {
            return none();
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
        let minted = sign_everyone(
            &mut log,
            &opened,
            (&actor, &device, incarnation),
            now_ms,
            seal,
            &deletes,
            &puts,
        )?;
        Ok((minted, (settled, overtaken)))
    };
    let authored = match deadline {
        Some(deadline) => author_and_commit_records_until(paths, deadline, draft)?,
        None => Some(author_and_commit_records(paths, draft)?),
    };
    let Some((minted, _, (settled, overtaken))) = authored else {
        return Ok(None);
    };
    #[cfg(test)]
    if let Some(hook) = PASS_COMMITTED.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
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
pub(super) fn overtaker(
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
    /// read reads as none (the set's file removed), as before W3, and so does a held draft that
    /// does not read ([`held_values`]); `draft-unclaimed`, a publish
    /// takes no claim ([`claim_draft`]), as before W3, so a pass may sign between its draft and
    /// its pending set; `names-unmerged`, an edit reviews only the names its own entries hold
    /// ([`everyone_edit`]), not those the view showed waiting or that stand now, so it never
    /// takes back a name only a waiting write added; `settle-uncarried`, a save that leaves a
    /// waiting settle of a conflict as its view showed it signs the name without settling it, as
    /// before 24410be588 (`draft_document_diff_with`'s `resolving`). One-mesh P3's
    /// `bases-without-held`: a write's bases leave out what this Mac's held drafts write
    /// ([`held_values`]), as before, so a waiting write drafted while an earlier version of this
    /// Mac is stored and its records are not kept yet is overtaken by that version's value;
    /// `drafts-at-once`, a publish takes no turn ([`draft_turn`]), as before, so its draft may run
    /// while another publish of this Mac drafted records it has not held yet, and a waiting write
    /// it makes is overtaken by that publish's value; `held-undeferred`, a vault pass defers only
    /// the names pending sets write, not those this Mac's held drafts change, as before, so it
    /// signs a later edit while an earlier direct draft of its name is in flight, and that
    /// draft's keep then takes the later edit's place.
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
    pub(crate) static BASES_WITHOUT_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static DRAFTS_AT_ONCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static HELD_UNDEFERRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`HELD_UNDEFERRED`] is on; never in a release build, which has neither switch.
fn held_undeferred() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        HELD_UNDEFERRED.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("held-undeferred")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether [`DRAFTS_AT_ONCE`] is on; never in a release build, which has neither switch.
fn drafts_at_once() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        DRAFTS_AT_ONCE.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("drafts-at-once")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether [`BASES_WITHOUT_HELD`] is on; never in a release build, which has neither switch.
fn bases_without_held() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        BASES_WITHOUT_HELD.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("bases-without-held")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
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
thread_local! {
    /// A test's hook, run once by the next vault pass on this thread that commits, between its
    /// commit and its settle ([`sign_waiting_writes_by`]): another lock section while the pass
    /// holds its claim, as P3's island lets one run between `signPass` and `settlePass`.
    pub(in super::super) static PASS_COMMITTED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// One write as [`Waiting::of_writes`] takes it: its name, the value it becomes (`None`: taken
/// back to absent) and the values it may be signed over (`None`: none).
#[cfg(test)]
pub(in super::super) type WriteShape<'a> = (&'a str, Option<&'a [u8]>, &'a [Option<&'a [u8]>]);

/// Waiting writes as each name, the value it becomes (`None`: taken back to absent) and its
/// bases ([`WaitingWrite::bases`]).
#[cfg(test)]
pub(in super::super) type Shapes = BTreeMap<String, (Option<Vec<u8>>, BTreeSet<Option<[u8; 32]>>)>;

/// The base `value` is in a write's bases ([`WaitingWrite::bases`]): its [`entry_digest`] as an
/// `[everyone]` entry with no flag and no anchor.
#[cfg(test)]
pub(in super::super) fn base_of(value: &[u8]) -> [u8; 32] {
    entry_digest(&Entry {
        name: String::new(),
        value: SecretBytes::from(value.to_vec()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    })
}

/// What waits ([`waiting_file`]), and each pending set that opens ([`pending_dir`]) by version,
/// oldest first, as [`Waiting::shapes`]: a set that does not open is left out.
#[cfg(test)]
pub(in super::super) fn waiting_shapes(
    paths: &Paths,
    identity: &Identity,
) -> (Shapes, Vec<([u8; 32], Shapes)>) {
    let waiting = read_waiting(paths, identity).expect("waiting").shapes();
    let mut files = pending_files(paths).expect("pending");
    files.sort_by_key(|&(order, seq, version, _)| (order, seq, version));
    let pending = files
        .into_iter()
        .filter_map(|(_, _, version, path)| {
            let sealed = fsx::read_private(&path, crate::membership_log::MAX_RECORDS_STREAM)
                .expect("a pending set")?;
            let (_, writes) = open_pending(identity, sealed.as_slice())?;
            Some((version, writes.shapes()))
        })
        .collect();
    (waiting, pending)
}

#[cfg(test)]
impl Waiting {
    /// `[everyone]` writes of `writes`: each name, the value it becomes (`None`: taken back to
    /// absent) and the values it may be signed over (`None`: none).
    pub(in super::super) fn of_writes(writes: &[WriteShape<'_>]) -> Self {
        Self(
            writes
                .iter()
                .map(|&(name, value, bases)| {
                    let to = value.map(|value| Entry {
                        name: name.to_owned(),
                        value: SecretBytes::from(value.to_vec()),
                        audience: Audience::Everyone,
                        if_absent: false,
                        anchor: None,
                    });
                    let bases = bases.iter().map(|base| base.map(base_of)).collect();
                    let write = WaitingWrite {
                        to,
                        bases,
                        resolve: false,
                    };
                    (name.to_owned(), write)
                })
                .collect(),
        )
    }

    /// These writes with each of `more` among the bases of the write of `name`, where one is
    /// ([`WaitingWrite::bases`]).
    pub(in super::super) fn with_bases(
        mut self,
        name: &str,
        more: impl IntoIterator<Item = Option<[u8; 32]>>,
    ) -> Self {
        if let Some(write) = self.0.get_mut(name) {
            write.bases.extend(more);
        }
        self
    }

    /// These writes and `other`'s, `other`'s where both write a name.
    pub(in super::super) fn joined(mut self, other: Self) -> Self {
        self.0.extend(other.0);
        self
    }

    /// Each name, the value it becomes and its bases ([`Shapes`]).
    pub(in super::super) fn shapes(&self) -> Shapes {
        self.0
            .iter()
            .map(|(name, write)| {
                let value = write.to.as_ref().map(|to| to.value.as_slice().to_vec());
                (name.clone(), (value, write.bases.clone()))
            })
            .collect()
    }

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

/// Whether the write of `name` that waits here may be signed over `read`, the log's value there
/// ([`WaitingWrite::stands_over`]); `None` where none waits.
#[cfg(test)]
pub(in super::super) fn waits_over(
    paths: &Paths,
    identity: &Identity,
    name: &str,
    read: Option<&Entry>,
) -> Option<bool> {
    read_waiting(paths, identity)
        .expect("waiting")
        .0
        .get(name)
        .map(|write| write.stands_over(read))
}

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
#[path = "waiting_edit_tests.rs"]
mod edit_tests;
