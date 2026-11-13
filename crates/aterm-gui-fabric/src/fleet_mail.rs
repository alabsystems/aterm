// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Direct session mail over an authenticated fleet drive. Its `fN` identifiers belong to this
//! mailbox, never to a broker log. A reply is retained for the sending node to read on its next
//! drive; no sender can read another node's conversations. The private file is authoritative,
//! so a lost reply, a process restart and a seamless update all preserve the same identifiers.
//!
//! AT REST the file is sealed (astream-aead's XChaCha20-Poly1305) under its directory's
//! `key`, one for every mailbox there: 32 random bytes minted by the first write, 0600 in the
//! 0700 directory, read under the same owner-only rule as the mailbox and left out of backups
//! (`aterm_uds::backup`), so a backup of sealed mailboxes that honours the mark cannot open them.
//! It is not the bus's `rest.key`: direct mail works with the bus off. A mailbox's additional data is its
//! file name, so it opens only as the session it was written for. A mailbox an earlier build
//! wrote in plaintext is sealed by its active owner's next transaction, before its edit.
//! Startup does not migrate another session's file: an idle older owner holds no transaction
//! lock and cannot read the sealed format. An uncommitted handoff candidate does no mailbox I/O.
//! Nothing here writes plaintext. A sealed mailbox whose key is
//! missing, or that does not open under it, refuses direct mail and is left as it is, and no key
//! is minted while any mailbox in the directory is sealed: put the key back, or move the sealed
//! mailboxes aside to start empty ones. Bus mail is untouched. WHAT IT DOES NOT PROTECT: the mail
//! from a process running as this user, or in a copy of the whole directory, key included.
//!
//! WITH THE MESH ON (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 C6) this mailbox is the way
//! pre-R3 Macs reach this one, and this one them, until D7 deletes it:
//! - **The fabric inbox holds the unread mail.** Each transaction (the first is the one the
//!   session's first start takes, [`restore`] at adoption) takes every unread row this process has
//!   not yet imported into the session's ring as an ordinary row ([`super::InboxRow::fleet`]).
//!   Listing it there ([`super::cmd_inbox`], `inbox seen`) marks the row listed in the sealed
//!   file ([`mark_listed`]), so it is imported once: no restart and no successor takes it again.
//!   The mark is the listing, never the import, because the ring is not carried: a successor
//!   offers again what the reader had not seen, as the bridge does for bus mail. A message is
//!   still answered by `post re=f<id>`, which its sender reads on its next drive.
//! - **A post to a pre-R3 Mac goes by direct fleet mail** ([`carry`]): a Mac this one never
//!   heard a presence row from ([`PeerMail::Fleet`]) has no mesh mail route, so `post
//!   to=@<sid>@<node>` drives it and posts into this mailbox's twin there.

use super::{FabricSession, serving_driver};
use serde::{Deserialize, Serialize};

#[cfg(unix)]
pub mod watch;

const ROWS: usize = 64;
const PER_NODE: usize = 16;
const TEXT: usize = 4096;

/// Capture the release policy once when the session is created.
#[cfg(unix)]
pub(super) struct MeshPolicy {
    pub(super) enabled: bool,
}

#[cfg(unix)]
impl Default for MeshPolicy {
    fn default() -> Self {
        Self {
            enabled: aterm_types::mesh_switch::MESH,
        }
    }
}

#[cfg(all(test, unix))]
impl MeshPolicy {
    /// The mesh on, whatever the switch says: a test's session.
    pub(super) const fn on() -> Self {
        Self { enabled: true }
    }
}

/// How another Mac takes a session's mail, with the mesh on (§11 C6).
///
/// A Mac speaks the mesh mail route exactly when this Mac has heard its presence row: rows exist
/// only on builds with the mesh on, the route ships in the same cut (waves B and C ship together,
/// decision 2026-11-05), and a row decodes only at the version C3 gave it. A build string or the
/// fleet book's version names no release before its cut, so neither can say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerMail {
    /// This Mac heard its presence row: mail goes on the bus, one hop (C3).
    Mesh,
    /// No presence row of it was heard here, a pre-R3 Mac: direct fleet mail reaches it
    /// ([`carry`]). A Mac on R3 not heard yet takes direct fleet mail too, until D7.
    Fleet,
}

/// A direct message as this process imported it into its session's ring: its id and key, which
/// name it in its mailbox whatever becomes of the ring's row, and which the mesh's rewrite of
/// its sender's node (B1, [`rewrite_nodes`]) leaves as they were.
#[cfg(unix)]
pub(crate) type Imported = (u64, String);
#[cfg(unix)]
const BUSY: &str = "another direct-mail operation holds this mailbox";
/// The largest mailbox, as the TOML that is sealed.
#[cfg(unix)]
const FILE_BYTES: usize = ROWS * (2 * TEXT * 6 + 2048);
/// The file name, in the mailbox directory, of the key every mailbox there is sealed under.
#[cfg(unix)]
pub const KEY: &str = "key";
/// What a sealed mailbox's file begins with, ahead of astream-aead's `nonce ‖ ciphertext ‖ tag`.
/// No TOML holds a NUL, so neither an earlier build's plaintext nor a sealed file is ever taken
/// for the other.
#[cfg(unix)]
const SEALED: &[u8] = b"\0aterm fleet mail, sealed\n";
/// The largest sealed mailbox file.
#[cfg(unix)]
const SEALED_BYTES: usize = SEALED.len() + FILE_BYTES + astream_aead::OVERHEAD;
/// How the key is left out of backups: the platform's mark, which needs no permission.
#[cfg(unix)]
const BACKUPS: aterm_uds::backup::Backups = aterm_uds::backup::Backups::SYSTEM;

#[derive(Debug)]
enum MailError {
    #[cfg(unix)]
    Busy,
    Failed(String),
}

impl std::fmt::Display for MailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(unix)]
            Self::Busy => f.write_str(BUSY),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

impl From<String> for MailError {
    fn from(why: String) -> Self {
        Self::Failed(why)
    }
}

