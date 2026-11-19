// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rewrite: the second transition of a records entry, beside the one that makes a records
//! file a layout (`docs/DESIGN-vault-log-2026-11-14.md` step 4, tracker FL-148D). The first only
//! ever grows what the entry holds; this one replaces it with a smaller file (a log pruned at a
//! final checkpoint, or one whose superseded bodies were shed), which is the only way those
//! bytes leave the disk. Its one caller outside the tests is a Mac that reads format 2 taking a
//! served root into its file (`client::persist_stored`), and the one reader of what it writes
//! is that Mac's records cache ([`selected`]); no running build reads format 2, so it stays
//! dark (FL-148F runs the pass that prunes and sheds).
//!
//! **The steps** ([`Step`], in this order):
//!
//! 1. `Intent`: `<entry>.rewriting` beside the entry declares the rewrite: the generation it
//!    makes, the predecessor's data (length and SHA-256), the original records file kept beside
//!    the predecessor where one is, the new data (length and SHA-256) and what the rewrite is
//!    ([`What`]: the checkpoint it roots at, or the set it sheds). The new data is measured
//!    before a byte of it is written, so the intent names it.
//! 2. `Copied`: the new data is written to a scratch file in the layout, flushed, read back
//!    against the intent, and given its name, `current.<generation>`.
//! 3. `Prepared`: the new layout header, the intent's own bytes, is `layout.<generation>`.
//! 4. `Pinned`: the pin beside the entry is replaced, by one rename, with one that names the
//!    new header, and the parent is flushed. This is the one point at which the entry changes:
//!    before it every reader selects the predecessor, whole, and after it the new data, whole.
//! 5. `Exchanged`: the predecessor's files are unlinked ([`Context::retire`]): first, beside
//!    the entry, the vault's head, which names the predecessor, and the original records file
//!    the first transition kept, and the parent is flushed; then the predecessor's data and
//!    header, and the layout is flushed; then the first transition's own intent. Then the
//!    rewrite's intent is removed.
//!
//! **Recovery** ([`recover`]) goes by the pin and nothing else: it never reads the intent, which
//! only says that a rewrite was cut short. While an intent stands, every file of the layout that
//! is not of the pinned generation is what that rewrite left: the next generation's where the
//! pin had not moved (the entry is then exactly the predecessor), the one before's where it had
//! (exactly the new store). It removes them, then the intent, and running it again removes
//! nothing more. Before it unlinks a predecessor it flushes the parent, so the pin that no
//! longer selects that predecessor is durable first, and reads the pinned data against the
//! header the pin names (below). It removes the head only while a file of the generation
//! before the pinned one still stands: a rewrite unlinks the head, and flushes that, before any
//! of them, so a head beside an entry whose predecessor is gone is the pinned data's own.
//!
//! Decided here, and why:
//!
//! - **A rewrite names the data it was computed from** ([`Predecessor`]). Its caller reads the
//!   records, computes the smaller file and only then takes the writer's lock here; a record
//!   appended between the two would be dropped unseen. So the rewrite is refused unless the
//!   entry holds, under the lock, exactly the bytes the caller read, and the intent records
//!   them. The pin is checked against them once more just before it moves. A records file kept
//!   whole is held to the same, and its rewrite measured, before it becomes a layout
//!   ([`Context::takes`]): a rewrite refused for either leaves the file a file.
//! - **New names, and the pin alone commits.** The new data and header take names the
//!   predecessor does not use, so nothing a reader selects is written in place, and no reader
//!   is refused while a rewrite runs: "the previous file stands until the pin" (the design's
//!   step 5). An exchange of two directories, as the first transition does, would leave a window
//!   in which the pin and the directory disagree and every reader waits on a writer.
//! - **Back before the pin, forward after.** A rewrite cut short before its pin is undone, not
//!   finished, even where its data is whole: a prune or a shed is a pure function of the
//!   predecessor, so the pass makes it again, while a build that knows no rewrite may have
//!   appended to the predecessor since (it holds the same writer lock and reads no intent), and
//!   finishing would drop what it wrote.
//! - **No predecessor is unlinked under a pin that may not be durable.** A rename is durable
//!   once its directory's flush returns. A rewrite whose flush of the pin's rename fails, or
//!   that dies before it, leaves a pin that a power cut may still take back: the predecessor
//!   must then be whole. So recovery flushes the parent before it goes forward and stops,
//!   removing nothing, where that fails; and [`rewrite`] answers the new data only where
//!   recovery finished.
//! - **The predecessor goes only once the new data reads as declared.** Recovery that goes
//!   forward reads the pinned data file's first bytes against the length and SHA-256 its header
//!   declares, while the predecessor's data still stands, and is refused, removing nothing,
//!   where they differ: the read back before the pin was of the page cache, and this one is of
//!   what the disk kept. Bytes appended since are past the declared length and are kept.
//! - **The header's `data` row is the rewrite's, not the file's.** A writer that reads format
//!   2 appends to the data file, or replaces it whole with its store, as writers of format 1 do
//!   to `current`, whose header's `initial` row is likewise never read once its pin stands. So
//!   no reader holds the pinned file to that row once the rewrite is done: each record in the
//!   file is signed, a rooted file's frame is bound by its roots
//!   ([`crate::membership_log::stored`]), and a file cut short reads as the records it still
//!   holds, as a format-1 file does.
//! - **The format-2 layout.** A header of eight rows under `aterm-records-layout/2` and a pin of
//!   four under `aterm-records-pin/2`, written by a rewrite alone. A build that reads only
//!   format 1 refuses both whole: its pin check compares the pin's every byte with the one its
//!   own header makes, and its header takes exactly six rows under `/1`, so it opens no data
//!   file. Format 1's header, pin, names and transition are untouched, byte for byte.
//! - **This build's own readers refuse it too.** [`super::data_path`], [`super::selected`],
//!   [`super::write_path`] and [`super::recover_pending`] are format-1 readers and know nothing
//!   of this module: they refuse a rewritten layout as an older build does, and read an entry
//!   whose rewrite was cut short before its pin as the predecessor it is. So no path that
//!   reads records alone is ever handed a file that holds a root's stubs. A reader that reads
//!   format 2 selects with [`data_path`].
//! - **The generation is a number and the rewrite that made it** ([`RecordsGeneration`],
//!   [`generation`]): the number is 1 at the first rewrite and one more at each, 0 for an entry
//!   never rewritten, and the rewrite is named by the SHA-256 of its header, the pin's second
//!   row, which binds the data it was computed from and the data it wrote. A consumer that
//!   keeps a place in the records (the mesh egress's members cursor) compares both for
//!   equality and places itself again when either differs. The number alone does not name
//!   what the entry holds: a rewrite whose pin a power cut took back is made again from what
//!   the entry holds by then, under the same number, and an entry removed and made again
//!   counts from 0 again and reaches it. Two rewrites of one number that share a header wrote
//!   the same bytes from the same bytes, so a place in one is a place in the other. An entry
//!   never rewritten has no rewrite to name: a place kept across the removal of such an entry
//!   and the making of another, both at 0, is not told apart here (the egress's module says
//!   what that costs).
//! - **The intent is the header.** They are the same bytes, so a header in the layout that no
//!   intent declared is not installed over, and a rewrite declares everything it will pin
//!   before it writes any of it.
//! - **Every scratch file is read back before its rename**, the data against the digest the
//!   intent declared: a write a disk cut short is refused while the predecessor still stands.
//! - **The original records file goes with the predecessor, and the header names it.** The
//!   first transition keeps the records file it copied beside the directory, so a writer that
//!   held it open could change nothing a reader selects. A rewrite that left it would free none
//!   of the bytes it was made to free, and would keep the dropped records' ciphertext on disk
//!   for good. Unlinking the name harms no such writer: its descriptor keeps the file, which
//!   nothing reads. Its name is in the predecessor's format-1 header alone, which is itself
//!   unlinked, so the first rewrite's header carries it (`kept`): recovery after the pin reads
//!   it from the header the pin names and needs no file of the predecessor to read, or to
//!   stand, to finish.
//! - **The head is removed, not written.** The head beside the entry names the predecessor by
//!   its stamp and digest, so no open reads it for the new file; left there it would keep the
//!   dropped records' frames. The head of the new file is written as any head is, by the first
//!   resolve of an open that began once the file had settled ([`crate::membership_log::head`]):
//!   a head names only a stamp that had settled, which a file just written has not.
//! - **An intent beside an entry with no pin declares nothing.** A rewrite declares its intent
//!   only under a pin and never removes a pin, so such an intent came with a restored folder:
//!   recovery removes it and its scratch, and nothing else.
//! - **Only files are a rewrite's.** Something that is no file at a name a rewrite writes (its
//!   intent, the next generation's data and header, their scratch and the pin's) is not this
//!   module's to remove: a rewrite is refused before it declares anything, and before a
//!   records file kept whole becomes a layout, and recovery before it removes anything.
//! - **What a rewrite is, is the caller's word.** The intent records the checkpoint or the
//!   shed set (its count and digest, so the intent stays bounded); this layer reads bytes and
//!   does not check them against it.
//! - **The byte bound is the format's** ([`crate::crossing::capacity`]), the one place a later
//!   change (tracker FL-249) raises it, and it is of the file: a rooted log's frame and its
//!   records together.
//! - **Unix only**: its looks at a name read a Unix mode (`fstatat`'s `st_mode`), which has no
//!   Windows arm here. A records file kept whole becomes a layout on Windows too, by an exchange
//!   of three renames (`super::Context::exchange`), so a Windows arm needs only those looks.
//!
//! **A writer that reads format 2** ([`write_path`], `client::persist_stored`) writes
//! `current.<generation>`: it recovers under the writer's lock before it writes, as
//! `super::write_path` does for the first transition, so no write meets a standing intent;
//! it holds the file, frame and records, to the format's bound, which a log's own byte bound
//! does not count a frame's roots toward; it writes only to the generation it selected, and
//! only a store rooted by the frame that generation's file begins with. A store rooted
//! otherwise is a rewrite ([`rewrite`]) from the bytes the writer read, every record of which
//! its store holds or dropped.
//!
//! Not yet:
//!
//! - That writer does not raise the pin's capacity where this build's record budget is larger
//!   than the one the rewrite pinned ([`super::write_path`] does for a format-1 pin). Its hook
//!   ([`crate::membership_log::on_written`]) hands over a generation with the store
//!   ([`crate::membership_log::store::Written::generation`]): the client's writer hands the
//!   generation of an entry never rewritten, and a marked Mac's write hands its hook nothing
//!   (`client::records_written`), so no rewritten generation is handed yet; the mesh egress
//!   takes the records a write hands it only under the generation its cursor counts
//!   (`aterm_mesh::egress::MembersCursor`).
//! - A reader in flight across a rewrite may write a head that names the predecessor after the
//!   rewrite removed it (a head's write takes no writer lock). No open reads it, and the next
//!   head written replaces it; until then it holds the dropped records' frames.
//! - The rewrite passes none of a records write's guards (the retained history's, the crossed
//!   mark, the `records_written` hook): its caller owns them (`client::write_records` for a
//!   served root; the pass, FL-148F, for a prune or a shed).

