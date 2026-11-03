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

use super::{FabricSession, serving_driver};
use serde::{Deserialize, Serialize};

const ROWS: usize = 64;
const PER_NODE: usize = 16;
const TEXT: usize = 4096;
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
    use aterm_uds::private_file::{PrivateFileError, check_private_dir, open_private};
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("the mailbox has no directory"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("the mailbox has no file name"))?;
    let none = || std::io::Error::new(std::io::ErrorKind::NotFound, "no mailbox");
    let (_file, metadata) = open_private(dir, name).map_err(|error| match error {
        // `open_private` answers a missing file before it judges an existing directory's
        // owner, mode and access list, so absence is judged against those rules here.
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
    /// A negative control's switch: this thread's refreshes ring no waiter.
    static UNRUNG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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

fn valid_node(node: &str) -> bool {
    node.len() == 32
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
    let mut head = Vec::with_capacity(SEALED.len());
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
    // A file this writes is a new one, published by rename; a stamp it cannot read caches none,
    // so the next reader takes a transaction.
    if plaintext {
        // Sealed before the edit, which may refuse, so this transaction seals it either way.
        write_mailbox(dir, name, &boxed, &mut key)?;
        stamp = self::stamp(&path).ok();
    }
    let (result, changed) = edit(&mut boxed)?;
    if changed {
        write_mailbox(dir, name, &boxed, &mut key)?;
        stamp = self::stamp(&path).ok();
    }
    // Refresh under the private lock so an older read cannot overwrite a newer write's cache.
    let cached = cache(ctx, &boxed, stamp);
    if changed || cached {
        self::changed(ctx);
    }
    Ok((result, changed))
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
    super::post_options(rest.split_whitespace()).any(|word| word.starts_with("re=f"))
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
                // Both participants have received these completed conversations. Reclaim
                // them only under pressure, after checking retained idempotency keys.
                boxed.rows.retain(|row| !row.reply_seen);
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
    ctx.fabric().changed.notify_all();
    drop(guard);
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
mod tests {
    use super::*;
    use crate::{Driver, DrivingAs, FabricFact, SessionFabric};
    use aterm_session::SessionId;

    struct Session {
        sid: SessionId,
        fabric: SessionFabric,
        path: std::path::PathBuf,
        facts: std::sync::Mutex<Vec<String>>,
        wakes: std::sync::atomic::AtomicU64,
        ready: std::sync::atomic::AtomicBool,
    }

    impl FabricSession for Session {
        fn self_id(&self) -> &SessionId {
            &self.sid
        }
        fn fabric(&self) -> &SessionFabric {
            &self.fabric
        }
        fn record_fact(&self, _: &'static str, fact: FabricFact<'_>) -> bool {
            self.facts
                .lock()
                .unwrap()
                .push(crate::test_support::said(fact));
            false
        }
        fn fabric_changed(&self) {
            self.wakes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn holder_token(&self) -> Option<String> {
            None
        }
        fn fleet_mail_path(&self) -> Option<std::path::PathBuf> {
            Some(self.path.clone())
        }
        fn fleet_mail_ready(&self) -> bool {
            self.ready.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn session(path: &std::path::Path) -> Session {
        Session {
            sid: SessionId::new("s-12345678"),
            fabric: SessionFabric::default(),
            path: path.to_owned(),
            facts: Default::default(),
            wakes: Default::default(),
            ready: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn as_node<T>(byte: &str, f: impl FnOnce() -> T) -> T {
        let _driver = DrivingAs::enter(Driver {
            node: byte.repeat(32),
            host: "peer.local".to_owned(),
        });
        f()
    }

    #[test]
    fn direct_arrival_discovers_once_without_reading_storage_on_presence_or_subscription() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let receiver = session(&path);
        assert_eq!(
            as_node("a", || post(
                &receiver,
                "key=notice kind=ask -- PRIVATE_BODY"
            )),
            "OK f1\n"
        );
        assert_eq!(receiver.fabric.mail_facts().unread, 1);
        assert_eq!(receiver.fabric.mail_facts().head, 1);
        assert_eq!(
            receiver.fabric.mail_facts().last.unwrap().from,
            format!("fleet:{}", "a".repeat(32))
        );
        let (high, rows) = receiver
            .fabric
            .direct_mail_since(0, &crate::MailFilter::default());
        assert_eq!(high, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 1);
        assert_eq!(receiver.facts.lock().unwrap().len(), 1);
        assert!(!receiver.facts.lock().unwrap()[0].contains("PRIVATE_BODY"));
        assert_eq!(receiver.wakes.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(
            as_node("a", || post(
                &receiver,
                "key=notice kind=ask -- PRIVATE_BODY"
            )),
            "OK f1 dup=1\n"
        );
        assert_eq!(receiver.facts.lock().unwrap().len(), 1);
        assert_eq!(receiver.wakes.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(
            receiver
                .fabric
                .direct_mail_since(1, &crate::MailFilter::default())
                .1
                .is_empty()
        );
        for filter in [
            crate::MailFilter {
                topic: Some("build".to_owned()),
                ..Default::default()
            },
            crate::MailFilter {
                from: Some("human".to_owned()),
                ..Default::default()
            },
            crate::MailFilter {
                kinds: vec!["note".to_owned()],
                ..Default::default()
            },
        ] {
            assert!(receiver.fabric.direct_mail_since(0, &filter).1.is_empty());
        }
        let successor = session(&path);
        restore(&successor);
        assert_eq!(successor.fabric.mail_facts().unread, 1);
        assert_eq!(
            successor.wakes.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert!(
            successor.facts.lock().unwrap().is_empty(),
            "restoring discovery must not repeat carried timeline marks"
        );
        assert_eq!(
            as_node("b", || post(
                &receiver,
                "key=later kind=note -- handoff arrival"
            )),
            "OK f2\n"
        );
        assert_eq!(
            successor.fabric.mail_facts().unread,
            1,
            "the other process has its own cache"
        );
        assert!(restore(&successor));
        assert_eq!(
            successor.fabric.mail_facts().unread,
            2,
            "a write after adoption is discovered on refresh"
        );
        assert_eq!(
            successor
                .fabric
                .direct_mail_since(1, &crate::MailFilter::default())
                .1[0]
                .id,
            2
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            successor.fabric.mail_facts().unread,
            2,
            "GUI reads only cached metadata"
        );
        assert_eq!(
            successor
                .fabric
                .direct_mail_since(0, &crate::MailFilter::default())
                .1
                .len(),
            2
        );
    }

    #[test]
    fn answering_or_retiring_direct_mail_clears_cached_pending_without_reusing_bus_cursors() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        assert_eq!(
            as_node("a", || post(&receiver, "key=one kind=ask -- question")),
            "OK f1\n"
        );
        let _ = crate::cmd_inbox(&receiver, "");
        assert_eq!(receiver.fabric.mail_facts().pending, 1);
        assert_eq!(
            post(&receiver, "re=f1 key=r kind=answer -- answer"),
            "OK f1 reply=1\n"
        );
        assert_eq!(receiver.fabric.mail_facts().unread, 0);
        assert_eq!(receiver.fabric.mail_facts().pending, 0);
        assert_eq!(receiver.fabric.last_msg_id(), 0);
        assert_eq!(
            as_node("a", || post(&receiver, "key=two kind=note -- note")),
            "OK f2\n"
        );
        let _ = crate::cmd_inbox(&receiver, "");
        assert_eq!(forget(&receiver, "f2"), "OK forgot=f2\n");
        assert_eq!(receiver.fabric.mail_facts().unread, 0);
        assert_eq!(receiver.fabric.last_direct_mail_id(), 2);
        assert_eq!(
            receiver
                .facts
                .lock()
                .unwrap()
                .iter()
                .filter(|f| f.contains("moved=1"))
                .count(),
            2
        );
    }

    #[test]
    fn a_failed_refresh_is_retryable_and_keeps_the_last_successful_snapshot() {
        use aterm_uds::private_file::PrivateLock;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let receiver = session(&path);
        assert_eq!(
            as_node("a", || post(&receiver, "key=one kind=ask -- question")),
            "OK f1\n"
        );
        let successor = session(&path);
        let held = PrivateLock::take(
            path.parent().unwrap(),
            "inbox.toml.lock",
            std::time::Duration::ZERO,
            "test lock",
        )
        .unwrap();
        assert!(
            !restore(&successor),
            "a busy observation must not commit its stamp"
        );
        assert_eq!(successor.fabric.last_direct_mail_id(), 0);
        drop(held);
        assert!(restore(&successor));
        assert_eq!(successor.fabric.last_direct_mail_id(), 1);
        std::fs::write(&path, "not valid toml").unwrap();
        assert!(
            !restore(&successor),
            "malformed observations must not commit their stamp"
        );
        assert_eq!(successor.fabric.mail_facts().unread, 1);
    }

    #[test]
    fn a_peer_posts_and_reads_its_reply_without_reading_another_peers_mail() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        assert_eq!(
            as_node("a", || crate::cmd_post(
                &receiver,
                "key=one kind=ask -- build status?",
                None
            )),
            "OK f1\n"
        );
        assert_eq!(
            as_node("b", || crate::cmd_post(
                &receiver,
                "key=two kind=note -- unrelated",
                None
            )),
            "OK f2\n"
        );
        let read = as_node("a", || crate::cmd_inbox_verb(&receiver, ""));
        assert!(read.contains("build%20status?"), "{read}");
        assert!(!read.contains("unrelated"), "{read}");
        let local = crate::cmd_inbox(&receiver, "");
        assert!(
            local.contains("msg f1 ") && local.contains("msg f2 "),
            "{local}"
        );
        assert_eq!(
            crate::cmd_post(&receiver, "re=f1 key=answer kind=answer -- complete", None),
            "OK f1 reply=1\n"
        );
        let reply = as_node("a", || crate::cmd_inbox_verb(&receiver, ""));
        assert!(reply.contains("reply_text=complete"), "{reply}");
        assert!(!as_node("b", || crate::cmd_inbox_verb(&receiver, "")).contains("complete"));
    }

    #[test]
    fn durable_keys_and_replies_survive_a_successor_using_the_same_session_path() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let before = session(&path);
        assert_eq!(
            as_node("a", || post(&before, "key=k kind=ask -- question")),
            "OK f1\n"
        );
        let successor = session(&path);
        assert_eq!(
            as_node("a", || post(&successor, "key=k kind=ask -- question")),
            "OK f1 dup=1\n"
        );
        assert!(as_node("a", || post(&successor, "key=k kind=ask -- changed")).starts_with("ERR "));
        assert_eq!(
            post(&successor, "re=f1 key=r kind=answer -- answer"),
            "OK f1 reply=1\n"
        );
        let again = session(&path);
        assert_eq!(
            post(&again, "re=f1 key=r kind=answer -- answer"),
            "OK f1 reply=1 dup=1\n"
        );
        assert!(as_node("a", || inbox(&again, "", false)).contains("reply_text=answer"));
    }

    #[test]
    fn no_local_sender_forges_a_peer_and_no_peer_replies_or_retires_another_conversation() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        assert!(post(&receiver, "key=k kind=ask -- forged").starts_with("ERR "));
        assert!(
            as_node("a", || post(
                &receiver,
                "from=other key=k kind=ask -- forged"
            ))
            .starts_with("ERR ")
        );
        assert_eq!(
            as_node("a", || post(&receiver, "key=k kind=ask -- question")),
            "OK f1\n"
        );
        assert!(
            as_node("b", || post(&receiver, "re=f1 key=r kind=answer -- forged"))
                .starts_with("ERR ")
        );
        assert!(as_node("b", || forget(&receiver, "f1")).starts_with("ERR denied"));
        assert!(forget(&receiver, "f1").starts_with("ERR "));
    }

    #[test]
    fn bounded_mailbox_refuses_without_losing_waiting_messages_and_wakes_normal_inbox_waits() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        for n in 0..PER_NODE {
            assert!(
                as_node("a", || post(
                    &receiver,
                    &format!("key=k{n} kind=ask -- question")
                ))
                .starts_with("OK ")
            );
        }
        assert!(
            as_node("a", || post(&receiver, "key=overflow kind=ask -- question"))
                .starts_with("ERR ")
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK inbox 0 fleet=f16\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0", "fleet_since=f16"], 0),
            "OK timeout\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=f16"], 0),
            "OK timeout\n"
        );
        assert!(crate::cmd_inbox(&receiver, "").contains("msg f16 "));
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK timeout\n"
        );
        assert_eq!(
            post(&receiver, "re=f1 key=r kind=answer -- answer"),
            "OK f1 reply=1\n"
        );
        assert!(as_node("b", || acknowledge(&receiver, "f1")).starts_with("ERR "));
        assert_eq!(
            as_node("a", || acknowledge(&receiver, "f1")),
            "OK seen=f1\n"
        );
        assert_eq!(forget(&receiver, "f1"), "OK forgot=f1\n");
        assert_eq!(
            as_node("a", || post(&receiver, "key=after kind=note -- after")),
            "OK f17\n"
        );
    }

    #[test]
    fn damaged_durable_state_refuses_and_is_not_replaced() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        as_node("a", || post(&receiver, "key=k kind=ask -- question"));
        assert_eq!(
            crate::deliver_row(
                &receiver,
                &[
                    "off=8",
                    "from=s-a@n-a",
                    "kind=ask",
                    "trust=agent",
                    "text=bus"
                ],
                &|| {}
            ),
            "OK 1\n"
        );
        std::fs::write(&receiver.path, b"other-peer-private-message = [").unwrap();
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK inbox 1\n"
        );
        assert!(crate::cmd_await_inbox(&receiver, &["since=f0"], 0).starts_with("ERR "));
        let page = crate::cmd_inbox(&receiver, "");
        assert!(
            page.starts_with("OK 1 ") && page.contains("fleet_error=unavailable"),
            "{page}"
        );
        assert!(page.contains("text=bus"), "{page}");
        assert!(
            receiver.fabric.lock().rows[0].listed,
            "unrelated damaged direct storage does not block bus delivery"
        );
        let refused = as_node("a", || post(&receiver, "key=k2 kind=ask -- question"));
        assert!(
            refused.starts_with("ERR ") && !refused.contains("other-peer-private-message"),
            "{refused}"
        );
        assert_eq!(
            std::fs::read(&receiver.path).unwrap(),
            b"other-peer-private-message = ["
        );
    }

    #[test]
    fn bus_and_direct_cursors_can_be_rearmed_independently() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        assert_eq!(
            crate::deliver_row(
                &receiver,
                &[
                    "off=8",
                    "from=s-a@n-a",
                    "kind=ask",
                    "trust=agent",
                    "text=bus"
                ],
                &|| {}
            ),
            "OK 1\n"
        );
        assert_eq!(
            as_node("a", || post(&receiver, "key=k kind=ask -- direct")),
            "OK f1\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK inbox 1\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=1"], 0),
            "OK inbox 1 fleet=f1\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=1", "fleet_since=f1"], 0),
            "OK timeout\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=f1"], 0),
            "OK timeout\n"
        );
        for bad in [
            &["since=f1", "fleet_since=f1"][..],
            &["fleet_since=f1", "since=f1"][..],
            &["since=f1", "since=1"][..],
            &["since=1", "since=f1"][..],
            &["re=8", "fleet_since=f1"][..],
            &["since=0", "topic=lab", "fleet_since=f1"][..],
        ] {
            assert!(
                crate::cmd_await_inbox(&receiver, bad, 0).starts_with("ERR usage"),
                "{bad:?}"
            );
        }
        assert_eq!(
            crate::deliver_row(
                &receiver,
                &[
                    "off=9",
                    "from=s-a@n-a",
                    "kind=ask",
                    "trust=agent",
                    "text=next"
                ],
                &|| {}
            ),
            "OK 2\n"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=1", "fleet_since=f1"], 0),
            "OK inbox 2\n"
        );
    }

    #[test]
    fn a_small_inbox_page_keeps_unreturned_bus_and_direct_rows_pending() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        crate::deliver_row(
            &receiver,
            &[
                "off=8",
                "from=s-a@n-a",
                "kind=ask",
                "trust=agent",
                "text=bus",
            ],
            &|| {},
        );
        as_node("a", || post(&receiver, "key=one kind=ask -- direct-one"));
        as_node("a", || post(&receiver, "key=two kind=ask -- direct-two"));
        let empty = crate::cmd_inbox(&receiver, "0");
        assert!(
            empty.starts_with("OK 0 ") && empty.contains("pending=3"),
            "{empty}"
        );
        let page = crate::cmd_inbox(&receiver, "1");
        assert!(
            page.starts_with("OK 1 ") && page.contains("pending=2"),
            "{page}"
        );
        assert!(
            page.contains("msg f2 ") && !page.contains("msg 1 "),
            "{page}"
        );
        assert!(!receiver.fabric.lock().rows[0].listed);
        assert!(crate::cmd_inbox(&receiver, "1").contains("msg f1 "));
        let bus = crate::cmd_inbox(&receiver, "1");
        assert!(
            bus.contains("msg 1 "),
            "retained listed direct rows cannot starve bus mail: {bus}"
        );
    }

    #[test]
    fn a_busy_durable_mailbox_waits_for_the_writer_without_losing_its_transaction() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        let dir = receiver.path.parent().unwrap();
        aterm_uds::private_file::create_private_dir(dir).unwrap();
        let held = aterm_uds::private_file::PrivateLock::take(
            dir,
            "inbox.toml.lock",
            std::time::Duration::ZERO,
            "busy",
        )
        .unwrap();
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
            let receiver = &receiver;
            scope.spawn(move || {
                let mut waiting_tx = Some(waiting_tx);
                let result = transaction_waiting(
                    receiver,
                    aterm_spec::wait::MUST_HAPPEN,
                    |boxed| {
                        boxed.next += 1;
                        Ok((boxed.next, true))
                    },
                    || {
                        if let Some(tx) = waiting_tx.take() {
                            tx.send(()).unwrap();
                        }
                    },
                );
                tx.send(result).unwrap()
            });
            waiting_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
            assert!(rx.try_recv().is_err(), "a held mailbox cannot be edited");
            assert_eq!(
                crate::cmd_await_inbox(receiver, &["since=0"], 0),
                "OK timeout\n"
            );
            assert_eq!(
                crate::cmd_await_inbox(receiver, &["since=f0"], 0),
                "OK timeout\n"
            );
            drop(held);
            assert_eq!(
                rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap()
                    .unwrap(),
                (1, true)
            );
        });
        assert_eq!(
            as_node("a", || post(&receiver, "key=k kind=ask -- question")),
            "OK f2\n"
        );
    }

    #[test]
    fn a_recipient_can_retire_read_mail_even_if_the_sender_never_returns() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        as_node("a", || post(&receiver, "key=k kind=note -- received"));
        assert!(forget(&receiver, "f1").starts_with("ERR "));
        inbox(&receiver, "--peek", false);
        assert!(forget(&receiver, "f1").starts_with("ERR "));
        inbox(&receiver, "", false);
        assert_eq!(forget(&receiver, "f1"), "OK forgot=f1\n");
        assert_eq!(
            as_node("a", || post(&receiver, "key=next kind=note -- next")),
            "OK f2\n"
        );
    }

    #[test]
    fn a_lost_inbox_response_can_be_recovered_from_the_retained_fleet_inbox() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        as_node("a", || post(&receiver, "key=k kind=ask -- retained"));
        assert!(crate::cmd_inbox(&receiver, "").contains("msg f1 "));
        assert!(crate::cmd_inbox_verb(&receiver, "fleet").contains("msg f1 "));
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK timeout\n"
        );
    }

    #[test]
    fn a_full_mailbox_reclaims_acknowledged_replies_without_a_manual_cleanup() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        for n in 0..PER_NODE {
            as_node("a", || {
                post(&receiver, &format!("key=k{n} kind=ask -- question"))
            });
        }
        post(&receiver, "re=f1 key=r kind=answer -- complete");
        as_node("a", || acknowledge(&receiver, "f1"));
        assert_eq!(
            as_node("a", || post(&receiver, "key=next kind=note -- next")),
            "OK f17\n"
        );
        let page = inbox(&receiver, "", false);
        assert!(!page.contains("msg f1 "), "{page}");
        assert!(
            page.contains("msg f2 ") && page.contains("msg f17 "),
            "{page}"
        );
    }

    fn transactions() -> u64 {
        TRANSACTIONS.with(std::cell::Cell::get)
    }

    fn timed() -> u64 {
        TIMED.with(std::cell::Cell::get)
    }

    #[test]
    fn the_tally_times_a_verbs_transaction_and_never_the_watchers_refresh() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let receiver = session(&path);
        let peer = session(&path);
        assert_eq!(
            as_node("a", || post(&peer, "key=one kind=ask -- first")),
            "OK f1\n"
        );
        let (all, verbs) = (transactions(), timed());
        assert!(restore(&receiver), "the watcher's refresh");
        assert_eq!(
            (transactions(), timed()),
            (all + 1, verbs),
            "a transaction no verb asked for"
        );
        assert_eq!(
            as_node("a", || post(&peer, "key=two kind=ask -- second")),
            "OK f2\n"
        );
        let (all, verbs) = (transactions(), timed());
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK inbox 0 fleet=f2\n"
        );
        assert!(crate::cmd_inbox(&receiver, "").contains("msg f2 "));
        assert_eq!(
            (transactions(), timed()),
            (all + 2, verbs + 2),
            "`await inbox` and `inbox` each asked for theirs"
        );
    }

    #[test]
    fn a_warm_cache_refuses_a_mailbox_that_loses_private_file_permissions() {
        check_warm_cache_access(false);
    }

    #[test]
    fn a_warm_cache_refuses_a_mailbox_directory_replaced_by_a_symlink() {
        check_warm_cache_access(true);
    }

    fn check_warm_cache_access(directory_link: bool) {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let receiver = session(&path);
        assert_eq!(
            as_node("a", || post(&receiver, "key=one kind=ask -- private")),
            "OK f1\n"
        );
        assert_eq!(pending(&receiver, 0, |_| true).unwrap(), Some(1));
        if directory_link {
            let moved = scratch.path().join("moved");
            std::fs::rename(path.parent().unwrap(), &moved).unwrap();
            symlink(moved, path.parent().unwrap()).unwrap();
        } else {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(
            pending(&receiver, 0, |_| true).is_err(),
            "a cached notification cannot bypass the private mailbox read"
        );
    }

    #[test]
    fn no_mailbox_in_a_directory_a_transaction_refuses_is_refused_not_read_as_empty() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let receiver = session(&path);
        let dir = path.parent().unwrap();
        let mode = |bits| std::fs::set_permissions(dir, std::fs::Permissions::from_mode(bits));
        std::fs::create_dir(dir).unwrap();
        mode(0o700).unwrap();
        // The control: a private directory with no mailbox in it is an empty mailbox.
        let page = crate::cmd_inbox(&receiver, "");
        assert!(
            page.starts_with("OK 0 ") && !page.contains("fleet_error"),
            "{page}"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=f0"], 0),
            "OK timeout\n"
        );
        // Open to others, the directory no transaction would use: still no file in it, and no
        // reader may call that an empty mailbox.
        mode(0o755).unwrap();
        assert!(
            crate::cmd_inbox_verb(&receiver, "fleet").starts_with("ERR fleet-mail: "),
            "the transaction refuses it"
        );
        let page = crate::cmd_inbox(&receiver, "");
        assert!(page.contains(" fleet_error=unavailable"), "{page}");
        let waited = crate::cmd_await_inbox(&receiver, &["since=f0"], 0);
        assert!(waited.starts_with("ERR fleet-mail: "), "{waited}");
        assert!(pending(&receiver, 0, |_| true).is_err());
        assert!(
            !dir.join("inbox.toml.lock").exists(),
            "refused before the lock"
        );
    }

    #[test]
    fn a_session_with_no_mailbox_lists_and_waits_without_making_its_directory_or_lock() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let receiver = session(&path);
        let before = transactions();
        for _ in 0..8 {
            let page = crate::cmd_inbox(&receiver, "");
            assert!(
                page.starts_with("OK 0 ") && !page.contains("fleet_error"),
                "{page}"
            );
            for args in [
                &["since=0"][..],
                &["since=f0"],
                &["since=0", "fleet_since=f0"],
            ] {
                assert_eq!(crate::cmd_await_inbox(&receiver, args, 0), "OK timeout\n");
            }
            assert_eq!(
                crate::cmd_await_inbox(&receiver, &["since=0"], 1),
                "OK timeout\n",
                "a wait that parks reads no mailbox either"
            );
        }
        assert_eq!(transactions(), before);
        let dir = path.parent().unwrap();
        assert!(!dir.exists(), "no directory, so no inbox.toml.lock");
        // The negative control: the transaction those readers no longer take makes both.
        assert!(crate::cmd_inbox_verb(&receiver, "fleet").starts_with("OK 0 fleet=1 "));
        assert_eq!(transactions(), before + 1);
        assert!(dir.join("inbox.toml.lock").exists());
        assert!(
            !dir.join(KEY).exists(),
            "a transaction that writes nothing mints no key"
        );
    }

    /// The mailbox at `path` as the writer before sealing left it: its TOML, in plaintext.
    fn plaintext_writer(path: &std::path::Path, boxed: &Mailbox) {
        aterm_uds::private_file::write_private(
            path.parent().unwrap(),
            path.file_name().unwrap().to_str().unwrap(),
            aterm_toml::to_string(boxed).unwrap().as_bytes(),
        )
        .unwrap();
    }

    /// The mailbox at `path`, opened as a transaction opens it.
    fn opened(path: &std::path::Path) -> Result<Mailbox, String> {
        let dir = path.parent().unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        let bytes = aterm_uds::private_file::read_private(dir, name, SEALED_BYTES)
            .map_err(|e| e.to_string())?;
        open_mailbox(dir, name, bytes, &mut None).map(|(boxed, _)| boxed)
    }

    /// Whether the file at `path` holds `text`'s bytes anywhere.
    fn holds(path: &std::path::Path, text: &str) -> bool {
        std::fs::read(path)
            .unwrap()
            .windows(text.len())
            .any(|window| window == text.as_bytes())
    }

    fn legacy_mailbox(path: &std::path::Path) -> Vec<u8> {
        aterm_uds::private_file::create_private_dir(path.parent().unwrap()).unwrap();
        plaintext_writer(
            path,
            &Mailbox {
                next: 1,
                rows: vec![Row {
                    id: 1,
                    node: "a".repeat(32),
                    key: "earlier-post".into(),
                    kind: "ask".into(),
                    text: "earlier question".into(),
                    listed: false,
                    reply: Some(Reply {
                        key: "earlier-reply".into(),
                        kind: "answer".into(),
                        text: "earlier answer".into(),
                    }),
                    reply_seen: false,
                }],
            },
        );
        std::fs::read(path).unwrap()
    }

    /// The pre-sealing reader opens the file anew for every transaction, then requires UTF-8
    /// and TOML. Keeping its session idle holds no transaction lock.
    fn earlier_reader(path: &std::path::Path) -> Result<String, String> {
        let bytes = aterm_uds::private_file::read_private(
            path.parent().unwrap(),
            path.file_name().unwrap().to_str().unwrap(),
            FILE_BYTES,
        )
        .map_err(|e| e.to_string())?;
        let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        let mailbox: Mailbox = aterm_toml::from_str(text).map_err(|e| e.to_string())?;
        aterm_toml::to_string(&mailbox).map_err(|e| e.to_string())
    }

    #[test]
    fn migration_keeps_an_idle_earlier_readers_mailbox_unchanged() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/s-aaaaaaaa.toml");
        let before = legacy_mailbox(&path);
        let expected = earlier_reader(&path).unwrap();
        let dir = path.parent().unwrap();
        // A new owner refreshes its own mailbox, without adopting the older owner's file.
        let current = session(&dir.join("s-bbbbbbbb.toml"));
        let first = restore(&current);
        let first_read = earlier_reader(&path);
        let second = restore(&current);
        let second_read = earlier_reader(&path);
        assert_eq!(first_read, Ok(expected.clone()), "startup {first:?}");
        assert_eq!(second_read, Ok(expected), "startup {second:?}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!dir.join(KEY).exists(), "a foreign mailbox mints no key");
    }

    #[test]
    fn migration_waits_for_committed_ownership_before_reading_or_writing() {
        use std::sync::atomic::Ordering;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let missing = scratch.path().join("not-created/inbox.toml");
        let pending = session(&missing);
        pending.ready.store(false, Ordering::SeqCst);
        assert!(!restore(&pending));
        assert!(as_node("b", || post(&pending, "key=new kind=ask -- new")).starts_with("ERR "));
        assert!(
            !missing.parent().unwrap().exists(),
            "no directory before Commit"
        );
        let path = scratch.path().join("private/s-aaaaaaaa.toml");
        let before = legacy_mailbox(&path);
        let owner = std::sync::Arc::new(session(&path));
        owner.ready.store(false, Ordering::SeqCst);
        // Use the Arc forwarding implementation too: a wrapper must not grant readiness.
        let refreshed = restore(&owner);
        let posted = as_node("b", || post(&owner, "key=pending kind=ask -- pending"));
        assert!(!refreshed, "an uncommitted candidate restored mail");
        assert!(posted.starts_with("ERR "), "{posted}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!path.parent().unwrap().join(KEY).exists());
        assert!(owner.fabric.lock().direct_mail.rows.is_empty());
        assert!(owner.facts.lock().unwrap().is_empty());
        assert_eq!(owner.wakes.load(Ordering::SeqCst), 0);

        owner.ready.store(true, Ordering::SeqCst);
        assert!(restore(&owner), "Commit activates the same owner");
        assert!(std::fs::read(&path).unwrap().starts_with(SEALED));
        let page = inbox(&owner, "", false);
        assert!(
            page.contains("text=earlier%20question")
                && page.contains("reply_text=earlier%20answer"),
            "{page}"
        );
        assert_eq!(
            as_node("b", || post(&owner, "key=committed kind=ask -- later")),
            "OK f2\n"
        );
        owner.ready.store(false, Ordering::SeqCst);
        let before = std::fs::read(&path).unwrap();
        let page = inbox(&owner, "", true);
        assert!(
            page.starts_with("ERR "),
            "a warm cache bypassed ownership: {page}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn no_row_text_reaches_the_disk_and_an_earlier_builds_plaintext_is_sealed_by_its_next_transaction()
     {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/s-12345678.toml");
        let receiver = session(&path);
        assert_eq!(
            as_node("a", || post(
                &receiver,
                "key=one kind=ask -- PRIVATE_ROW_TEXT"
            )),
            "OK f1\n"
        );
        assert_eq!(
            post(&receiver, "re=f1 key=r kind=answer -- PRIVATE_REPLY_TEXT"),
            "OK f1 reply=1\n"
        );
        let texts = ["PRIVATE_ROW_TEXT", "PRIVATE_REPLY_TEXT", "[[rows]]"];
        let sealed = |why: &str| {
            assert!(std::fs::read(&path).unwrap().starts_with(SEALED), "{why}");
            for text in texts {
                assert!(!holds(&path, text), "{why}: {text} on disk");
            }
        };
        sealed("after a post and a reply");
        // The negative control: the writer before sealing leaves every row's text readable.
        let boxed = opened(&path).unwrap();
        plaintext_writer(&path, &boxed);
        for text in texts {
            assert!(holds(&path, text), "the old writer's file holds {text}");
        }
        // Its next transaction, a remote read that edits nothing, seals it and loses no row.
        let successor = session(&path);
        let page = as_node("a", || inbox(&successor, "", false));
        assert!(
            page.contains("text=PRIVATE_ROW_TEXT")
                && page.contains("reply_text=PRIVATE_REPLY_TEXT"),
            "{page}"
        );
        sealed("after a read");
        // As does one whose edit refuses: the seal comes first.
        plaintext_writer(&path, &boxed);
        assert!(forget(&successor, "f9").starts_with("ERR "));
        sealed("after a refused verb");
        assert_eq!(opened(&path).unwrap().rows.len(), 1);
    }

    #[test]
    fn a_mailbox_moved_to_another_sessions_file_does_not_open_there_and_is_not_replaced() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let mine = scratch.path().join("private/s-11111111.toml");
        let theirs = mine.with_file_name("s-22222222.toml");
        assert_eq!(
            as_node("a", || post(
                &session(&mine),
                "key=k kind=ask -- MOVED_TEXT"
            )),
            "OK f1\n"
        );
        std::fs::rename(&mine, &theirs).unwrap();
        let moved = std::fs::read(&theirs).unwrap();
        let other = session(&theirs);
        let refused = crate::cmd_inbox_verb(&other, "fleet");
        assert!(
            refused.starts_with("ERR fleet-mail: ") && refused.contains("does not open"),
            "{refused}"
        );
        assert!(as_node("a", || post(&other, "key=k2 kind=note -- next")).starts_with("ERR "));
        assert_eq!(std::fs::read(&theirs).unwrap(), moved, "left as it is");
        // The control: under its own name, the same bytes open.
        std::fs::rename(&theirs, &mine).unwrap();
        let page = crate::cmd_inbox_verb(&session(&mine), "fleet");
        assert!(page.contains("text=MOVED_TEXT"), "{page}");
    }

    #[test]
    fn a_missing_key_refuses_direct_mail_mints_none_and_leaves_bus_mail_listed() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let dir = path.parent().unwrap();
        let key = dir.join(KEY);
        let receiver = session(&path);
        assert_eq!(
            as_node("a", || post(&receiver, "key=k kind=ask -- direct")),
            "OK f1\n"
        );
        assert_eq!(
            crate::deliver_row(
                &receiver,
                &[
                    "off=8",
                    "from=s-a@n-a",
                    "kind=ask",
                    "trust=agent",
                    "text=bus"
                ],
                &|| {}
            ),
            "OK 1\n"
        );
        let held = std::fs::read(&key).unwrap();
        let sealed = std::fs::read(&path).unwrap();
        std::fs::remove_file(&key).unwrap();
        let refused = crate::cmd_inbox_verb(&receiver, "fleet");
        assert!(
            refused.starts_with("ERR fleet-mail: ") && refused.contains("key beside it is missing"),
            "{refused}"
        );
        let page = crate::cmd_inbox(&receiver, "");
        assert!(
            page.starts_with("OK 1 ")
                && page.contains("fleet_error=unavailable")
                && page.contains("text=bus"),
            "{page}"
        );
        assert!(
            crate::cmd_await_inbox(&session(&path), &["since=f0"], 0)
                .starts_with("ERR fleet-mail: "),
            "a wait with no cache reads the mailbox"
        );
        assert!(as_node("a", || post(&receiver, "key=k2 kind=ask -- next")).starts_with("ERR "));
        assert!(!key.exists(), "no key is minted beside a sealed mailbox");
        assert_eq!(std::fs::read(&path).unwrap(), sealed, "left as it is");
        // The control: the key put back opens it, and is kept out of backups again.
        aterm_uds::private_file::write_private(dir, KEY, &held).unwrap();
        let page = crate::cmd_inbox_verb(&receiver, "fleet");
        assert!(
            page.contains("msg f1 ") && page.contains("text=direct"),
            "{page}"
        );
        #[cfg(target_os = "macos")]
        assert!(BACKUPS.excluded(&std::fs::File::open(&key).unwrap()));
    }

    #[test]
    fn the_key_is_owner_only_and_left_out_of_backups() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let key = path.with_file_name(KEY);
        let receiver = session(&path);
        assert_eq!(
            as_node("a", || post(&receiver, "key=k kind=ask -- question")),
            "OK f1\n"
        );
        let meta = std::fs::metadata(&key).unwrap();
        assert_eq!((meta.permissions().mode() & 0o777, meta.len()), (0o600, 32));
        // Only macOS has a mark; elsewhere nothing is left out of any backup, and the mint says so.
        #[cfg(target_os = "macos")]
        {
            let marked =
                |path: &std::path::Path| BACKUPS.excluded(&std::fs::File::open(path).unwrap());
            assert!(marked(&key), "the key is left out of backups");
            assert!(
                !marked(&path),
                "the control: the sealed mailbox beside it is not"
            );
        }
        let mode = |bits| std::fs::set_permissions(&key, std::fs::Permissions::from_mode(bits));
        mode(0o644).unwrap();
        let refused = crate::cmd_inbox_verb(&session(&path), "fleet");
        assert!(
            refused.starts_with("ERR fleet-mail: ") && refused.contains("key does not read"),
            "{refused}"
        );
        assert!(as_node("b", || post(&receiver, "key=k2 kind=note -- next")).starts_with("ERR "));
        // The control: back at 0600, it reads.
        mode(0o600).unwrap();
        assert!(crate::cmd_inbox_verb(&session(&path), "fleet").contains("msg f1 "));
    }

    #[test]
    fn a_key_of_another_length_is_refused_by_the_length_it_has() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let dir = path.parent().unwrap();
        let receiver = session(&path);
        assert_eq!(
            as_node("a", || post(&receiver, "key=k kind=ask -- question")),
            "OK f1\n"
        );
        let held = std::fs::read(dir.join(KEY)).unwrap();
        for len in [64, 31] {
            aterm_uds::private_file::write_private(dir, KEY, &vec![7; len]).unwrap();
            let refused = crate::cmd_inbox_verb(&session(&path), "fleet");
            assert!(
                refused.contains(&format!("it holds {len} bytes, not 32")),
                "{refused}"
            );
        }
        // The control: the key it was sealed under reads.
        aterm_uds::private_file::write_private(dir, KEY, &held).unwrap();
        assert!(crate::cmd_inbox_verb(&session(&path), "fleet").contains("msg f1 "));
    }

    #[test]
    fn no_key_is_minted_while_another_mailbox_beside_it_is_sealed_so_the_key_put_back_opens_all() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let first = scratch.path().join("private/s-aaaaaaaa.toml");
        let second = first.with_file_name("s-bbbbbbbb.toml");
        let dir = first.parent().unwrap();
        let key = dir.join(KEY);
        assert_eq!(
            as_node("a", || post(&session(&first), "key=k kind=ask -- FIRST")),
            "OK f1\n"
        );
        let held = std::fs::read(&key).unwrap();
        std::fs::remove_file(&key).unwrap();
        // The second session has no mailbox: a key its first write minted would leave the first
        // session's mail unopened under any key file.
        let refused = as_node("a", || post(&session(&second), "key=k kind=ask -- SECOND"));
        assert!(
            refused.starts_with("ERR fleet-mail: ")
                && refused.contains("sealed")
                && refused.contains("put the key back"),
            "{refused}"
        );
        assert!(!key.exists(), "no key is minted beside a sealed mailbox");
        assert!(!second.exists());
        // Put back, the one key opens both: the first session's mail, and the second's after it.
        aterm_uds::private_file::write_private(dir, KEY, &held).unwrap();
        assert_eq!(
            as_node("a", || post(&session(&second), "key=k kind=ask -- SECOND")),
            "OK f1\n"
        );
        let page = |path: &std::path::Path| crate::cmd_inbox_verb(&session(path), "fleet");
        assert!(page(&first).contains("text=FIRST"), "{}", page(&first));
        assert!(page(&second).contains("text=SECOND"), "{}", page(&second));
        // The control: with every sealed mailbox moved out of the folder, a lost key is replaced.
        std::fs::remove_file(&key).unwrap();
        for path in [&first, &second] {
            std::fs::rename(path, scratch.path().join(path.file_name().unwrap())).unwrap();
        }
        let third = first.with_file_name("s-cccccccc.toml");
        assert_eq!(
            as_node("a", || post(&session(&third), "key=k kind=ask -- THIRD")),
            "OK f1\n"
        );
        assert!(key.exists());
    }

    #[test]
    fn a_changed_stamp_takes_the_transaction_and_an_unchanged_one_reads_the_cache() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let receiver = session(&path);
        let peer = session(&path);
        assert_eq!(
            as_node("a", || post(&receiver, "key=one kind=ask -- question")),
            "OK f1\n"
        );
        let before = transactions();
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK inbox 0 fleet=f1\n"
        );
        assert_eq!(transactions(), before, "the post's own write is cached");

        assert_eq!(
            as_node("b", || post(&peer, "key=two kind=ask -- from the peer")),
            "OK f2\n"
        );
        let before = transactions();
        let after_f1 = ["since=0", "fleet_since=f1"];
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &after_f1, 0),
            "OK inbox 0 fleet=f2\n"
        );
        assert_eq!(
            transactions(),
            before + 1,
            "the peer's write moved the stamp"
        );
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &after_f1, 0),
            "OK inbox 0 fleet=f2\n"
        );
        assert_eq!(
            transactions(),
            before + 1,
            "and that read cached the new one"
        );

        let page = crate::cmd_inbox(&receiver, "");
        assert!(
            page.contains("msg f1 ") && page.contains("msg f2 "),
            "{page}"
        );
        assert_eq!(
            transactions(),
            before + 2,
            "rows to mark take the transaction"
        );
        let page = crate::cmd_inbox(&receiver, "");
        assert!(page.starts_with("OK 0 "), "{page}");
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK timeout\n"
        );
        assert_eq!(transactions(), before + 2, "a page with nothing to list");

        assert_eq!(
            as_node("b", || post(&peer, "key=three kind=ask -- unread")),
            "OK f3\n"
        );
        crate::deliver_row(
            &receiver,
            &[
                "off=8",
                "from=s-a@n-a",
                "kind=ask",
                "trust=agent",
                "text=bus",
            ],
            &|| {},
        );
        let before = transactions();
        assert_eq!(
            crate::cmd_await_inbox(&receiver, &["since=0"], 0),
            "OK inbox 1\n"
        );
        assert_eq!(
            transactions(),
            before,
            "a latching bus row reads no mailbox"
        );
    }

    #[test]
    fn a_handoff_peers_write_is_listed_by_the_next_inbox_and_its_refresh_wakes_a_parked_wait() {
        use std::sync::atomic::Ordering;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("private/inbox.toml");
        let receiver = session(&path);
        // Another process's handle on the same mailbox: its writes ring only its own waiters.
        let peer = session(&path);
        assert_eq!(
            as_node("a", || post(&receiver, "key=one kind=ask -- first")),
            "OK f1\n"
        );
        assert!(crate::cmd_inbox(&receiver, "").contains("msg f1 "));
        assert_eq!(
            as_node("b", || post(&peer, "key=two kind=ask -- second")),
            "OK f2\n"
        );
        assert_eq!(
            receiver.fabric.last_direct_mail_id(),
            1,
            "nothing refreshed"
        );
        let page = crate::cmd_inbox(&receiver, "");
        assert!(
            page.contains("msg f2 ") && !page.contains("msg f1 "),
            "{page}"
        );

        let parks = || receiver.fabric.inbox_parks.load(Ordering::SeqCst);
        // What rings the receiver's parked waits: the epoch and the session's watchers move
        // together, once a ring.
        let rings = || {
            (
                receiver.fabric.direct_epoch.load(Ordering::SeqCst),
                receiver.wakes.load(Ordering::SeqCst),
            )
        };
        // Past the hang detector below, so an answer that comes before it is a ring's and never
        // the wait's deadline: a wait that times out reads the mailbox as it answers.
        let timeout = 2 * aterm_spec::wait::MUST_HAPPEN;
        // A wait parked on the receiver while the peer writes `key`, and then the watcher's
        // refresh, unrung when `unrung`; an unrung refresh is followed by a bus row's ring.
        // Its answer, how often it parked, and how far the refresh moved the rings.
        let parked_through = |key: &str, unrung: bool| {
            /// A failed assertion ends the wait at once, not at its deadline.
            struct Release<'a>(&'a SessionFabric);
            impl Drop for Release<'_> {
                fn drop(&mut self) {
                    if std::thread::panicking() {
                        self.0.close();
                    }
                }
            }
            std::thread::scope(|scope| {
                let (tx, rx) = std::sync::mpsc::channel();
                let before = parks();
                let receiver = &receiver;
                let armed = std::time::Instant::now();
                scope.spawn(move || {
                    let timeout = u64::try_from(timeout.as_millis()).unwrap();
                    // Unheard once a failed assertion has gone.
                    let _ = tx.send(crate::cmd_await_inbox(receiver, &["since=0"], timeout));
                });
                let _release = Release(&receiver.fabric);
                aterm_spec::wait::within(
                    aterm_spec::wait::MUST_HAPPEN,
                    std::time::Duration::from_millis(1),
                    || (parks() > before).then_some(()),
                )
                .expect("the wait parks");
                let bound = receiver
                    .fabric
                    .inbox_park_until
                    .lock()
                    .unwrap()
                    .expect("a park's bound");
                assert!(
                    bound >= armed + timeout,
                    "no recheck ends the park before the wait's own deadline"
                );
                assert!(
                    as_node("b", || post(&peer, &format!("key={key} kind=ask -- later")))
                        .starts_with("OK f")
                );
                let (epoch, wakes) = rings();
                UNRUNG.with(|u| u.set(unrung));
                let refreshed = restore(receiver);
                UNRUNG.with(|u| u.set(false));
                assert!(refreshed, "the watcher's refresh");
                let (rung_epoch, rung_wakes) = rings();
                if unrung {
                    crate::deliver_row(
                        receiver,
                        &[
                            "off=8",
                            "from=s-a@n-a",
                            "kind=ask",
                            "trust=agent",
                            "text=bus",
                        ],
                        &|| {},
                    );
                }
                let answer = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
                if answer.is_err() {
                    receiver.fabric.wake_waiters_for_test();
                }
                (
                    answer.expect("the parked wait answers"),
                    parks() - before,
                    (rung_epoch - epoch, rung_wakes - wakes),
                )
            })
        };
        assert_eq!(
            parked_through("three", false),
            ("OK inbox 0 fleet=f3\n".to_owned(), 1, (1, 1)),
            "the refresh rang the one park awake"
        );
        assert!(crate::cmd_inbox(&receiver, "").contains("msg f3 "));
        // The negative control. Unrung, the refresh moves no ring, and the park's only other
        // bound is the wait's own deadline (both asserted above, neither by a race), so the
        // wait sleeps through f4 until the bus row's ring, which latches before the mailbox is
        // read.
        assert_eq!(
            parked_through("four", true),
            ("OK inbox 1\n".to_owned(), 1, (0, 0))
        );
        assert_eq!(
            receiver.fabric.last_direct_mail_id(),
            4,
            "refreshed, unrung"
        );
    }

    #[test]
    fn inbox_verb_and_direct_mail_parsers_reject_signed_numbers_and_stuck_fleet_prefixes() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let receiver = session(&scratch.path().join("private/inbox.toml"));
        assert_eq!(
            as_node("a", || post(&receiver, "key=k kind=ask -- question")),
            "OK f1\n"
        );
        assert_eq!(
            as_node("a", || crate::cmd_inbox_verb(&receiver, "seen")),
            "ERR usage: inbox seen f<id>\n"
        );
        assert!(
            as_node("a", || crate::cmd_inbox_verb(&receiver, "fleet--peek")).starts_with("ERR ")
        );
        assert!(as_node("a", || crate::cmd_inbox_verb(&receiver, "fleet1")).starts_with("ERR "));
        assert!(as_node("a", || inbox(&receiver, "since=f+1", false)).starts_with("ERR "));
        assert!(as_node("a", || inbox(&receiver, "+1", false)).starts_with("ERR "));
        assert_eq!(
            post(&receiver, "re=f+1 key=r kind=answer -- answer"),
            "ERR fleet-mail: invalid direct-mail option\n"
        );
        let _ = crate::cmd_inbox(&receiver, "");
        assert_eq!(
            crate::cmd_inbox_verb(&receiver, "seen   f+1 forget"),
            "ERR usage: inbox seen f<id> forget\n"
        );
        assert_eq!(
            crate::cmd_inbox_verb(&receiver, "seen   f1 forget"),
            "OK forgot=f1\n"
        );
    }
}