impl From<&str> for MailError {
    fn from(why: &str) -> Self {
        Self::Failed(why.to_owned())
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Mailbox {
    next: u64,
    rows: Vec<Row>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Row {
    id: u64,
    node: String,
    key: String,
    kind: String,
    text: String,
    #[serde(default)]
    listed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply: Option<Reply>,
    #[serde(default)]
    reply_seen: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct Reply {
    key: String,
    kind: String,
    text: String,
}

/// The bounded, body-free snapshot read by presence and subscription readers, and by `inbox` and
/// `await inbox` while the file is the one it was taken from ([`from_cache`]). The private
/// mailbox remains authoritative; only transactions refresh this cache.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Metadata {
    pub high: u64,
    pub rows: Vec<Notice>,
}

/// A direct arrival's independent `fN` identity, never a broker offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub id: u64,
    pub from: String,
    pub kind: String,
    pub listed: bool,
    pub replied: bool,
}

/// The mailbox file as one observation saw it. Every write publishes a new file by rename
/// (`write_private`), so a write by this process or by a handoff peer changes it.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(unix)]
impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            ctime: meta.ctime(),
            ctime_nsec: meta.ctime_nsec(),
        }
    }
}

/// The stamp of the private file at `path`, judged on its open descriptor without reading its
/// contents, taking the mailbox's lock or making its directory. The directory and file pass the
/// same ownership, mode, symlink and ACL rules as a mailbox transaction.
/// # Errors
/// `NotFound` when there is no mailbox and its directory is missing or private, the two a
/// transaction makes or uses; a missing mailbox in a directory a transaction refuses, any other
/// unsafe path, or a failed metadata read is refused.
#[cfg(unix)]
pub fn stamp(path: &std::path::Path) -> std::io::Result<Stamp> {
    use aterm_uds::private_file::{JudgedDir, PrivateFileError, check_private_dir};
    #[cfg(test)]
    STAMPS.with(|n| n.set(n.get() + 1));
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the mailbox has no directory"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("the mailbox has no file name"))?;
    let none = || std::io::Error::new(std::io::ErrorKind::NotFound, "no mailbox");
    let judged = JudgedDir::judge(dir).map_err(|error| match error {
        PrivateFileError::Io(error) => error,
        error => std::io::Error::other(error.to_string()),
    })?;
    let (_file, metadata) = judged.open(name).map_err(|error| match error {
        // `JudgedDir::open` answers a missing file before it judges an existing directory's
        // owner, mode and access list, so absence is judged against those rules here.
        PrivateFileError::Missing if judged.is_private() || judged.is_missing() => none(),
        PrivateFileError::Missing => match check_private_dir(dir) {
            Ok(()) => none(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => none(),
            Err(refused) => refused,
        },
        PrivateFileError::Io(error) => error,
        error => std::io::Error::other(error.to_string()),
    })?;
    Ok(Stamp::of(&metadata))
}

#[cfg(all(test, unix))]
thread_local! {
    /// This thread's mailbox transactions, which a reader answered from the cache did not take.
    static TRANSACTIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Those of them the handler tally times as [`super::handler_tally::Row::FleetMail`].
    static TIMED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// This thread's mailbox stamps ([`stamp`]).
    static STAMPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// A negative control's switch: this thread's refreshes ring no waiter.
    static UNRUNG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// A negative control's switch: this thread's ring listings write no mark ([`mark_listed`]).
    static UNMARKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Answer a reader from the cache when no transaction is needed to know the mailbox: `None`
/// asks for one. A missing file whose directory a transaction would make or use ([`stamp`]) is
/// an empty mailbox: every write publishes by rename, so one descriptor observes one whole
/// state, and creates neither the directory nor the lock file. A file
/// whose stamp is the one the cache was taken at under the lock is what the cache describes.
/// `answer` runs under the session's fabric lock and itself answers `None` when the mailbox
/// must be read or edited.
#[cfg(unix)]
fn from_cache<T>(
    ctx: &dyn FabricSession,
    path: &std::path::Path,
    answer: impl FnOnce(&Metadata) -> Option<T>,
) -> Option<T> {
    if !ctx.fleet_mail_ready() {
        return None;
    }
    match stamp(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => answer(&Metadata::default()),
        Ok(now) => {
            let inbox = ctx.fabric().lock();
            if inbox.direct_stamp == Some(now) {
                answer(&inbox.direct_mail)
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

/// Cache `boxed` and the stamp of the file it was read from or written to, under the
/// mailbox's lock, so the pair always describes one state.
#[cfg(unix)]
fn cache(ctx: &dyn FabricSession, boxed: &Mailbox, stamp: Option<Stamp>) -> bool {
    let metadata = Metadata {
        high: boxed.next,
        rows: boxed
            .rows
            .iter()
            .map(|row| Notice {
                id: row.id,
                from: format!("fleet:{}", row.node),
                kind: row.kind.clone(),
                listed: row.listed,
                replied: row.reply.is_some(),
            })
            .collect(),
    };
    let mut inbox = ctx.fabric().lock();
    inbox.direct_stamp = stamp;
    if inbox.direct_mail != metadata {
        inbox.direct_mail = metadata;
        true
    } else {
        false
    }
}

/// Refresh discovery after adoption or a write by a handoff peer, only on a worker.
/// False means this observation failed or a writer holds the lock; retry its file stamp.
/// Missing or damaged storage does not affect broker mail or session startup.
pub fn restore(ctx: &dyn FabricSession) -> bool {
    if ctx.fleet_mail_path().is_some() {
        #[cfg(unix)]
        return transaction_within(ctx, std::time::Duration::ZERO, |_| Ok(((), false))).is_ok();
    }
    true
}

fn kind(word: &str) -> bool {
    matches!(word, "ask" | "task" | "note" | "answer" | "report")
}

fn id(word: &str) -> Option<u64> {
    let digits = word.strip_prefix('f')?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
        .filter(|n| *n > 0)
}

/// Whether `node` is a fleet node's id as a proved drive names it: 32 lowercase hex digits, or
/// `n-` and 16 of them (the mesh's form).
#[must_use]
pub fn valid_node(node: &str) -> bool {
    let (node, len) = node.strip_prefix("n-").map_or((node, 32), |id| (id, 16));
    node.len() == len
        && node
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(unix)]
fn validated(boxed: &Mailbox) -> bool {
    let mut ids = std::collections::BTreeSet::new();
    let mut keys = std::collections::BTreeSet::new();
    boxed.rows.len() <= ROWS
        && boxed.rows.iter().all(|row| {
            row.id > 0
                && row.id <= boxed.next
                && ids.insert(row.id)
                && keys.insert((&row.node, &row.key))
                && valid_node(&row.node)
                && super::valid_key(&row.key)
                && kind(&row.kind)
                && !row.text.is_empty()
                && row.text.len() <= TEXT
                && (!row.reply_seen || row.reply.is_some())
                && row.reply.as_ref().is_none_or(|reply| {
                    super::valid_key(&reply.key)
                        && kind(&reply.kind)
                        && !reply.text.is_empty()
                        && reply.text.len() <= TEXT
                })
        })
}

/// The span of a transaction a verb asked for, which the handler tally counts as
/// [`super::handler_tally::Row::FleetMail`]. The session's watcher's refresh ([`restore`])
/// serves no verb and takes none.
#[cfg(unix)]
fn verb_span() -> super::handler_tally::Timer {
    #[cfg(test)]
    TIMED.with(|n| n.set(n.get() + 1));
    super::handler_tally::timer(|| super::handler_tally::Row::FleetMail)
}

/// A verb's transaction, waiting up to a minute for the mailbox's lock.
fn transaction<T>(
    ctx: &dyn FabricSession,
    edit: impl FnOnce(&mut Mailbox) -> Result<(T, bool), String>,
) -> Result<(T, bool), String> {
    #[cfg(unix)]
    let _span = verb_span();
    transaction_within(ctx, std::time::Duration::from_secs(60), edit).map_err(|why| why.to_string())
}

#[cfg(unix)]
fn transaction_within<T>(
    ctx: &dyn FabricSession,
    within: std::time::Duration,
    edit: impl FnOnce(&mut Mailbox) -> Result<(T, bool), String>,
) -> Result<(T, bool), MailError> {
    transaction_waiting(ctx, within, edit, || {})
}

/// The mailbox directory's [`KEY`]. Its bytes are cleared when it drops.
#[cfg(unix)]
struct Key([u8; astream_aead::KEY_LEN]);

#[cfg(unix)]
impl Drop for Key {
    fn drop(&mut self) {
        self.0.fill(0);
        std::hint::black_box(&mut self.0);
    }
}

/// The [`KEY`] in the mailbox directory `dir`, `None` when there is none, read under the
/// owner-only rule and marked to stay out of backups again: a copy put back from elsewhere may
/// have lost the mark. A temporary a crashed [`mint_key`] left is the key's second link, which
/// that rule refuses, so it is swept first; a live minter's stays, and this read refuses for its
/// moment.
#[cfg(unix)]
fn read_key(dir: &std::path::Path) -> Result<Option<Key>, String> {
    use aterm_uds::private_file::{PrivateFileError, open_private};
    use std::io::Read as _;
    let _ = aterm_uds::atomic_file::sweep_temps_beside(&dir.join(KEY), std::time::Duration::ZERO);
    let (file, meta) = match open_private(dir, KEY) {
        Ok(opened) => opened,
        Err(PrivateFileError::Missing) => return Ok(None),
        Err(e) => return Err(format!("the direct fleet mail key does not read: {e}")),
    };
    // A volume that keeps no attributes, or a system with no mark: the mint said so.
    let _ = BACKUPS.exclude(&file);
    let mut bytes = Vec::with_capacity(astream_aead::KEY_LEN + 1);
    let read = (&file)
        .take(astream_aead::KEY_LEN as u64 + 1)
        .read_to_end(&mut bytes);
    let key = <[u8; astream_aead::KEY_LEN]>::try_from(bytes.as_slice()).map(Key);
    bytes.fill(0);
    std::hint::black_box(&mut bytes);
    read.map_err(|e| format!("the direct fleet mail key does not read: {e}"))?;
    // The read stops one byte past a key's length; the opened file's own length is the one to
    // name.
    key.map(Some).map_err(|_| {
        format!(
            "the direct fleet mail key does not read: it holds {} bytes, not {}",
            meta.len(),
            astream_aead::KEY_LEN
        )
    })
}

/// Whether file `name` in the mailbox directory is a mailbox, `<session>.toml`: never a
/// temporary (`.`-led), a lock (`.lock`) or the [`KEY`].
#[cfg(unix)]
fn is_mailbox(name: &str) -> bool {
    !name.starts_with('.') && name.ends_with(".toml")
}

/// The mailbox names in `dir`, including the target of a temporary of their own beside them
/// ([`aterm_uds::atomic_file::create_temp_beside`]). A missing directory lists none.
#[cfg(unix)]
fn mailboxes(dir: &std::path::Path) -> Result<std::collections::BTreeSet<String>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(format!("the direct fleet mail folder does not list: {e}")),
    };
    let mut listed = std::collections::BTreeSet::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| format!("the direct fleet mail folder does not list: {e}"))?;
        let name = entry.file_name();
        let temporary = aterm_uds::atomic_file::temp_target(&name);
        if let Some(mailbox) = temporary.or(name.to_str())
            && is_mailbox(mailbox)
        {
            listed.insert(mailbox.to_owned());
        }
    }
    Ok(listed)
}

/// Whether the mailbox file `name` in `dir`, judged by `judged`, is sealed: `None` when there is
/// no such file.
#[cfg(unix)]
fn sealed_file(
    judged: &aterm_uds::private_file::JudgedDir<'_>,
    name: &str,
) -> Result<Option<bool>, String> {
    use std::io::Read as _;
    let file = match judged.open(name) {
        Ok((file, _)) => file,
        Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(None),
        Err(e) => {
            return Err(format!(
                "a direct mailbox beside the key does not read: {e}"
            ));
        }
    };
    let mut head = Vec::with_capacity(SEALED.len() + 1);
    file.take(SEALED.len() as u64)
        .read_to_end(&mut head)
        .map_err(|e| format!("a direct mailbox beside the key does not read: {e}"))?;
    Ok(Some(head == SEALED))
}

/// Mint the [`KEY`] in `dir`, marked to stay out of backups from its first moment, and read it
/// back. It is never written over a key: another session's mailbox may have minted it first,
/// and that is the one every mailbox here seals under. Nor is it minted while any mailbox in
/// `dir` is sealed, or may be (one that does not read): every mailbox here is sealed under the
/// one key, so a new one would leave that mail unopened under any key file, and the key put
/// back would leave unopened whatever the new one had sealed.
#[cfg(unix)]
fn mint_key(dir: &std::path::Path) -> Result<Key, String> {
    let judged = aterm_uds::private_file::JudgedDir::judge(dir)
        .map_err(|e| format!("no direct fleet mail key could be made: {e}"))?;
    for name in mailboxes(dir)? {
        if sealed_file(&judged, &name)?.unwrap_or(false) {
            return Err(
                "the direct fleet mail key is missing and a mailbox beside it is sealed \
                        under it: a new key would leave that mail unopened, so none is made; put \
                        the key back, or move every sealed mailbox out of its folder"
                    .to_owned(),
            );
        }
    }
    let mut bytes = [0u8; astream_aead::KEY_LEN];
    let mut marked = Ok(());
    let wrote = aterm_uds::rand::fill(&mut bytes).and_then(|()| {
        aterm_uds::private_file::write_private_new(dir, KEY, &bytes, |file| {
            marked = BACKUPS.exclude(file);
            Ok(())
        })
    });
    bytes.fill(0);
    std::hint::black_box(&mut bytes);
    let wrote = wrote.map_err(|e| format!("no direct fleet mail key could be made: {e}"))?;
    if wrote && let Err(why) = marked {
        aterm_log::warn!(
            "{}: could not be left out of backups ({why}); a backup of this folder holds the key \
             beside the direct fleet mail it seals",
            dir.join(KEY).display()
        );
    }
    read_key(dir)?.ok_or_else(|| "the direct fleet mail key made a moment ago is gone".to_owned())
}

/// The mailbox whose file `name` in `dir` holds `bytes`, and whether they were an earlier
/// build's plaintext. A sealed one opens under the directory's key, which is left in `key`, with
/// `name` as its additional data.
#[cfg(unix)]
fn open_mailbox(
    dir: &std::path::Path,
    name: &str,
    bytes: Vec<u8>,
    key: &mut Option<Key>,
) -> Result<(Mailbox, bool), String> {
    let (text, plaintext) = match bytes.strip_prefix(SEALED) {
        Some(sealed) => {
            let held = read_key(dir)?.ok_or(
                "this session's direct mailbox is sealed and the key beside it is missing",
            )?;
            let text = astream_aead::open(&held.0, name.as_bytes(), sealed).map_err(
                |_| "this session's direct mailbox does not open under the key beside it",
            )?;
            *key = Some(held);
            (text, false)
        }
        None => (bytes, true),
    };
    let boxed = std::str::from_utf8(&text)
        .ok()
        .and_then(|text| aterm_toml::from_str::<Mailbox>(text).ok())
        .ok_or("direct fleet mailbox is malformed")?;
    Ok((boxed, plaintext))
}

/// Publish `boxed` as the mailbox file `name` in `dir`, sealed under `key`, minted into it when
/// it holds none and the directory has none.
#[cfg(unix)]
fn write_mailbox(
    dir: &std::path::Path,
    name: &str,
    boxed: &Mailbox,
    key: &mut Option<Key>,
) -> Result<(), String> {
    let text = aterm_toml::to_string(boxed).map_err(|e| e.to_string())?;
    if text.len() > FILE_BYTES {
        return Err("direct fleet mailbox is full".into());
    }
    seal_file(dir, name, text.as_bytes(), key)
}

/// Publish `text` as the file `name` in `dir`, sealed under `key`, the directory's key read or
/// minted into it when it holds none.
#[cfg(unix)]
fn seal_file(
    dir: &std::path::Path,
    name: &str,
    text: &[u8],
    key: &mut Option<Key>,
) -> Result<(), String> {
    let key = match key {
        Some(key) => key,
        None => key.insert(match read_key(dir)? {
            Some(key) => key,
            None => mint_key(dir)?,
        }),
    };
    let mut file = Vec::with_capacity(text.len() + SEALED.len() + astream_aead::OVERHEAD);
    file.extend_from_slice(SEALED);
    file.extend_from_slice(&astream_aead::seal(&key.0, name.as_bytes(), text));
    aterm_uds::private_file::write_private(dir, name, &file).map_err(|e| e.to_string())
}

#[cfg(unix)]
fn transaction_waiting<T>(
    ctx: &dyn FabricSession,
    within: std::time::Duration,
    edit: impl FnOnce(&mut Mailbox) -> Result<(T, bool), String>,
    waiting: impl FnMut(),
) -> Result<(T, bool), MailError> {
    use aterm_uds::private_file::{PrivateFileError, PrivateLock};
    if !ctx.fleet_mail_ready() {
        return Err("direct fleet mail awaits committed session ownership".into());
    }
    #[cfg(test)]
    TRANSACTIONS.with(|n| n.set(n.get() + 1));
    let path = ctx
        .fleet_mail_path()
        .ok_or("direct fleet mail has no private storage")?;
    let dir = path.parent().ok_or("direct fleet mail has no directory")?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("invalid mailbox name")?;
    aterm_uds::private_file::create_private_dir(dir).map_err(|e| e.to_string())?;
    let _lock = PrivateLock::take_with(dir, &format!("{name}.lock"), within, BUSY, waiting)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                MailError::Busy
            } else {
                MailError::Failed(e.to_string())
            }
        })?;
    let mut key = None;
    let (mut boxed, mut stamp, plaintext) =
        match aterm_uds::private_file::read_private_with_metadata(dir, name, SEALED_BYTES) {
            Ok((bytes, meta)) => {
                let (boxed, plaintext) = open_mailbox(dir, name, bytes, &mut key)?;
                (boxed, Some(Stamp::of(&meta)), plaintext)
            }
            Err(PrivateFileError::Missing) => (Mailbox::default(), None, false),
            Err(error) => return Err(error.to_string().into()),
        };
    if !validated(&boxed) {
        return Err("direct fleet mailbox is malformed".into());
    }
    let migrated = if ctx.fabric().mesh.enabled {
        let aliases = mesh_aliases()?;
        rewrite_nodes(&mut boxed, &aliases)
    } else {
        0
    };
    // A file this writes is a new one, published by rename; a stamp it cannot read caches none,
    // so the next reader takes a transaction.
    if plaintext || migrated > 0 {
        // Sealed before the edit, which may refuse, so this transaction seals it either way.
        write_mailbox(dir, name, &boxed, &mut key)?;
        stamp = self::stamp(&path).ok();
        if migrated > 0 {
            aterm_log::info!("mesh mail rewrite: rows={migrated}");
        }
    }
    let (result, changed) = edit(&mut boxed)?;
    if changed {
        write_mailbox(dir, name, &boxed, &mut key)?;
        stamp = self::stamp(&path).ok();
    }
    // Under the private lock, as the cache is: what entered the ring is what the file holds.
    let imported = ctx.fabric().mesh.enabled && import(ctx, &boxed);
    // Refresh under the private lock so an older read cannot overwrite a newer write's cache.
    let cached = cache(ctx, &boxed, stamp);
    if changed || cached || imported {
        self::changed(ctx);
    }
    Ok((result, changed))
}