use super::{
    Context, DATA, HEADER, META_LIMIT, Step, check_capacity, durable_dir, hex, pin_path,
    read_bounded, read_file, recover_locked, refused, regular, scratch, syscall, unhex,
    writer_lock,
};
use crate::crossing::{FORMAT_ONE, FORMAT_TWO};
use crate::{Result, fsx};
use aterm_digest::Sha256;
use aterm_dirfd::{self as fd, AtFlags, FileType, RenameFlags};
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The first row of a layout header a rewrite writes.
const HEADER_MAGIC: &str = "aterm-records-layout/2";
/// The first row of a pin a rewrite writes.
const PIN_MAGIC: &str = "aterm-records-pin/2";

/// A broken rewrite, for the tests' negative controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mutant {
    /// The predecessor's files are unlinked before the pin moves.
    UnlinkFirst,
    /// The pin moves as soon as the intent is declared, before the new data and header stand.
    PinFirst,
    /// Recovery leaves what an interrupted rewrite wrote, its intent included.
    NoRecovery,
    /// No scratch file is read back before its rename.
    Unverified,
    /// The rewrite writes its data over `current`, under the format-1 header and pin.
    InPlace,
    /// A rewrite cut short before its pin is finished where its data and header are whole.
    ForwardBeforePin,
    /// The rewrite replaces whatever the entry holds, not only the data it was computed from.
    Unread,
    /// Recovery unlinks the predecessor of the pinned generation though the flush that makes
    /// the pin's rename durable failed.
    RetireUnflushed,
    /// The predecessor's data and header are unlinked before the unlink of the head and of the
    /// kept original is flushed.
    RetireUnordered,
    /// The kept original's name is read from the predecessor's own header when it is unlinked,
    /// not from the header the pin names.
    KeptUnnamed,
    /// Recovery unlinks the predecessor without reading the pinned data against its header.
    ForwardUnread,
    /// Recovery removes the head whether or not a file of the predecessor stands.
    HeadAlways,
    /// An intent beside an entry that has no pin is held to a pin, as any other.
    StaleIntentKept,
    /// Nothing looks at what stands at a rewrite's names before it declares or recovers.
    Unlooked,
    /// A records file kept whole becomes a layout before its rewrite is held to what it holds.
    PrepareFirst,
    /// The generation is its number alone: no rewrite is named.
    NumberAlone,
}

