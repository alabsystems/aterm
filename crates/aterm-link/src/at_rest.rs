// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUS LOG AT REST: `bus.log` and its `.state` and `.held` sealed under `rest.key`, astream's
//! at-rest (claim `broker.guarded-at-rest`), so a backup that leaves the key out holds no mail it
//! can read.
//!
//! The key is a file beside the log, 0600 in a 0700 folder. Not the Keychain: an ad hoc dev build
//! or a lab instance asking for a Keychain item puts a dialog on the owner's screen. On macOS it
//! carries the mark Time Machine honours ([`aterm_uds::backup`]). No other system has a mark, so a
//! backup tool there copies the key beside the log unless it is told to skip it, and a start says
//! so.
//!
//! WHAT IT PROTECTS: `bus.log` in a backup that leaves the key out, and in any copy made without
//! it. WHAT IT DOES NOT: a process running as this user, or a copy of the whole root, key
//! included. Crash-restore state stays 0600 plaintext. Anti-rollback stays off: astream keeps it
//! for Strict, and aterm's broker defaults to Ordered.
//!
//! THE RULES a start follows ([`prepare`]):
//!
//! - A sealed log (a `<log>.seal` beside it) opens under its key, or not at all: a missing or
//!   damaged key refuses, naming the key file and the way out ([`refusal`]), and nothing in the
//!   log changes. It is never opened keyless and never replaced: giving it up is a person's
//!   [`retire`], which moves it aside and starts a new log with a new identity.
//! - A log a [`retire`] stopped part way does not open at all, and no key is minted beside it,
//!   until the retire is run again ([`unfinished_retire`]): by then `.seal` may have gone, and
//!   nothing else a start reads would say the records beside it are sealed.
//! - A key is minted only where no log is known to be sealed ([`mint`]), and never over a key: a
//!   new key makes a sealed log unreadable.
//! - An existing plaintext log beside a key is sealed once, offline, under its lock
//!   ([`astream_broker::BrokerLog::seal_plaintext`]): the plaintext log stands until the sealed
//!   copy has read back record for record. A seal that cannot run leaves its records as they are:
//!   a log that reads as plaintext serves plaintext and the next start tries again, and one that
//!   does not (a damaged record, or a sealed log whose `.seal` is gone) is refused by the open
//!   that follows. The log is never moved aside and no mail is dropped.

use std::io;
use std::path::{Path, PathBuf};

use aterm_uds::backup::Backups;

/// The key's file name, beside the log it seals.
pub const REST_KEY: &str = "rest.key";

/// The verb that gives a log up.
pub const RETIRE_VERB: &str = "aterm link broker --retire-log";

/// The words every remedy for a sealed log's refusal opens with. It is the last line of
/// `broker.err`, whose quote cuts at 256 characters, and `aterm fabric doctor` reads the line by
/// these words, so they stay ahead of every path the line names.
pub const SEALED_LOG_REMEDY: &str = "the log is sealed at rest and is not opened";

/// The words the remedy for a [`retire`] that stopped part way opens with, for the same reader.
pub const UNFINISHED_RETIRE_REMEDY: &str = "the log's retire did not finish and it is not opened";

/// What marks a [`retire`] under way: `<log>.retiring`, naming the folder it moves to.
const RETIRING: &str = ".retiring";

const KEY_LEN: usize = 32;

/// A log's key. Its bytes are cleared when it drops, and its `Debug` shows none of them.
pub struct Key([u8; KEY_LEN]);

impl Key {
    /// The bytes, for astream's open.
    #[must_use]
    pub const fn bytes(&self) -> [u8; KEY_LEN] {
        self.0
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        self.0.fill(0);
        std::hint::black_box(&mut self.0);
    }
}

/// What a log's key file holds.
#[derive(Debug)]
pub enum KeyFile {
    /// No file.
    Absent,
    /// A key.
    Held(Key),
    /// A file that is no usable key (the reader's refusal, or the wrong length).
    Damaged(String),
}

/// The key file beside `log`.
#[must_use]
pub fn key_path(log: &Path) -> PathBuf {
    log.with_file_name(REST_KEY)
}