/// With the mesh on, take into the session's ring every unread message of `boxed` this process
/// has not imported, and forget the ones the mailbox no longer holds (§11 C6). Whether a row
/// entered. A row waits out a committing update's fence as a bridge's delivery does.
#[cfg(unix)]
fn import(ctx: &dyn FabricSession, boxed: &Mailbox) -> bool {
    let fabric = ctx.fabric();
    let mut inbox = fabric.wait_fenced_delivery(fabric.lock(), ctx.self_id().as_str(), None);
    inbox.fleet_imported.retain(|(id, key)| {
        boxed
            .rows
            .iter()
            .any(|row| row.id == *id && row.key == *key)
    });
    let mut entered = false;
    for row in boxed.rows.iter().filter(|row| !row.listed) {
        if inbox.fleet_imported.insert((row.id, row.key.clone())) {
            inbox.import_fleet(row.id, &row.node, &row.kind, &row.text);
            entered = true;
        }
    }
    entered
}

/// Mark the imported messages `ids`, which the session's ring has now listed, listed in the
/// sealed mailbox (§11 C6): the import's mark, so no later start, restart or successor imports
/// them again. Nothing to mark takes no transaction.
/// # Errors
/// The mailbox is busy past a minute, unreadable or malformed: the messages stay unlisted there,
/// and the next start offers them again.
pub(crate) fn mark_listed(ctx: &dyn FabricSession, ids: &[u64]) -> Result<(), String> {
    #[cfg(all(test, unix))]
    if UNMARKED.with(std::cell::Cell::get) {
        return Ok(());
    }
    if ids.is_empty() {
        return Ok(());
    }
    transaction(ctx, |boxed| {
        let mut moved = false;
        for row in boxed.rows.iter_mut().filter(|row| ids.contains(&row.id)) {
            moved |= !row.listed;
            row.listed = true;
        }
        Ok(((), moved))
    })
    .map(|_| ())
}