/// A point at which a test ends a rewrite as a crash would, or loses part of a write.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cut {
    /// The process dies while it writes this scratch file: half of it stands, unrenamed.
    Torn(Piece),
    /// The process dies with this scratch file whole and flushed, before its rename.
    Flushed(Piece),
    /// The disk loses the second half of this scratch file and reports no error.
    Lost(Piece),
    /// The process dies once it has unlinked this many files, counted from when the cut is set.
    Unlinked(usize),
    /// The flush of the parent that makes the pin's rename durable fails, in the rewrite and in
    /// every recovery, while the cut stands.
    Unflushed,
}

/// The files a rewrite writes.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Piece {
    Intent,
    Data,
    Header,
    Pin,
}

#[cfg(test)]
thread_local! {
    /// The mutant this thread's rewrites run as, if any.
    pub(super) static MUTANT: std::cell::Cell<Option<Mutant>> = const { std::cell::Cell::new(None) };
    /// Where this thread's next rewrite or recovery is cut, if anywhere.
    pub(super) static CUT: std::cell::Cell<Option<Cut>> = const { std::cell::Cell::new(None) };
    /// Each unlink this thread's rewrites and recoveries made (`unlink <name>`) and each flush
    /// of a directory [`Context::retire`] made (`flush parent`, `flush layout`), in order.
    pub(super) static TRACE: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn mutant(which: Mutant) -> bool {
    MUTANT.get() == Some(which)
}

/// [`Cut::Torn`], [`Cut::Flushed`] and [`Cut::Lost`] at the scratch file of `name`, open as
/// `file`.
#[cfg(test)]
fn cut_scratch(name: &str, file: &File) -> Result<()> {
    let piece = if name.starts_with(DATA) {
        Piece::Data
    } else if name.starts_with(HEADER) {
        Piece::Header
    } else if name.ends_with(".rewriting") {
        Piece::Intent
    } else {
        Piece::Pin
    };
    let half = || -> Result<()> { Ok(file.set_len(file.metadata()?.len() / 2)?) };
    match CUT.get() {
        Some(Cut::Torn(at)) if at == piece => {
            half()?;
            Err(refused("a test's crash tore a scratch file"))
        }
        Some(Cut::Flushed(at)) if at == piece => {
            Err(refused("a test's crash came before a rename"))
        }
        Some(Cut::Lost(at)) if at == piece => half(),
        _ => Ok(()),
    }
}

/// What a rewrite is: what its intent and header record of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum What {
    /// The log pruned at the final checkpoint of this id
    /// ([`crate::membership_log::checkpoint::checkpoint_id`]).
    Checkpoint([u8; 32]),
    /// The bodies of this many records shed, with the SHA-256 of their ids in ascending order.
    Shed {
        /// How many records' bodies it shed.
        records: u32,
        /// SHA-256 of their ids, ascending.
        digest: [u8; 32],
    },
}

impl What {
    fn encode(&self) -> String {
        match self {
            Self::Checkpoint(id) => format!("checkpoint {}", hex(id)),
            Self::Shed { records, digest } => format!("shed {records} {}", hex(digest)),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        if let Some(id) = text.strip_prefix("checkpoint ") {
            return unhex(id).map(Self::Checkpoint);
        }
        let (records, digest) = text.strip_prefix("shed ")?.split_once(' ')?;
        Some(Self::Shed {
            records: records.parse().ok()?,
            digest: unhex(digest)?,
        })
    }
}

/// The most bytes the data of a layout of `format` holds: that format's capacity
/// ([`crate::crossing::capacity`], the storage class of byte bound).
const fn bytes_of(format: u32) -> usize {
    crate::membership_log::Reading::Format(format).capacity()
}

/// The format of the generation `number`: 1 for an entry never rewritten.
const fn format_of(number: u64) -> u32 {
    if number == 0 { FORMAT_ONE } else { FORMAT_TWO }
}

/// The data file of the generation `number` in the layout: `current` before any rewrite.
fn data_name(number: u64) -> String {
    if number == 0 {
        DATA.to_owned()
    } else {
        format!("{DATA}.{number}")
    }
}

/// The header of the generation `number` in the layout: `layout` before any rewrite.
fn header_name(number: u64) -> String {
    if number == 0 {
        HEADER.to_owned()
    } else {
        format!("{HEADER}.{number}")
    }
}

/// A layout header a rewrite writes, and its intent: one generation of the entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Generation {
    /// SHA-256 of the entry's name, as a format-1 header binds it.
    entry: [u8; 32],
    /// Which rewrite of the entry made it, from 1.
    number: u64,
    /// The predecessor's data when the rewrite was declared: its length and SHA-256.
    from: (usize, [u8; 32]),
    /// The nonce of the original records file the first transition kept beside the predecessor
    /// (`.aterm-records-<nonce>`), which goes with it: only the first rewrite of an entry has
    /// one, and only where the entry was a records file before it was a layout.
    kept: Option<String>,
    /// The data the rewrite wrote: its length and SHA-256. Later appends grow the file past it.
    data: (usize, [u8; 32]),
    what: What,
    /// The most records its writer held ([`crate::membership_log::RecordBudgets::held`]): the
    /// least a reader must hold, which the pin may raise.
    capacity: usize,
}

impl Generation {
    fn encode(&self) -> Vec<u8> {
        format!(
            "{HEADER_MAGIC}\nentry {}\ngeneration {}\nfrom {} {}\nkept {}\ndata {} {}\nrewrite {}\ncapacity {}\n",
            hex(&self.entry),
            self.number,
            self.from.0,
            hex(&self.from.1),
            self.kept.as_deref().unwrap_or("-"),
            self.data.0,
            hex(&self.data.1),
            self.what.encode(),
            self.capacity
        )
        .into_bytes()
    }

    /// The pin that names this header, requiring `capacity` of its reader.
    fn pin(&self, capacity: usize) -> Vec<u8> {
        format!(
            "{PIN_MAGIC}\n{}\ncapacity {capacity}\ngeneration {}\n",
            hex(&Sha256::digest(self.encode())),
            self.number
        )
        .into_bytes()
    }