fn beside(log: &Path, suffix: &str) -> PathBuf {
    let mut name = log.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn seal_path(log: &Path) -> PathBuf {
    beside(log, ".seal")
}

/// The folder `log` sits in: `.` for a bare relative name, whose parent is the empty path.
fn folder_of(log: &Path) -> &Path {
    match log.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// Whether anything is at `path`. A name it cannot tell about counts as there: the safe direction
/// for every question this module asks.
fn present(path: &Path) -> bool {
    !matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == io::ErrorKind::NotFound)
}

/// Whether `log` is sealed: the log, with astream's binding sidecar beside it. A `.seal` beside
/// no log is what a deleted log left, and binds nothing.
#[must_use]
pub fn is_sealed(log: &Path) -> bool {
    present(log) && present(&seal_path(log))
}

/// `sealed` or `plaintext`, what a status line says of `log`.
#[must_use]
pub fn word(log: &Path) -> &'static str {
    if is_sealed(log) {
        "sealed"
    } else {
        "plaintext"
    }
}

/// `log`'s key file, read under astream's owner-only secret rule by `reader`.
fn read_with(log: &Path, reader: fn(&str) -> io::Result<Vec<u8>>) -> KeyFile {
    let path = key_path(log);
    match reader(&path.to_string_lossy()) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => KeyFile::Absent,
        Err(e) => KeyFile::Damaged(e.to_string()),
        Ok(mut bytes) => {
            let held = <[u8; KEY_LEN]>::try_from(bytes.as_slice());
            let len = bytes.len();
            bytes.fill(0);
            std::hint::black_box(&mut bytes);
            held.map_or_else(
                |_| {
                    KeyFile::Damaged(format!(
                        "{}: holds {len} bytes, not {KEY_LEN}",
                        path.display()
                    ))
                },
                |key| KeyFile::Held(Key(key)),
            )
        }
    }
}

/// `log`'s key file, past what a crash inside [`mint`] left: the temporary it linked the key from
/// is the key's second link, which the reader refuses and no writer will remove. A live minter's
/// temporary stays, and this read refuses for its moment.
#[must_use]
pub fn read_key(log: &Path) -> KeyFile {
    sweep_key_temporaries(log);
    read_with(log, crate::transport::read_mint_secret)
}

fn sweep_key_temporaries(log: &Path) {
    let _ = aterm_uds::atomic_file::sweep_temps_beside(&key_path(log), std::time::Duration::ZERO);
}

/// Mint a key beside `log`, marked to stay out of backups from its first moment: `Ok(None)` when
/// a key is there already, which is left as it is, and otherwise what the mark said (it never
/// stops a mint: a sealed log with its key in a backup still protects every other copy, and the
/// caller says so).
///
/// # Errors
/// `log` is sealed (a new key would make it unreadable), or no entropy, a folder that is not
/// private, or a write's own error.
pub fn mint(log: &Path, backups: Backups) -> io::Result<Option<io::Result<()>>> {
    if is_sealed(log) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is sealed at rest: a new key would make it unreadable",
                log.display()
            ),
        ));
    }
    let mut bytes = [0u8; KEY_LEN];
    aterm_uds::rand::fill(&mut bytes)?;
    let mut marked = Ok(());
    let wrote =
        aterm_uds::private_file::write_private_new(folder_of(log), REST_KEY, &bytes, |file| {
            marked = backups.exclude(file);
            Ok(())
        });
    bytes.fill(0);
    std::hint::black_box(&mut bytes);
    Ok(wrote?.then_some(marked))
}

/// The line a start logs for a key the backup mark would not take.
fn unmarked_note(log: &Path, why: &io::Error) -> String {
    format!(
        "{}: could not be left out of backups ({why}); a backup of this folder holds the key \
         beside the log it seals",
        key_path(log).display()
    )
}

/// Mark `log`'s existing key to stay out of backups, what a crash or a restored copy may have
/// lost.
fn mark_existing(log: &Path, backups: Backups) -> io::Result<()> {
    backups.exclude_path(&key_path(log))
}