/// A post to a session on a pre-R3 Mac ([`PeerMail::Fleet`]), as `post` read it.
pub(crate) struct Carry<'a> {
    /// The session, on `node`.
    pub sid: &'a str,
    /// The Mac.
    pub node: &'a str,
    pub kind: &'a str,
    /// The idempotency key the poster gave, if any.
    pub key: Option<&'a str>,
    pub text: &'a str,
    /// The text came on the post's line, not as `len=` bytes.
    pub inline: bool,
    /// The post asks nothing direct fleet mail cannot carry: no `re=`, `dl=`, `via=` or
    /// `--wait-ack`.
    pub plain: bool,
}

/// Carry `post` by direct fleet mail (§11 C6): one fleet drive to its Mac, which posts it into
/// the session's mailbox there as `fleet:<this node>`, and keeps the session's reply for this
/// Mac's next drive (`inbox fleet` there). A post with no key gets one of this session's
/// (`<sid>.<16 hex>`), which the reply names, so it can be posted again once.
pub(crate) fn carry(ctx: &dyn FabricSession, post: &Carry<'_>) -> String {
    if !kind(post.kind) {
        return refusal(format!(
            "{} is a pre-R3 Mac, reached by direct fleet mail, which takes kind=ask, task, note, \
             answer or report",
            post.node
        ));
    }
    if !post.plain {
        return refusal(format!(
            "{} is a pre-R3 Mac, reached by direct fleet mail, which carries no re=, dl=, via= \
             or --wait-ack",
            post.node
        ));
    }
    if !post.inline
        || post.text.is_empty()
        || post.text.len() > TEXT
        || post.text.contains(['\n', '\r'])
    {
        return refusal(format!(
            "{} is a pre-R3 Mac, reached by direct fleet mail, which takes inline text of \
             1..{TEXT} bytes on one line",
            post.node
        ));
    }
    let key = match post.key {
        Some(key) => key.to_owned(),
        None => match minted_key(ctx) {
            Ok(key) => key,
            Err(why) => return refusal(why),
        },
    };
    match drive_post(ctx, post.sid, post.node, post.kind, &key, post.text) {
        Ok(Carried { id, dup }) => format!(
            "OK fleet=f{id} to=@{}@{} key={key}{}\n",
            post.sid,
            post.node,
            if dup { " dup=1" } else { "" }
        ),
        Err(Uncarried::NoDrive(why)) => refusal(format!(
            "{} is a pre-R3 Mac, reached by direct fleet mail, and no drive reached it: {why}",
            post.node
        )),
        Err(Uncarried::Answered(answer)) => refusal(format!(
            "{} answered the direct fleet mail: {answer}",
            post.node
        )),
    }
}