    fn decode(wire: &[u8]) -> Result<Self> {
        let invalid = || refused("invalid rewritten layout header");
        let text = std::str::from_utf8(wire).map_err(|_| invalid())?;
        let rows: Vec<_> = text.lines().collect();
        if rows.len() != 8 || rows[0] != HEADER_MAGIC {
            return Err(invalid());
        }
        let custody = |row: &str, word: &str| -> Option<(usize, [u8; 32])> {
            let (len, digest) = row.strip_prefix(word)?.split_once(' ')?;
            Some((len.parse().ok()?, unhex(digest)?))
        };
        let header = Self {
            entry: rows[1]
                .strip_prefix("entry ")
                .and_then(unhex)
                .ok_or_else(invalid)?,
            number: rows[2]
                .strip_prefix("generation ")
                .and_then(|text| text.parse::<u64>().ok())
                .filter(|number| *number > 0)
                .ok_or_else(invalid)?,
            from: custody(rows[3], "from ").ok_or_else(invalid)?,
            kept: match rows[4].strip_prefix("kept ").ok_or_else(invalid)? {
                "-" => None,
                nonce
                    if nonce.len() == 32
                        && nonce
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
                {
                    Some(nonce.to_owned())
                }
                _ => return Err(invalid()),
            },
            data: custody(rows[5], "data ").ok_or_else(invalid)?,
            what: rows[6]
                .strip_prefix("rewrite ")
                .and_then(What::decode)
                .ok_or_else(invalid)?,
            capacity: rows[7]
                .strip_prefix("capacity ")
                .and_then(|text| text.parse::<usize>().ok())
                .filter(|capacity| *capacity > 0 && *capacity <= u32::MAX as usize)
                .ok_or_else(invalid)?,
        };
        if header.data.0 > bytes_of(FORMAT_TWO)
            || header.from.0 > bytes_of(format_of(header.number - 1))
        {
            return Err(refused("rewritten custody exceeds its bound"));
        }
        // Only an entry's first rewrite has a format-1 predecessor, which alone keeps an original.
        if header.kept.is_some() && header.number != 1 {
            return Err(invalid());
        }
        if header.encode() != wire {
            return Err(refused("noncanonical rewritten layout header"));
        }
        Ok(header)
    }
}

/// What a pin a rewrite wrote says: the SHA-256 of the header it names, the capacity it
/// requires of a reader and the generation. `None` for any pin that does not begin with a
/// rewrite's first row: a format-1 pin, which [`Context::active`] judges.
///
/// # Errors
/// The pin begins as a rewrite's and is not exactly one.
fn rewritten_pin(wire: &[u8]) -> Result<Option<([u8; 32], usize, u64)>> {
    let Some(rest) = wire.strip_prefix(format!("{PIN_MAGIC}\n").as_bytes()) else {
        return Ok(None);
    };
    let invalid = || refused("invalid rewritten pin");
    let text = std::str::from_utf8(rest).map_err(|_| invalid())?;
    let rows: Vec<_> = text.lines().collect();
    let [digest, required, number] = rows.as_slice() else {
        return Err(invalid());
    };
    let digest: [u8; 32] = unhex(digest).ok_or_else(invalid)?;
    let required = required
        .strip_prefix("capacity ")
        .and_then(|text| text.parse::<usize>().ok())
        .filter(|capacity| *capacity > 0 && *capacity <= u32::MAX as usize)
        .ok_or_else(invalid)?;
    let number = number
        .strip_prefix("generation ")
        .and_then(|text| text.parse::<u64>().ok())
        .filter(|number| *number > 0)
        .ok_or_else(invalid)?;
    let canonical = format!(
        "{PIN_MAGIC}\n{}\ncapacity {required}\ngeneration {number}\n",
        hex(&digest)
    );
    if canonical.as_bytes() != wire {
        return Err(invalid());
    }
    Ok(Some((digest, required, number)))
}

/// The generation the pin selects, judged.
enum Pinned {
    /// A format-1 layout, never rewritten, requiring this capacity of its reader.
    One { capacity: usize },
    /// A rewritten layout: its header, and the capacity its pin requires.
    Two { header: Generation, capacity: usize },
}

impl Pinned {
    const fn number(&self) -> u64 {
        match self {
            Self::One { .. } => 0,
            Self::Two { header, .. } => header.number,
        }
    }

    const fn capacity(&self) -> usize {
        match self {
            Self::One { capacity } | Self::Two { capacity, .. } => *capacity,
        }
    }
}

/// The data a rewrite replaces, as the caller that computed the new data read it: its length and
/// SHA-256. A rewrite of an entry that holds other bytes by the time it holds the writer's lock
/// is refused, so no record written since that read is dropped unseen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Predecessor {
    /// Its length.
    pub(crate) len: usize,
    /// Its SHA-256.
    pub(crate) digest: [u8; 32],
}

impl Predecessor {
    /// The predecessor whose data is `bytes`.
    pub(crate) fn of(bytes: &[u8]) -> Self {
        Self {
            len: bytes.len(),
            digest: Sha256::digest(bytes),
        }
    }
}

/// What a reader of format 2 selects at an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Custody {
    /// The rewrite generation of the entry ([`generation`]).
    pub(crate) generation: u64,
    /// Its data file.
    pub(crate) path: PathBuf,
}

/// What a rewrite made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rewritten {
    /// The generation it made.
    pub(crate) generation: u64,
    /// The data file it wrote, which the pin now selects.
    pub(crate) path: PathBuf,
    /// The data's length.
    pub(crate) len: usize,
    /// The data's SHA-256.
    pub(crate) digest: [u8; 32],
}

/// What writes a rewrite's data: called twice, to measure the data and then to write it, and
/// refused where the two differ.
pub(crate) type Produce<'a> = &'a dyn Fn(&mut dyn Write) -> std::io::Result<()>;

/// Whether the pin beside `entry` is one a rewrite wrote.
fn is_rewritten(entry: &Path) -> Result<bool> {
    Ok(
        fsx::read_private(&pin_path(entry)?, META_LIMIT)?.is_some_and(|pin| {
            pin.as_slice()
                .starts_with(format!("{PIN_MAGIC}\n").as_bytes())
        }),
    )
}

pub use super::RecordsGeneration;

/// The rewrite generation of `entry` ([`RecordsGeneration`]), read off its pin alone. A
/// consumer that keeps a place in the records compares it for equality and places itself again
/// when it differs: a rewrite drops records and so moves every place.
///
/// # Errors
/// The pin cannot be read, or begins as a rewrite's and is not exactly one.
pub(crate) fn generation(entry: &Path) -> Result<RecordsGeneration> {
    let pin = fsx::read_private(&pin_path(entry)?, META_LIMIT)?;
    let Some((header, _, number)) = pin
        .map(|pin| rewritten_pin(pin.as_slice()))
        .transpose()?
        .flatten()
    else {
        return Ok(RecordsGeneration::default());
    };
    #[cfg(test)]
    if mutant(Mutant::NumberAlone) {
        return Ok(RecordsGeneration {
            number,
            rewrite: None,
        });
    }
    Ok(RecordsGeneration {
        number,
        rewrite: Some(header),
    })
}

/// [`super::data_path`] for a reader that reads format 2: the pinned generation's data file,
/// or what `super::data_path` answers for an entry never rewritten. A rewrite cut short
/// changes nothing here: the pin selects one generation, whole.
///
/// # Errors
/// As [`super::data_path`], or the rewritten layout is not one this reader takes (its pin and
/// header disagree, it requires more capacity than this reader holds, its data is missing).
pub(crate) fn data_path(entry: &Path) -> Result<Custody> {
    if !is_rewritten(entry)? {
        return super::data_path(entry).map(|path| Custody {
            generation: 0,
            path,
        });
    }
    match Context::new(entry)?.pinned()? {
        Pinned::Two { header, .. } => Ok(Custody {
            generation: header.number,
            path: entry.join(data_name(header.number)),
        }),
        Pinned::One { .. } => Err(refused("the layout pin changed during resolution")),
    }
}