/// Why a log's start stops, and the way out.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused {
    /// What is wrong.
    pub why: String,
    /// What to do about it.
    pub remedy: String,
}

fn shown(path: &Path) -> String {
    path.display().to_string()
}

fn quoted(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

/// The refusal of a log a [`retire`] stopped part way, when one did. Its `.seal` may have gone by
/// then, so nothing else says the records beside it are sealed: nothing opens it and no key is
/// minted beside it until the retire is run again.
#[must_use]
pub fn unfinished_retire(log: &Path) -> Option<Refused> {
    present(&beside(log, RETIRING)).then(|| Refused {
        why: format!(
            "{}: a `--retire-log` of this log stopped part way ({} is there): it is not opened, \
             and no key is minted beside it",
            log.display(),
            shown(&beside(log, RETIRING))
        ),
        remedy: format!(
            "{UNFINISHED_RETIRE_REMEDY}: run `{RETIRE_VERB} '{}'` again, which moves the rest to \
             the same folder, and the next start begins a new log with a new identity",
            quoted(log)
        ),
    })
}

/// The refusal of a log whose key `problem` says what is wrong with.
#[must_use]
pub fn refusal(log: &Path, problem: &str) -> Refused {
    let key = shown(&key_path(log));
    if is_sealed(log) {
        Refused {
            why: format!(
                "{} is sealed at rest and its key does not read ({problem}): it is not opened, \
                 and nothing in it changes",
                log.display()
            ),
            remedy: format!(
                "{SEALED_LOG_REMEDY}: put the key it was sealed under back at {key}, or give the \
                 log up: `{RETIRE_VERB} '{}'` moves it aside, its sealed records kept, and the \
                 next start begins a new log with a new identity",
                quoted(log)
            ),
        }
    } else {
        Refused {
            why: format!(
                "the at-rest key beside {} does not read ({problem})",
                log.display()
            ),
            remedy: format!(
                "move {key} aside and start again: no log is sealed under it, and `aterm fabric \
                 on` mints another"
            ),
        }
    }
}

/// The remedy for an open that refused a sealed `log` as `kind`: the plain remedy for a corrupt
/// record would send the person to a repair that cannot run keyless. A plaintext log has that plain
/// remedy, a key beside it or not: `--repair` goes by the seal.
#[must_use]
pub fn open_remedy(log: &Path, kind: io::ErrorKind) -> Option<String> {
    (kind == io::ErrorKind::InvalidData && is_sealed(log)).then(|| {
        let at = quoted(log);
        format!(
            "{SEALED_LOG_REMEDY}. A damaged record: `aterm link broker --repair '{at}'` cuts it \
             there, under its key, and the records from it on are lost; it refuses, cutting \
             nothing, when the key is not the one that sealed it. A key that is not the log's: \
             put the log's back at {}. A key that is gone: `{RETIRE_VERB} '{at}'` moves the log \
             aside, its sealed records kept, and the next start begins a new log with a new \
             identity",
            shown(&key_path(log))
        )
    })
}

/// The remedy for a `--repair` that refused `log` as `kind`. Beside a key, a keyless repair that
/// refuses is most likely a sealed log whose `.seal` is gone, which no key opens either.
#[must_use]
pub fn repair_remedy(log: &Path, kind: io::ErrorKind) -> Option<String> {
    open_remedy(log, kind).or_else(|| {
        (kind == io::ErrorKind::InvalidData && present(&key_path(log))).then(|| {
            format!(
                "if this log was sealed at rest, its binding {} is gone and no key opens it \
                 without that: put the file back, or give the log up: `{RETIRE_VERB} '{}'` moves \
                 it aside, its records kept, and the next start begins a new log with a new \
                 identity",
                shown(&seal_path(log)),
                quoted(log)
            )
        })
    })
}

/// What a broker's start does at rest.
#[derive(Debug)]
pub struct Prepared {
    /// The key to open the log under, keyed and bound; `None` serves it plaintext.
    pub key: Option<Key>,
    /// What to tell the log file: a key minted, a log sealed, a seal that could not run.
    pub notes: Vec<String>,
}

/// Decide how a broker opens `log` ([module rules](self)), minting a key where this is a fabric
/// root (a mint secret beside the log) and sealing an existing plaintext log once.
///
/// # Errors
/// [`Refused`], when the log's key is missing or damaged, or a [`retire`] of it stopped part way.
pub fn prepare(log: &Path, backups: Backups) -> Result<Prepared, Refused> {
    if let Some(refused) = unfinished_retire(log) {
        return Err(refused);
    }
    let mut notes = Vec::new();
    let sealed = is_sealed(log);
    let key = match read_key(log) {
        KeyFile::Held(key) => {
            if let Err(e) = mark_existing(log, backups) {
                notes.push(unmarked_note(log, &e));
            }
            key
        }
        KeyFile::Damaged(problem) => return Err(refusal(log, &problem)),
        KeyFile::Absent if sealed => {
            return Err(refusal(
                log,
                &format!("{}: no such file", shown(&key_path(log))),
            ));
        }
        KeyFile::Absent if !present(&log.with_file_name(crate::enable::MINT_SECRET)) => {
            return Ok(Prepared { key: None, notes });
        }
        KeyFile::Absent => {
            match mint(log, backups) {
                Ok(Some(marked)) => {
                    notes.push(format!("{}: minted a new key", shown(&key_path(log))));
                    if let Err(e) = marked {
                        notes.push(unmarked_note(log, &e));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    notes.push(format!(
                        "{}: no key could be minted ({e}); it serves plaintext",
                        shown(&key_path(log))
                    ));
                    return Ok(Prepared { key: None, notes });
                }
            }
            match read_key(log) {
                KeyFile::Held(key) => key,
                KeyFile::Damaged(problem) => return Err(refusal(log, &problem)),
                KeyFile::Absent => {
                    return Err(refusal(log, "the key minted a moment ago is gone"));
                }
            }
        }
    };
    if sealed || !present(log) {
        return Ok(Prepared {
            key: Some(key),
            notes,
        });
    }
    match astream_broker::BrokerLog::seal_plaintext(log, key.bytes()) {
        Ok(done) => {
            notes.push(format!(
                "{}: sealed at rest under {} ({} bytes, up to offset {}); the plaintext it was is \
                 unlinked, not erased: a snapshot or a backup taken before now still holds it",
                log.display(),
                shown(&key_path(log)),
                done.bytes,
                done.head.0
            ));
            Ok(Prepared {
                key: Some(key),
                notes,
            })
        }
        // A seal a crash stopped past its marker: the open inside it finished the switch.
        Err(_) if is_sealed(log) => Ok(Prepared {
            key: Some(key),
            notes,
        }),
        // A broker holds the log: this start's open says so, as it always has.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(Prepared { key: None, notes }),
        Err(e) => {
            notes.push(format!(
                "{}: not sealed at rest ({e}); its records stay as they are: a log that reads as \
                 plaintext serves plaintext and the next start tries again, and one that does not \
                 is refused by the open that follows",
                log.display()
            ));
            Ok(Prepared { key: None, notes })
        }
    }
}

/// What [`ensure_key`] found or did.
#[derive(Debug, PartialEq, Eq)]
pub struct KeyStep {
    /// A key was minted, or (a dry run) would be; otherwise one is there.
    pub minted: bool,
    /// Why the key could not be left out of backups, when it could not.
    pub unmarked: Option<String>,
}

/// `aterm fabric on`'s key step: the key beside `log`, minted where there is none and no log is
/// sealed, and left out of backups. A dry run reads, as the real run will once the folder is 0700,
/// and writes nothing.
///
/// # Errors
/// [`Refused`] for a missing or damaged key or a [`retire`] that stopped part way, as a broker's
/// start would; a mint's own error as its `why`.
pub fn ensure_key(log: &Path, backups: Backups, dry_run: bool) -> Result<KeyStep, Refused> {
    if let Some(refused) = unfinished_retire(log) {
        return Err(refused);
    }
    let reader = if dry_run {
        crate::transport::read_mint_secret_narrowed
    } else {
        sweep_key_temporaries(log);
        crate::transport::read_mint_secret
    };
    let step = |minted, marked: io::Result<()>| KeyStep {
        minted,
        unmarked: marked.err().map(|e| unmarked_note(log, &e)),
    };
    match read_with(log, reader) {
        KeyFile::Held(_) if dry_run => Ok(step(false, Ok(()))),
        KeyFile::Held(_) => Ok(step(false, mark_existing(log, backups))),
        KeyFile::Damaged(problem) => Err(refusal(log, &problem)),
        KeyFile::Absent if is_sealed(log) => Err(refusal(
            log,
            &format!("{}: no such file", shown(&key_path(log))),
        )),
        KeyFile::Absent if dry_run => Ok(step(true, Ok(()))),
        KeyFile::Absent => match mint(log, backups) {
            Ok(Some(marked)) => Ok(step(true, marked)),
            Ok(None) => Ok(step(false, Ok(()))),
            Err(e) => Err(Refused {
                why: format!("{}: {e}", shown(&key_path(log))),
                remedy: "fix that and run it again".to_string(),
            }),
        },
    }
}

/// What [`retire`] did.
#[derive(Debug)]
pub struct Retired {
    /// The folder the log went to.
    pub to: PathBuf,
    /// What that folder holds, the log among it.
    pub moved: Vec<String>,
    /// Whether a new key now waits for the next start's new log.
    pub minted: bool,
}

/// The log's lock through [`retire`]'s moves, released by `LOCK_UN` before the close (`clippy.toml`'s
/// flock release rule).
struct RetireLock(std::fs::File);

impl Drop for RetireLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// `log`'s lock, or the refusal of a log a running broker holds.
fn retire_lock(log: &Path) -> Result<RetireLock, String> {
    let at = log.display();
    let holding = std::fs::File::open(log).map_err(|e| format!("{at}: {e}"))?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard's own take: its Drop releases by LOCK_UN"
    )]
    let taken = holding.try_lock();
    if taken.is_err() {
        return Err(format!(
            "{at} is held by a running broker; it is left as it was"
        ));
    }
    Ok(RetireLock(holding))
}