/// A message direct fleet mail took ([`drive_post`]): its id in the mailbox on the other Mac, and
/// whether that mailbox already held it under the same key (`dup=1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Carried {
    pub id: u64,
    pub dup: bool,
}

/// Why direct fleet mail did not take a message ([`drive_post`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Uncarried {
    /// No drive reached the Mac, or it hung up before it answered.
    NoDrive(String),
    /// The Mac answered with something other than the message's id: its refusal, at most 512
    /// characters of it.
    Answered(String),
}

/// One direct-mail post of `text` (`kind`, one line, under `key`) to session `sid` on `node`: one
/// fleet drive to that Mac, which posts it into the session's mailbox there as `fleet:<this
/// node>` ([`post`] under its driver). A message it took for the first time is this session's
/// [`super::FabricFact::FleetMailOut`]; a duplicate is the one an earlier post carried, already on
/// the log.
pub(crate) fn drive_post(
    ctx: &dyn FabricSession,
    sid: &str,
    node: &str,
    kind: &str,
    key: &str,
    text: &str,
) -> Result<Carried, Uncarried> {
    let request = format!("@{sid} post key={key} kind={kind} -- {text}");
    let answer = ctx
        .fleet_drive(node, &request)
        .map_err(Uncarried::NoDrive)?;
    let answer = answer.trim_end_matches(['\r', '\n']);
    let mut words = answer.split(' ');
    match (
        words.next(),
        words.next().and_then(id),
        words.next(),
        words.next(),
    ) {
        (Some("OK"), Some(id), dup @ (None | Some("dup=1")), None) => {
            if dup.is_none() {
                ctx.record_fact(
                    "post",
                    super::FabricFact::FleetMailOut {
                        id,
                        kind,
                        to: &format!("@{sid}@{node}"),
                        idem: key,
                    },
                );
            }
            Ok(Carried {
                id,
                dup: dup.is_some(),
            })
        }
        _ => Err(Uncarried::Answered(answer.chars().take(512).collect())),
    }
}