/// [`super::selected`] for a reader that reads format 2: [`data_path`]'s file at a rewritten
/// entry, held across the read by selecting it again ([`confirm`]), and what `super::selected`
/// answers at an entry never rewritten.
///
/// # Errors
/// As [`data_path`].
pub(crate) fn selected(entry: &Path) -> Result<super::Selected> {
    if !is_rewritten(entry)? {
        return super::selected(entry);
    }
    Ok(super::Selected {
        entry: entry.to_path_buf(),
        path: data_path(entry)?.path,
        witnesses: None,
        rewritten: true,
    })
}

/// Hold a selection of `path` at the rewritten `entry` after its data was read
/// ([`super::Selected::confirm`]): the pin still selects that file. A rewrite since moved the
/// pin to another generation's, whose file has another name.
///
/// # Errors
/// As [`data_path`], or the entry now selects another file.
pub(super) fn confirm(entry: &Path, path: &Path) -> Result<()> {
    if data_path(entry)?.path == path {
        Ok(())
    } else {
        Err(refused(CHANGED))
    }
}

/// [`super::write_path`] for a writer that reads format 2, at an entry whose generation was
/// `under` when the writer read the store it writes: the data file of that generation, with
/// the entry's writer lock held until the target is dropped. A rewrite cut short is undone or
/// finished first ([`recover`]), so no write meets a standing intent. The writer appends to
/// the file or replaces it whole, as a format-1 writer does to `current`; the file's frame is
/// the writer's to keep ([`crate::membership_log::stored`]).
///
/// # Errors
/// As [`recover`]; or the entry is no longer of generation `under` (a rewrite since dropped
/// records the writer's store may hold: read again), or was never rewritten, which
/// `super::write_path` writes.
pub(crate) fn write_path(entry: &Path, under: u64) -> Result<super::WriteTarget> {
    let guard = writer_lock(entry)?;
    let context = recover_under_lock(entry)?;
    match context.pinned()? {
        Pinned::Two { header, .. } if header.number == under => Ok(super::WriteTarget {
            path: entry.join(data_name(header.number)),
            _guard: guard,
        }),
        _ => Err(refused(CHANGED)),
    }
}

/// Undo or finish a rewrite of `entry` that was cut short, whichever its pin says (the
/// module's recovery), after any pending first transition. Running it again changes nothing.
///
/// # Errors
/// As [`super::recover_pending`]; the pin is not one this reader takes, or something that is
/// no file stands at a name a rewrite writes: then nothing is removed. Where the pin had
/// moved: the flush that makes it durable fails, or the pinned data is not what its header
/// declares while its predecessor's data stands: then nothing of the predecessor is removed.
pub(crate) fn recover(entry: &Path) -> Result<()> {
    let _guard = writer_lock(entry)?;
    recover_under_lock(entry).map(|_| ())
}

fn recover_under_lock(entry: &Path) -> Result<Context> {
    // A rewritten pin leaves no first transition pending, and the format-1 recovery refuses it.
    let rewritten = is_rewritten(entry)?;
    if !rewritten {
        recover_locked(entry)?;
    }
    let context = Context::new(entry)?;
    context.recover_rewrite()?;
    if rewritten {
        // As the format-1 recovery holds a pinned layout to its pin with nothing pending.
        context.pinned()?;
    }
    Ok(context)
}

/// Replace the data `from` that `entry` holds with what `produce` writes, as the rewrite `what`
/// (the module's steps), under the entry's writer lock. A records file kept whole becomes a
/// layout first, once it is known to hold `from` and `produce` to write no more than the format
/// holds. A rewrite that is refused for what the entry holds or for what `produce` measures
/// leaves the entry exactly as it was. One that fails at a later step leaves exactly the
/// predecessor's data (in a layout, where it was a file) or, where its pin had moved, exactly
/// the new data; it answers the new data only where recovery finished, so the pin is durable,
/// and otherwise the error, the entry being whichever its pin says.
///
/// # Errors
/// There are no records to rewrite; the entry holds other data than `from` (a writer appended
/// since the caller read it: read again and compute the rewrite again); the layout is not one
/// this reader takes; something that is no file stands at a name the rewrite writes; the
/// generation is spent; `produce` fails, writes more than a format-2 layout holds or writes
/// other bytes the second time; or a step fails.
pub(crate) fn rewrite(
    entry: &Path,
    from: Predecessor,
    what: What,
    produce: Produce<'_>,
) -> Result<Rewritten> {
    let _guard = writer_lock(entry)?;
    if super::absent(entry)? {
        return Err(refused("there are no records to rewrite"));
    }
    let context = recover_under_lock(entry)?;
    if fsx::kind(entry)? != fsx::Kind::Dir {
        #[cfg(test)]
        let held = !mutant(Mutant::PrepareFirst);
        #[cfg(not(test))]
        let held = true;
        if held {
            context.takes(from, produce)?;
        }
        context.prepare(|_| Ok(()))?;
    }
    let before = context.pinned()?.number();
    match context.rewrite_with(from, what, produce, &|_| Ok(())) {
        Ok(done) => Ok(done),
        Err(error) => {
            // The pin says which store stands, whatever stopped the rewrite; it is the new one
            // for this caller only once recovery finished, which flushed the pin's rename.
            let recovered = context.recover_rewrite();
            match (recovered, context.pinned()) {
                (Ok(()), Ok(Pinned::Two { header, .. }))
                    if header.number.checked_sub(1) == Some(before) =>
                {
                    Ok(Rewritten::of(entry, &header))
                }
                _ => Err(error),
            }
        }
    }
}

impl Rewritten {
    fn of(entry: &Path, header: &Generation) -> Self {
        Self {
            generation: header.number,
            path: entry.join(data_name(header.number)),
            len: header.data.0,
            digest: header.data.1,
        }
    }
}

impl Context {
    /// The rewrite intent's name in the parent.
    fn rewriting(&self) -> String {
        format!("{}.rewriting", self.name)
    }

    /// A rewritten header's bytes, decoded and held to this entry.
    fn generation(&self, wire: &[u8]) -> Result<Generation> {
        let header = Generation::decode(wire)?;
        if header.entry != Sha256::digest(self.name.as_bytes()) {
            return Err(refused("the layout belongs to another records entry"));
        }
        Ok(header)
    }