/// `Ok` for a file with what a bus log has beside it. Anything else is refused before anything is
/// opened or moved: a folder would go whole, its running broker's log and its mint secret with it,
/// and a file no log ever was would go with whatever shares its name.
fn is_a_log(log: &Path) -> Result<(), String> {
    let at = log.display();
    let meta = std::fs::symlink_metadata(log).map_err(|e| format!("{at}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{at} is not a file; it is left as it was"));
    }
    if !present(&beside(log, ".id")) && !present(&seal_path(log)) {
        return Err(format!(
            "{at} is no bus log: every log that has been served has a {} beside it, and a sealed \
             one a {}; it is left as it was",
            shown(&beside(log, ".id")),
            shown(&seal_path(log))
        ));
    }
    Ok(())
}

/// The refusal of a log that is not to be given up: it opens as it stands, or a broker holds it.
fn refuses_retire(log: &Path) -> Result<(), String> {
    use astream_broker::{BrokerLog, Durability};
    let at = log.display();
    let sealed = is_sealed(log);
    let opened = match read_key(log) {
        KeyFile::Held(key) if sealed => BrokerLog::open_encrypted_bound(log, key.bytes()),
        _ if sealed => Err(io::Error::new(io::ErrorKind::InvalidData, "no usable key")),
        _ => BrokerLog::open_with(log, Durability::Strict),
    };
    match opened {
        Ok(mut opened) => {
            let _ = opened.close_clean();
            Err(format!(
                "{at} opens as it stands, so there is nothing to give up; it is left as it was"
            ))
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(format!(
            "{at} is held by a running broker ({e}); it is left as it was"
        )),
        Err(_) => Ok(()),
    }
}

/// The folder a stopped [`retire`] of `log` was moving to, from its marker; `None` with no marker.
fn retiring_to(log: &Path) -> Result<Option<PathBuf>, String> {
    let marker = beside(log, RETIRING);
    let name = match std::fs::read_to_string(&marker) {
        Ok(name) => name,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", marker.display())),
    };
    if !name.starts_with("retired-") || name.contains('/') {
        return Err(format!(
            "{} names no retired folder ({name:?}); nothing moved",
            marker.display()
        ));
    }
    let to = folder_of(log).join(name);
    // A power loss can take the folder's entry and keep the marker's.
    aterm_uds::private_file::create_private_dir(&to)
        .map_err(|e| format!("{}: {e}", to.display()))?;
    Ok(Some(to))
}

/// Begin [`retire`]'s moves: the new folder, then the marker naming it, durable before anything
/// moves.
fn begin_retire(log: &Path) -> Result<PathBuf, String> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let name = format!("retired-{ms}");
    let to = folder_of(log).join(&name);
    aterm_uds::private_file::create_private_dir(&to)
        .map_err(|e| format!("{}: {e}", to.display()))?;
    let marker = beside(log, RETIRING);
    aterm_uds::atomic_file::write(&marker, name.as_bytes(), 0o600)
        .map_err(|e| format!("{}: {e}", marker.display()))?;
    Ok(to)
}