#[cfg(not(unix))]
fn minted_key(_ctx: &dyn FabricSession) -> Result<String, String> {
    Err("direct fleet mail is available on Unix".to_owned())
}

/// A key for a post that gave none ([`carry`]): this session's sid and 16 random hex digits, so
/// no other session of this Mac, which every one shares as the sender there, names it.
#[cfg(unix)]
fn minted_key(ctx: &dyn FabricSession) -> Result<String, String> {
    let mut nonce = [0u8; 8];
    aterm_uds::rand::fill(&mut nonce)
        .map_err(|e| format!("no key could be made for the direct fleet mail: {e}"))?;
    let mut key = format!("{}.", ctx.self_id().as_str());
    for byte in nonce {
        key.push_str(&format!("{byte:02x}"));
    }
    if super::valid_key(&key) {
        Ok(key)
    } else {
        Err("this session's id makes no direct fleet mail key".to_owned())
    }
}

#[cfg(unix)]
fn mesh_aliases() -> Result<Vec<(String, String)>, String> {
    use aterm_uds::private_file::{PrivateFileError, read_private};
    let Some(dir) = aterm_types::dirs::state_dir() else {
        return Ok(Vec::new());
    };
    match read_private(&dir, "mesh-node-aliases", 1024 * 1024) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
            text.lines()
                .map(|line| {
                    let (old, new) = line.split_once(' ').ok_or("malformed mesh node alias")?;
                    if !valid_node(old) || !new.starts_with("n-") || !valid_node(new) {
                        return Err("malformed mesh node alias".to_owned());
                    }
                    Ok((old.to_owned(), new.to_owned()))
                })
                .collect()
        }
        Err(PrivateFileError::Missing) => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(unix)]
fn rewrite_nodes(boxed: &mut Mailbox, aliases: &[(String, String)]) -> usize {
    let mut changed = 0;
    for row in &mut boxed.rows {
        if let Some((_, new)) = aliases
            .iter()
            .find(|(old, new)| old == &row.node && old != new)
        {
            row.node.clone_from(new);
            changed += 1;
        }
    }
    changed
}

#[cfg(not(unix))]
fn transaction_within<T>(
    _ctx: &dyn FabricSession,
    _within: std::time::Duration,
    _edit: impl FnOnce(&mut Mailbox) -> Result<(T, bool), String>,
) -> Result<(T, bool), MailError> {
    Err("direct fleet mail is available on Unix".into())
}

fn reply<T: AsRef<str>>(result: Result<T, String>) -> String {
    match result {
        Ok(text) => text.as_ref().to_owned(),
        Err(why) => format!(
            "ERR fleet-mail: {}\n",
            why.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(1024)
                .collect::<String>()
        ),
    }
}

struct Post<'a> {
    key: &'a str,
    kind: &'a str,
    text: &'a str,
    re: Option<u64>,
}

fn parse_post(rest: &str, answering: bool) -> Result<Post<'_>, String> {
    let (options, text) = rest.split_once(" -- ").ok_or(
        "use key=<token> kind=<kind> -- <text>, or re=f<id> key=<token> kind=<kind> -- <text>",
    )?;
    let (mut key, mut message_kind, mut re) = (None, None, None);
    for word in options.split_whitespace() {
        if let Some(value) = word.strip_prefix("key=").filter(|v| super::valid_key(v)) {
            if key.replace(value).is_some() {
                return Err("duplicate key".to_owned());
            }
        } else if let Some(value) = word.strip_prefix("kind=").filter(|v| kind(v)) {
            if message_kind.replace(value).is_some() {
                return Err("duplicate kind".to_owned());
            }
        } else if let Some(value) = word.strip_prefix("re=").and_then(id).filter(|_| answering) {
            if re.replace(value).is_some() {
                return Err("duplicate reply id".to_owned());
            }
        } else {
            return Err("invalid direct-mail option".to_owned());
        }
    }
    if text.is_empty() || text.len() > TEXT {
        return Err("text must contain 1..4096 bytes".to_owned());
    }
    let (Some(key), Some(message_kind)) = (key, message_kind) else {
        return Err("direct mail requires key and kind".to_owned());
    };
    if answering != re.is_some() {
        return Err("a local reply requires re=f<id>".to_owned());
    }
    Ok(Post {
        key,
        kind: message_kind,
        text,
        re,
    })
}

/// Whether this local post addresses a retained direct message instead of the bus.
pub fn is_reply(rest: &str) -> bool {
    aterm_types::control_verbs::post_options(rest.split_whitespace())
        .any(|word| word.starts_with("re=f"))
}