    /// The generation the pin selects, judged as a reader of format 2 judges it: a format-1 pin
    /// as [`Context::active`] does, and a rewritten one by the header it names (its digest, its
    /// entry and number, a capacity the pin does not lower and this reader holds) and its data
    /// file, a private file within the format's bound.
    fn pinned(&self) -> Result<Pinned> {
        let pin = read_file(&self.parent, &self.pin, META_LIMIT)?;
        let Some((digest, capacity, number)) = rewritten_pin(&pin)? else {
            return Ok(Pinned::One {
                capacity: self.active()?,
            });
        };
        let directory = self.directory()?;
        let wire = read_file(&directory, &header_name(number), META_LIMIT)?;
        if Sha256::digest(&wire) != digest {
            return Err(refused("the layout pin does not match"));
        }
        let header = self.generation(&wire)?;
        if header.number != number || capacity < header.capacity {
            return Err(refused("the layout pin does not match"));
        }
        check_capacity(capacity)?;
        regular(&directory, &data_name(number), bytes_of(FORMAT_TWO))?;
        Ok(Pinned::Two { header, capacity })
    }

    /// Hold a rewrite of a records file kept whole to what [`Context::rewrite_with`] holds it to
    /// once the file is a layout, before it becomes one: the file is `from`'s data, and what
    /// `produce` writes is no more than a format-2 layout holds.
    fn takes(&self, from: Predecessor, produce: Produce<'_>) -> Result<()> {
        self.beside()?;
        let bound = bytes_of(FORMAT_ONE);
        let mut file = super::optional_regular(&self.parent, &self.name, bound)?
            .ok_or_else(|| refused("there are no records to rewrite"))?;
        #[cfg(test)]
        let compared = !mutant(Mutant::Unread);
        #[cfg(not(test))]
        let compared = true;
        if compared && digest_of(&mut file, bound)? != (from.len, from.digest) {
            return Err(refused(CHANGED));
        }
        produce(&mut Measure::new(bytes_of(FORMAT_TWO)))?;
        Ok(())
    }

    /// Look at each name a rewrite writes beside the entry: its intent, the intent's scratch and
    /// the pin's scratch. (The pin itself is judged by [`Context::pinned`].)
    ///
    /// # Errors
    /// Something that is no file stands at one of them ([`stands`]).
    fn beside(&self) -> Result<()> {
        let rewriting = self.rewriting();
        for name in [
            format!("{rewriting}.building"),
            rewriting,
            format!("{}.building", self.pin),
        ] {
            stands(&self.parent, &name)?;
        }
        Ok(())
    }

    /// The nonce of the original records file the format-1 layout in `directory` keeps beside
    /// it, where it keeps one.
    fn kept_original(&self, directory: &File) -> Result<Option<String>> {
        let first = self.header(directory)?;
        Ok(first.original.is_some().then_some(first.nonce))
    }

    /// MUTANT ([`Mutant::KeptUnnamed`]): the kept original's nonce as the predecessor's own
    /// header in `directory` names it, where that header still stands.
    #[cfg(test)]
    fn kept_by_predecessor(&self, directory: &File) -> Result<Option<String>> {
        let Some(mut first) = super::optional_regular(directory, HEADER, META_LIMIT)? else {
            return Ok(None);
        };
        let first = super::Layout::decode(&read_bounded(&mut first, META_LIMIT)?)?;
        Ok(first.original.is_some().then_some(first.nonce))
    }

    /// The module's recovery, under the writer lock: nothing where no rewrite intent or scratch
    /// of one stands.
    fn recover_rewrite(&self) -> Result<()> {
        let rewriting = self.rewriting();
        let building = format!("{rewriting}.building");
        // Both are looked at: something that is no file at either name is refused here, before
        // anything is removed.
        let intent = stands(&self.parent, &rewriting)?;
        if !stands(&self.parent, &building)? && !intent {
            return Ok(());
        }
        #[cfg(test)]
        if mutant(Mutant::NoRecovery) {
            return Ok(());
        }
        #[cfg(test)]
        if mutant(Mutant::ForwardBeforePin) && self.finish_unpinned().is_ok() {
            return Ok(());
        }
        #[cfg(test)]
        let unpinned = !mutant(Mutant::StaleIntentKept);
        #[cfg(not(test))]
        let unpinned = true;
        if unpinned
            && !present(&self.parent, &self.pin)?
            && fsx::kind(&self.entry)? != fsx::Kind::Dir
        {
            // No pin and no layout: no rewrite began here, so the intent declares nothing.
            remove(&self.parent, &building)?;
            remove(&self.parent, &rewriting)?;
            durable_dir(&self.parent)?;
            return Ok(());
        }
        let pinned = self.pinned()?;
        let directory = self.directory()?;
        // Every refusal comes before the first removal. The pin's scratch, removed last, is
        // only a file, and what a rewrite from the pinned generation wrote before its pin moved
        // is only a file:
        stands(&self.parent, &format!("{}.building", self.pin))?;
        let unpinned = pinned.number().checked_add(1).map(|next| {
            let (data, header) = (data_name(next), header_name(next));
            [
                format!("{data}.building"),
                data,
                format!("{header}.building"),
                header,
            ]
        });
        for name in unpinned.iter().flatten() {
            stands(&directory, name)?;
        }
        // and what a rewrite to the pinned generation had not yet unlinked goes only under a
        // durable pin and beside new data that reads as declared.
        let retiring = match &pinned {
            Pinned::One { .. } => None,
            Pinned::Two { header, .. } => {
                // The pin that no longer selects the predecessor is durable before the
                // predecessor goes: a rewrite that died before its own flush of the rename, or
                // whose flush failed, left a pin a power cut may still take back.
                let flushed = pin_durable(&self.parent);
                #[cfg(test)]
                let flushed = if mutant(Mutant::RetireUnflushed) {
                    Ok(())
                } else {
                    flushed
                };
                flushed?;
                let before = header.number - 1;
                let data = present(&directory, &data_name(before))?;
                #[cfg(test)]
                let read = !mutant(Mutant::ForwardUnread);
                #[cfg(not(test))]
                let read = true;
                // While the predecessor's data stands, it goes only once the new data reads,
                // from the disk, as its header declares.
                if data && read {
                    self.written(&directory, header)?;
                }
                // The head is the predecessor's only while a file of the predecessor stands: a
                // rewrite unlinks the head, and flushes that, before any of them.
                let head = data || present(&directory, &header_name(before))?;
                #[cfg(test)]
                let head = head || mutant(Mutant::HeadAlways);
                Some((before, head, header.kept.as_deref()))
            }
        };
        for name in unpinned.iter().flatten() {
            remove(&directory, name)?;
        }
        if let Some((before, head, kept)) = retiring {
            self.retire(&directory, before, head, kept)?;
        }
        durable_dir(&directory)?;
        remove(&self.parent, &format!("{}.building", self.pin))?;
        remove(&self.parent, &building)?;
        remove(&self.parent, &rewriting)?;
        durable_dir(&self.parent)?;
        Ok(())
    }