/// What [`retire`] has yet to move out of `log`'s folder, in order: every sidecar, the log, its
/// key. The marker is not among it: it goes when these have.
fn retire_names(log: &Path) -> Result<Vec<String>, String> {
    let at = log.display();
    let dir = folder_of(log);
    let name = log
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{at} names no file"))?;
    let prefix = format!("{name}.");
    let marker = format!("{name}{RETIRING}");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|entry| entry.starts_with(&prefix) && *entry != marker)
        .collect();
    names.sort();
    if present(log) {
        names.push(name);
    }
    if present(&key_path(log)) {
        names.push(REST_KEY.to_string());
    }
    Ok(names)
}

/// Give a log up that will not open: move it, every sidecar it has and its key to a new
/// `retired-<ms>` folder beside it, and mint a new key. Nothing is deleted, so a key found later
/// still reads it. The next start creates a new log with a new identity, which every bridge takes
/// as a replaced log.
///
/// `<log>.retiring`, made durable before the first move and removed after the last, names the
/// folder. While it stands a start refuses and mints nothing ([`unfinished_retire`]), so a crash
/// part way never has a new key minted beside sealed records that have lost their `.seal`, and
/// running this again moves the rest to the same folder, whatever the log itself would say by
/// then.
///
/// # Errors
/// It names no file with what a log has beside it; there is no log; it opens (nothing to give up);
/// a broker holds it; or a move fails, where a later run takes up what is left.
pub fn retire(log: &Path, backups: Backups) -> Result<Retired, String> {
    let dir = folder_of(log);
    let (to, held) = match retiring_to(log)? {
        Some(to) => (to, present(log).then(|| retire_lock(log)).transpose()?),
        None => {
            is_a_log(log)?;
            refuses_retire(log)?;
            let held = retire_lock(log)?;
            (begin_retire(log)?, Some(held))
        }
    };
    for entry in retire_names(log)? {
        std::fs::rename(dir.join(&entry), to.join(&entry))
            .map_err(|e| format!("{}: {e}", dir.join(&entry).display()))?;
    }
    drop(held);
    let flushed = |folder: &Path| {
        aterm_durable::Flush::Durable
            .dir(folder)
            .map_err(|e| format!("{}: {e}", folder.display()))
    };
    flushed(&to)?;
    flushed(dir)?;
    let marker = beside(log, RETIRING);
    std::fs::remove_file(&marker).map_err(|e| format!("{}: {e}", marker.display()))?;
    flushed(dir)?;
    let mut moved: Vec<String> = std::fs::read_dir(&to)
        .map_err(|e| format!("{}: {e}", to.display()))?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    moved.sort();
    let name = log
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let keyed = moved.iter().any(|entry| *entry == REST_KEY);
    let sealed = moved.contains(&format!("{name}.seal"));
    let minted = (sealed || keyed) && matches!(mint(log, backups), Ok(Some(_)));
    Ok(Retired { to, moved, minted })
}

#[cfg(test)]
#[path = "at_rest_tests.rs"]
mod tests;