/// Deliver from the proved remote node, or retain the local recipient's answer for that node.
/// Each retained key is idempotent: the same key with other bytes refuses, including after a restart.
pub fn post(ctx: &dyn FabricSession, rest: &str) -> String {
    let driver = serving_driver();
    if driver.as_ref().is_some_and(|d| !valid_node(&d.node)) {
        return "ERR denied: direct mail requires a proved fleet node\n".to_owned();
    }
    let parsed = match parse_post(rest, driver.is_none()) {
        Ok(post) => post,
        Err(why) => return reply::<String>(Err(why)),
    };
    let mut arrival = None;
    let mut answered = None;
    let result = transaction(ctx, |boxed| {
        if let Some(driver) = &driver {
            if let Some(row) = boxed
                .rows
                .iter()
                .find(|r| r.node == driver.node && r.key == parsed.key)
            {
                return if row.kind == parsed.kind && row.text == parsed.text {
                    Ok((format!("OK f{} dup=1\n", row.id), false))
                } else {
                    Err("that key already names another message".to_owned())
                };
            }
            if boxed.rows.len() >= ROWS
                || boxed.rows.iter().filter(|r| r.node == driver.node).count() >= PER_NODE
            {
                // Listing consumes mail without a reply; a retained reply still belongs to
                // its sender until acknowledged. Reclaim only under pressure, after checking
                // retained idempotency keys, so read progress notes cannot fill the mailbox.
                boxed
                    .rows
                    .retain(|row| !row.reply_seen && (!row.listed || row.reply.is_some()));
            }
            if boxed.rows.len() >= ROWS
                || boxed.rows.iter().filter(|r| r.node == driver.node).count() >= PER_NODE
            {
                return Err("direct mailbox is full; the recipient can retire listed conversations with inbox seen f<id> forget".to_owned());
            }
            boxed.next = boxed
                .next
                .checked_add(1)
                .ok_or("direct-mail id space is exhausted")?;
            let id = boxed.next;
            arrival = Some((id, format!("fleet:{}", driver.node)));
            boxed.rows.push(Row {
                id,
                node: driver.node.clone(),
                key: parsed.key.to_owned(),
                kind: parsed.kind.to_owned(),
                text: parsed.text.to_owned(),
                listed: false,
                reply: None,
                reply_seen: false,
            });
            Ok((format!("OK f{id}\n"), true))
        } else {
            let id = parsed.re.ok_or("a local reply requires re=f<id>")?;
            let row = boxed
                .rows
                .iter_mut()
                .find(|r| r.id == id)
                .ok_or("no such direct message")?;
            if let Some(was) = &row.reply {
                return if was.key == parsed.key
                    && was.kind == parsed.kind
                    && was.text == parsed.text
                {
                    Ok((format!("OK f{id} reply=1 dup=1\n"), false))
                } else {
                    Err("this message already has another reply".to_owned())
                };
            }
            row.reply = Some(Reply {
                key: parsed.key.to_owned(),
                kind: parsed.kind.to_owned(),
                text: parsed.text.to_owned(),
            });
            row.listed = true;
            answered = Some(id);
            Ok((format!("OK f{id} reply=1\n"), true))
        }
    });
    if result.is_ok()
        && let Some((id, from)) = arrival
    {
        ctx.record_fact(
            "mail",
            super::FabricFact::FleetMailIn {
                id,
                from: &from,
                kind: parsed.kind,
            },
        );
    }
    if result.is_ok()
        && let Some(id) = answered
    {
        ctx.record_fact("mail-seen", super::FabricFact::FleetMailSeen { id });
    }
    finish(ctx, result)
}

fn finish(_ctx: &dyn FabricSession, result: Result<(String, bool), String>) -> String {
    reply(result.map(|(text, _)| text))
}

#[cfg(unix)]
fn changed(ctx: &dyn FabricSession) {
    #[cfg(test)]
    if UNRUNG.with(std::cell::Cell::get) {
        return;
    }
    let guard = ctx.fabric().lock();
    ctx.fabric()
        .direct_epoch
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    ctx.fabric().changed.ring(guard);
    ctx.fabric_changed();
}

fn render(row: &Row, meta: bool) -> String {
    let mut line = format!(
        "msg f{} from=fleet:{} kind={} trust=agent key={} len={} reply={} reply_seen={}",
        row.id,
        row.node,
        row.kind,
        row.key,
        row.text.len(),
        u8::from(row.reply.is_some()),
        u8::from(row.reply_seen)
    );
    if !meta {
        line.push_str(&format!(
            " text={}",
            aterm_control::wire::pct_encode(&row.text)
        ));
    }
    if let Some(answer) = &row.reply {
        line.push_str(&format!(
            " reply_kind={} reply_len={}",
            answer.kind,
            answer.text.len()
        ));
        if !meta {
            line.push_str(&format!(
                " reply_text={}",
                aterm_control::wire::pct_encode(&answer.text)
            ));
        }
    }
    line.push('\n');
    line
}

/// One direct-mail page, including rows omitted by a normal inbox's shared row limit.
#[derive(Default)]
pub struct View {
    /// The rendered rows, with a header for a standalone direct read.
    pub text: String,
    /// Rows included in `text`.
    pub count: usize,
    /// Direct rows omitted by the page limit.
    pub pending: usize,
}

/// Format a direct-mail refusal without including untrusted parser excerpts.
pub fn refusal(why: String) -> String {
    reply::<String>(Err(why))
}

/// Remote reads are restricted to the authenticated node; local reads see every sender.
pub fn inbox(ctx: &dyn FabricSession, rest: &str, combined: bool) -> String {
    reply(view(ctx, rest, combined).map(|page| page.text))
}