    /// Hold the pinned data file to the header the pin names: its first bytes are the length
    /// and SHA-256 the rewrite declared. Bytes past them are a later writer's.
    fn written(&self, directory: &File, header: &Generation) -> Result<()> {
        let mut data = regular(directory, &data_name(header.number), bytes_of(FORMAT_TWO))?;
        let mut digest = Sha256::new();
        let mut left = header.data.0;
        let mut buffer = [0; 64 * 1024];
        while left > 0 {
            let want = left.min(buffer.len());
            let n = match data.read(&mut buffer[..want]) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                read => read?,
            };
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
            left -= n;
        }
        if left > 0 || digest.finalize() != header.data.1 {
            return Err(refused(
                "the rewritten data is not what its header declares; its predecessor is kept",
            ));
        }
        Ok(())
    }

    /// MUTANT ([`Mutant::ForwardBeforePin`]): move the pin to the declared generation where its
    /// header and data stand, without a look at the predecessor.
    #[cfg(test)]
    fn finish_unpinned(&self) -> Result<()> {
        let wire = read_file(&self.parent, &self.rewriting(), META_LIMIT)?;
        let header = self.generation(&wire)?;
        let directory = self.directory()?;
        if read_file(&directory, &header_name(header.number), META_LIMIT)? != wire {
            return Err(refused("no header"));
        }
        regular(&directory, &data_name(header.number), bytes_of(FORMAT_TWO))?;
        publish(&self.parent, &self.pin, &header.pin(header.capacity), true)?;
        self.retire(&directory, header.number - 1, true, header.kept.as_deref())?;
        remove(&self.parent, &self.rewriting())?;
        Ok(())
    }

    /// Unlink the files of the generation `number`, which the pin no longer selects. First,
    /// beside the entry: with `head` the vault's head, which names that generation's data, and
    /// with `kept` the original records file the first transition kept, by its nonce; and the
    /// parent is flushed, so neither outlives a power cut that the unlinks after it survive.
    /// Then the generation's data and header, and the layout is flushed. For an entry never
    /// rewritten before, then the first transition's intent. A file already gone is passed over.
    fn retire(&self, directory: &File, number: u64, head: bool, kept: Option<&str>) -> Result<()> {
        if head {
            remove(&self.parent, &format!("{}.head", self.name))?;
        }
        #[cfg(test)]
        let unnamed;
        #[cfg(test)]
        let kept = if mutant(Mutant::KeptUnnamed) && number == 0 {
            unnamed = self.kept_by_predecessor(directory)?;
            unnamed.as_deref()
        } else {
            kept
        };
        if let Some(nonce) = kept {
            remove(&self.parent, &format!(".aterm-records-{nonce}"))?;
        }
        #[cfg(test)]
        let ordered = !mutant(Mutant::RetireUnordered);
        #[cfg(not(test))]
        let ordered = true;
        if ordered {
            settle(&self.parent, "parent")?;
        }
        remove(directory, &data_name(number))?;
        remove(directory, &header_name(number))?;
        settle(directory, "layout")?;
        if number == 0 {
            remove(&self.parent, &self.intent)?;
            settle(&self.parent, "parent")?;
        }
        Ok(())
    }

    /// The module's steps, `after` called once each is durable: a test ends the rewrite there,
    /// as [`Context::prepare`]'s tests end the first transition.
    fn rewrite_with(
        &self,
        from: Predecessor,
        what: What,
        produce: Produce<'_>,
        after: &dyn Fn(Step) -> Result<()>,
    ) -> Result<Rewritten> {
        let directory = self.directory()?;
        let pinned = self.pinned()?;
        let number = pinned.number();
        let next = number
            .checked_add(1)
            .ok_or_else(|| refused("the rewrite generation is spent"))?;
        let (from_name, from_bound) = (data_name(number), bytes_of(format_of(number)));
        let bound = bytes_of(FORMAT_TWO);
        let kept = if number == 0 {
            self.kept_original(&directory)?
        } else {
            None
        };
        let rewriting = self.rewriting();
        let (name, heading) = (data_name(next), header_name(next));
        // Only a file, which recovery removes, may stand at a name this rewrite writes.
        self.beside()?;
        for unpinned in [&name, &heading] {
            stands(&directory, unpinned)?;
            stands(&directory, &format!("{unpinned}.building"))?;
        }
        let standing = digest_of(
            &mut regular(&directory, &from_name, from_bound)?,
            from_bound,
        )?;
        #[cfg(test)]
        let compared = !mutant(Mutant::Unread);
        #[cfg(not(test))]
        let compared = true;
        if compared && standing != (from.len, from.digest) {
            return Err(refused(CHANGED));
        }
        let mut measure = Measure::new(bound);
        produce(&mut measure)?;
        let header = Generation {
            entry: Sha256::digest(self.name.as_bytes()),
            number: next,
            from: standing,
            kept,
            data: (measure.len, measure.digest.finalize()),
            what,
            capacity: pinned
                .capacity()
                .max(crate::membership_log::RecordBudgets::current().held),
        };
        let wire = header.encode();
        publish(&self.parent, &rewriting, &wire, false)?;
        after(Step::Intent)?;
        #[cfg(test)]
        if mutant(Mutant::PinFirst) {
            publish(&self.parent, &self.pin, &header.pin(header.capacity), true)?;
        }

        let building = format!("{name}.building");
        let mut data = scratch(&directory, &building, bound)?;
        produce(&mut Within {
            file: &mut data,
            left: header.data.0,
        })?;
        aterm_durable::durable(&data)?;
        #[cfg(test)]
        cut_scratch(&name, &data)?;
        #[cfg(test)]
        let read_back = !mutant(Mutant::Unverified);
        #[cfg(not(test))]
        let read_back = true;
        data.seek(SeekFrom::Start(0))?;
        if read_back && digest_of(&mut data, bound)? != header.data {
            return Err(refused(
                "the rewritten data is not what its intent declared",
            ));
        }
        #[cfg(test)]
        if mutant(Mutant::InPlace) {
            syscall(fd::renameat(
                &directory,
                building.as_str(),
                &directory,
                DATA,
            ))?;
            durable_dir(&directory)?;
            remove(&self.parent, &rewriting)?;
            return Ok(Rewritten {
                generation: 0,
                path: self.entry.join(DATA),
                len: header.data.0,
                digest: header.data.1,
            });
        }
        syscall(fd::renameat_with(
            &directory,
            building.as_str(),
            &directory,
            name.as_str(),
            RenameFlags::NOREPLACE,
        ))?;
        durable_dir(&directory)?;
        after(Step::Copied)?;

        publish(&directory, &heading, &wire, false)?;
        after(Step::Prepared)?;

        #[cfg(test)]
        let ordered = !mutant(Mutant::UnlinkFirst) && !mutant(Mutant::PinFirst);
        #[cfg(not(test))]
        let ordered = true;
        #[cfg(test)]
        if mutant(Mutant::UnlinkFirst) {
            self.retire(&directory, number, true, header.kept.as_deref())?;
        }
        // The predecessor the intent declared still stands, under the pin that selected it.
        if ordered
            && (self.pinned()?.number() != number
                || digest_of(
                    &mut regular(&directory, &from_name, from_bound)?,
                    from_bound,
                )? != header.from)
        {
            return Err(refused(
                "the records changed while their rewrite was prepared",
            ));
        }
        publish(&self.parent, &self.pin, &header.pin(header.capacity), true)?;
        after(Step::Pinned)?;

        self.retire(&directory, number, true, header.kept.as_deref())?;
        after(Step::Exchanged)?;
        remove(&self.parent, &rewriting)?;
        durable_dir(&self.parent)?;
        Ok(Rewritten::of(&self.entry, &header))
    }
}

/// What a rewrite computed from other data than the entry holds is refused with.
const CHANGED: &str = "the records changed since their rewrite was computed; read them again";

/// Whether a file stands at `name` in `parent`, a name a rewrite writes.
///
/// # Errors
/// Something that is no file stands there: it is not a rewrite's, and nothing is done to it.
fn stands(parent: &File, name: &str) -> Result<bool> {
    match fd::fstatat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode).is_file() => Ok(true),
        Ok(_) => {
            #[cfg(test)]
            if mutant(Mutant::Unlooked) {
                return Ok(true);
            }
            Err(refused(
                "something that is no file stands at a name a rewrite writes",
            ))
        }
        Err(error) if error.is_noent() => Ok(false),
        Err(error) => syscall(Err(error)),
    }
}

/// Flush `parent`, which holds the pin's rename: what makes the pin durable.
fn pin_durable(parent: &File) -> Result<()> {
    #[cfg(test)]
    if CUT.get() == Some(Cut::Unflushed) {
        return Err(refused(
            "a test's disk failed the flush of the pin's rename",
        ));
    }
    Ok(durable_dir(parent)?)
}

/// Flush `dir`, the layout or its parent as `which` says, for [`Context::retire`].
fn settle(dir: &File, which: &str) -> Result<()> {
    #[cfg(test)]
    TRACE.with_borrow_mut(|trace| trace.push(format!("flush {which}")));
    #[cfg(not(test))]
    let _ = which;
    Ok(durable_dir(dir)?)
}

/// Whether anything stands at `name` in `parent`.
fn present(parent: &File, name: &str) -> Result<bool> {
    match fd::fstatat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(error) if error.is_noent() => Ok(false),
        Err(error) => syscall(Err(error)),
    }
}

/// Unlink `name` in `parent`; nothing where it is already gone.
fn remove(parent: &File, name: &str) -> Result<()> {
    match fd::unlinkat(parent, name, AtFlags::empty()) {
        Ok(()) => {
            #[cfg(test)]
            TRACE.with_borrow_mut(|trace| trace.push(format!("unlink {name}")));
            #[cfg(test)]
            match CUT.get() {
                Some(Cut::Unlinked(1)) => {
                    return Err(refused("a test's crash came after an unlink"));
                }
                Some(Cut::Unlinked(left)) => CUT.set(Some(Cut::Unlinked(left - 1))),
                _ => {}
            }
            Ok(())
        }
        Err(error) if error.is_noent() => Ok(()),
        Err(error) => syscall(Err(error)),
    }
}

/// Give `name` in `parent` the bounded `bytes`: written to its scratch file, flushed, read back,
/// renamed, and the directory flushed. Without `replace` a file already there is left where it
/// holds exactly `bytes` and refused where it holds anything else; with it, which only the pin
/// is published with, it is replaced by the rename ([`pin_durable`]).
fn publish(parent: &File, name: &str, bytes: &[u8], replace: bool) -> Result<()> {
    if bytes.len() > META_LIMIT {
        return Err(refused("rewrite metadata exceeds its bound"));
    }
    if !replace && let Some(mut existing) = super::optional_regular(parent, name, META_LIMIT)? {
        if read_bounded(&mut existing, META_LIMIT)? == bytes {
            return Ok(());
        }
        return Err(refused("existing rewrite metadata differs from its intent"));
    }
    let building = format!("{name}.building");
    let mut file = scratch(parent, &building, META_LIMIT)?;
    file.write_all(bytes)?;
    aterm_durable::durable(&file)?;
    #[cfg(test)]
    cut_scratch(name, &file)?;
    #[cfg(test)]
    let read_back = !mutant(Mutant::Unverified);
    #[cfg(not(test))]
    let read_back = true;
    file.seek(SeekFrom::Start(0))?;
    if read_back && read_bounded(&mut file, META_LIMIT)? != bytes {
        return Err(refused("rewrite metadata did not read back as written"));
    }
    syscall(if replace {
        fd::renameat(parent, building.as_str(), parent, name)
    } else {
        fd::renameat_with(
            parent,
            building.as_str(),
            parent,
            name,
            RenameFlags::NOREPLACE,
        )
    })?;
    if replace {
        pin_durable(parent)
    } else {
        Ok(durable_dir(parent)?)
    }
}

/// The length and SHA-256 of `file` from where it stands, which holds at most `bound` bytes.
fn digest_of(file: &mut File, bound: usize) -> Result<(usize, [u8; 32])> {
    let mut buffer = [0; 64 * 1024];
    let mut len = 0usize;
    let mut digest = Sha256::new();
    loop {
        let n = match file.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            read => read?,
        };
        if n == 0 {
            break;
        }
        len = len
            .checked_add(n)
            .filter(|len| *len <= bound)
            .ok_or_else(|| refused("a record file grew past its bound"))?;
        digest.update(&buffer[..n]);
    }
    Ok((len, digest.finalize()))
}

/// Measures what a rewrite will write: its length, within `bound`, and its SHA-256.
struct Measure {
    len: usize,
    bound: usize,
    digest: Sha256,
}

impl Measure {
    fn new(bound: usize) -> Self {
        Self {
            len: 0,
            bound,
            digest: Sha256::new(),
        }
    }
}

impl Write for Measure {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.len = self
            .len
            .checked_add(bytes.len())
            .filter(|len| *len <= self.bound)
            .ok_or_else(|| {
                std::io::Error::other(
                    "membership records storage: a rewrite holds more than its format's capacity",
                )
            })?;
        self.digest.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Writes a rewrite's data to its scratch file, no more than the intent declared.
struct Within<'a> {
    file: &'a mut File,
    left: usize,
}

impl Write for Within<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.left = self.left.checked_sub(bytes.len()).ok_or_else(|| {
            std::io::Error::other(
                "membership records storage: a rewrite wrote more than its intent declared",
            )
        })?;
        self.file.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
#[path = "rewrite_tests.rs"]
mod tests;