/// Read direct rows; `combined` appends unlisted arrivals to the normal inbox without
/// consuming a broker cursor. A standalone fleet inbox can reread retained conversations.
/// # Errors
/// Invalid options or unavailable or malformed private storage.
pub fn view(ctx: &dyn FabricSession, rest: &str, combined: bool) -> Result<View, String> {
    if combined && ctx.fleet_mail_path().is_none() {
        return Ok(View::default());
    }
    let driver = serving_driver();
    let (mut since, mut peek, mut meta) = (0, false, false);
    let mut limit = ROWS;
    for word in rest.split_whitespace() {
        if word == "--peek" {
            peek = true;
        } else if word == "--meta" {
            meta = true;
        } else if let Some(value) = word
            .strip_prefix("since=f")
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|v| v.parse::<u64>().ok())
        {
            since = value;
        } else if word.bytes().all(|b| b.is_ascii_digit())
            && let Ok(value) = word.parse::<usize>()
        {
            limit = value.min(ROWS);
        } else if combined && word.starts_with("since=") {
        } else {
            return Err("use inbox fleet [since=f<id>] [<n>] [--peek] [--meta]".to_owned());
        }
    }
    // A combined page with no row to list or count is empty, and needs no transaction; one
    // with a row takes it, which lists the row and marks it.
    #[cfg(unix)]
    if combined
        && let Some(path) = ctx.fleet_mail_path()
        && let Some(page) = from_cache(ctx, &path, |meta| {
            let from = driver.as_ref().map(|d| format!("fleet:{}", d.node));
            let none = !meta.rows.iter().any(|r| {
                from.as_ref().is_none_or(|from| *from == r.from) && r.id > since && !r.listed
            });
            none.then(View::default)
        })
    {
        return Ok(page);
    }
    let result = transaction(ctx, |boxed| {
        let allowed = |r: &&Row| {
            driver.as_ref().is_none_or(|d| d.node == r.node)
                && r.id > since
                && (!combined || !r.listed)
        };
        let ids: std::collections::BTreeSet<u64> = boxed
            .rows
            .iter()
            .filter(allowed)
            .rev()
            .take(limit)
            .map(|r| r.id)
            .collect();
        let pending = boxed
            .rows
            .iter()
            .filter(allowed)
            .filter(|row| !row.listed && !ids.contains(&row.id))
            .count();
        let mut text = String::new();
        let mut moved = false;
        for row in &mut boxed.rows {
            if ids.contains(&row.id) {
                text.push_str(&render(row, meta));
                if driver.is_none() && !peek {
                    moved |= !row.listed;
                    row.listed = true;
                }
            }
        }
        if combined {
            return Ok((
                View {
                    text,
                    count: ids.len(),
                    pending,
                },
                moved,
            ));
        }
        let head = boxed
            .rows
            .iter()
            .filter(|r| driver.as_ref().is_none_or(|d| d.node == r.node))
            .map(|r| r.id)
            .max()
            .unwrap_or(0);
        Ok((
            View {
                text: format!("OK {} fleet=1 head=f{head}\n{text}", ids.len()),
                count: ids.len(),
                pending,
            },
            moved,
        ))
    });
    result.map(|(page, _)| page)
}

/// A sender explicitly acknowledges a reply it has read; no other node can acknowledge it.
pub fn acknowledge(ctx: &dyn FabricSession, word: &str) -> String {
    let Some(driver) = serving_driver() else {
        return "ERR denied: the sending node acknowledges its reply\n".to_owned();
    };
    let Some(id) = id(word.trim()) else {
        return "ERR usage: inbox seen f<id>\n".to_owned();
    };
    let result = transaction(ctx, |boxed| {
        let row = boxed
            .rows
            .iter_mut()
            .find(|r| r.id == id && r.node == driver.node)
            .ok_or("no such direct message")?;
        if row.reply.is_none() {
            return Err("this direct message has no reply to acknowledge".to_owned());
        }
        let moved = !row.reply_seen;
        row.reply_seen = true;
        Ok((format!("OK seen=f{id}\n"), moved))
    });
    finish(ctx, result)
}

/// The recipient may retire a conversation it has listed, without depending on the sender
/// remaining online to answer or acknowledge it. Unread messages cannot be discarded.
pub fn forget(ctx: &dyn FabricSession, word: &str) -> String {
    if serving_driver().is_some() {
        return "ERR denied: only the recipient retires direct mail\n".to_owned();
    }
    let Some(id) = id(word.trim()) else {
        return "ERR usage: inbox seen f<id> forget\n".to_owned();
    };
    let mut unread = false;
    let result = transaction(ctx, |boxed| {
        let row = boxed
            .rows
            .iter()
            .find(|r| r.id == id)
            .ok_or("no such direct message")?;
        if !row.listed {
            return Err("list this direct message before retiring it".to_owned());
        }
        unread = row.reply.is_none();
        boxed.rows.retain(|r| r.id != id);
        Ok((format!("OK forgot=f{id}\n"), true))
    });
    if result.is_ok() && unread {
        ctx.record_fact("mail-seen", super::FabricFact::FleetMailSeen { id });
    }
    finish(ctx, result)
}

/// The newest unlisted direct message past `after` a normal inbox wait may wake for, from the
/// cache while the mailbox file is the one it describes ([`from_cache`]).
/// # Errors
/// The private mailbox is unreadable or malformed. Contention is no observation this round;
/// the wait retries when it is rung: by a write in this process, or by the session's fleet-mail
/// watcher, whose refresh sees a handoff peer's write ([`restore`]).
pub fn pending(
    ctx: &dyn FabricSession,
    after: u64,
    accepts: impl Fn(&str) -> bool,
) -> Result<Option<u64>, String> {
    if serving_driver().is_some() {
        return Ok(None);
    }
    let Some(path) = ctx.fleet_mail_path() else {
        return Ok(None);
    };
    #[cfg(unix)]
    if let Some(id) = from_cache(ctx, &path, |meta| {
        Some(
            meta.rows
                .iter()
                .filter(|r| r.id > after && !r.listed && accepts(&r.kind))
                .map(|r| r.id)
                .max(),
        )
    }) {
        return Ok(id);
    }
    #[cfg(not(unix))]
    let _ = path;
    let read = {
        #[cfg(unix)]
        let _span = verb_span();
        transaction_within(ctx, std::time::Duration::ZERO, |boxed| {
            Ok((
                boxed
                    .rows
                    .iter()
                    .filter(|r| r.id > after && !r.listed && accepts(&r.kind))
                    .map(|r| r.id)
                    .max(),
                false,
            ))
        })
    };
    match read {
        Ok((id, _)) => Ok(id),
        #[cfg(unix)]
        Err(MailError::Busy) => Ok(None),
        Err(why) => Err(why.to_string()),
    }
}

#[cfg(all(test, unix))]
#[path = "fleet_mail_tests.rs"]
pub(super) mod tests;
