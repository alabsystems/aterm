// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Persistent bridge state directory (`StateDir`): atomic file replacement, quarantine, and session cursors.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) mod asks;
pub(crate) mod journal;

use journal::Replay as _;

/// One outstanding deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deadline {
    /// The bus offset of the ask (what a reply carries back as `re=`).
    pub off: u64,
    /// The asking session.
    pub sid: String,
    /// When it expires, in `now_ms()`.
    pub at: u64,
    /// The advisory `dl=` the ask carried, echoed onto the verdict.
    pub dl: u64,
    /// The principal whose reply settles it.
    pub to: Option<String>,
}

impl Deadline {
    /// The one line this deadline is stored as.
    #[must_use]
    pub fn render(&self) -> String {
        let mut line = format!(
            "off={} sid={} at={} dl={}",
            self.off, self.sid, self.at, self.dl
        );
        if let Some(to) = &self.to {
            line.push_str(&format!(" to={to}"));
        }
        line
    }

    /// Parse one back; a partial line is `None`, never a deadline to publish a verdict on.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let (mut off, mut sid, mut at, mut dl, mut to) = (None, None, None, None, None);
        let dec = |v: &str| {
            (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .then(|| v.parse().ok())
                .flatten()
        };
        for tok in line.split_whitespace() {
            match tok.split_once('=') {
                Some(("off", v)) => off = dec(v),
                Some(("sid", v)) => sid = Some(v.to_string()),
                Some(("at", v)) => at = dec(v),
                Some(("dl", v)) => dl = dec(v),
                Some(("to", v)) => to = Some(v.to_string()),
                _ => {}
            }
        }
        let sid: String = sid?;
        if sid.is_empty() {
            return None;
        }
        if to.as_deref() == Some("") {
            return None;
        }
        Some(Self {
            off: off?,
            sid,
            at: at?,
            dl: dl?,
            to,
        })
    }
}

/// One `ask`/`task` this node published, and the principal whose reply it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    /// The bus offset of the ask.
    pub off: u64,
    /// The principal whose reply it is.
    pub to: String,
}

impl Asked {
    /// The one line this entry is stored as.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{} {}", self.off, self.to)
    }

    /// Parse one back; a partial line is `None`.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut toks = line.split_whitespace();
        let off = toks
            .next()
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()?;
        let to = toks.next()?.to_string();
        toks.next().is_none().then_some(Self { off, to })
    }
}

/// How many published asks the node remembers the recipient of, newest last.
pub const ASKED_KEEP: usize = 1024;

/// How many `post key=` reservations one session keeps, newest last.
pub const KEYS_KEEP: usize = 4096;

/// How many outstanding deadlines the node keeps, newest offset last.
pub const DEADLINES_KEEP: usize = 4096;

/// How many tofu pins the table keeps, oldest first sight out.
pub const PINS_KEEP: usize = 4096;

/// One session's position on one broadcast topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TopicCursor {
    /// The next offset this topic may deliver from.
    pub next: u64,
}

impl TopicCursor {
    /// Parse the state file's `<topic> <next>` line.
    #[must_use]
    pub fn parse(line: &str) -> Option<(String, TopicCursor)> {
        let mut words = line.split_whitespace();
        let topic = words.next().filter(|t| crate::subject::is_topic(t))?;
        let number = |w: &str| {
            (!w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))
                .then(|| w.parse::<u64>().ok())?
        };
        let next = number(words.next()?)?;
        if let Some(extra) = words.next() {
            number(extra)?;
        }
        if words.next().is_some() {
            return None;
        }
        Some((topic.to_string(), TopicCursor { next }))
    }
}

/// What this node knows of the bus log it reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogWitness {
    /// The highest broker head (one past the newest offset) this node has seen on the log it reads.
    pub high: u64,
    /// The identity of that log, once a broker that answers one was read.
    pub id: Option<astream_broker::LogIdentity>,
}

impl LogWitness {
    /// The one line this is stored as.
    #[must_use]
    pub fn render(&self) -> String {
        match self.id {
            Some(id) => format!("id={id} high={}", self.high),
            None => format!("high={}", self.high),
        }
    }

    /// Parse one back: `None` without a readable `high=`.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let high = line
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("high="))
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()?;
        let id = line
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("id="))
            .and_then(astream_broker::LogIdentity::parse);
        Some(Self { high, id })
    }

    /// Whether a broker answering `head` under `id` is provably not the log this witness saw.
    #[must_use]
    pub fn replaced_by(&self, head: u64, id: Option<astream_broker::LogIdentity>) -> bool {
        head < self.high || matches!((self.id, id), (Some(was), Some(now)) if was != now)
    }

    /// Where the log this witness saw continues on the log a broker answers as `id`, with the `lineage` its same `Hello` told ([`astream_broker::Client::log_lineage`]).
    #[must_use]
    pub fn continues(
        &self,
        id: Option<astream_broker::LogIdentity>,
        lineage: Option<&astream_broker::Lineage>,
    ) -> Option<u64> {
        let (was, now) = (self.id?, id?);
        if was == now {
            return Some(u64::MAX);
        }
        now.continues(was, lineage?)
    }
}

/// One fleet halt this node holds in force ([`StateDir::standing_halts`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingHalt {
    /// The `reason=` the sessions are held under.
    pub reason: String,
    /// `true` when the face this node reads carries no row for it.
    pub kept: bool,
}

impl StandingHalt {
    /// `<fleet> <human> <reason> witnessed|kept`.
    #[must_use]
    pub fn render(&self, fleet: &str, human: &str) -> String {
        let how = if self.kept { "kept" } else { "witnessed" };
        format!("{fleet} {human} {} {how}", self.reason)
    }

    /// Parse one back as `(fleet, human, halt)`.
    #[must_use]
    pub fn parse(line: &str) -> Option<(String, String, Self)> {
        let mut toks = line.split_whitespace();
        let (fleet, human, reason, how) = (toks.next()?, toks.next()?, toks.next()?, toks.next()?);
        if toks.next().is_some() || !human.starts_with("h-") {
            return None;
        }
        let kept = match how {
            "kept" => true,
            "witnessed" => false,
            _ => return None,
        };
        Some((
            fleet.to_string(),
            human.to_string(),
            Self {
                reason: reason.to_string(),
                kept,
            },
        ))
    }
}

fn safe_line(line: &str) -> String {
    line.chars().filter(|c| !c.is_control()).take(120).collect()
}

/// Whether a clean disk heals `e`.
pub(crate) fn a_clean_disk_heals(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    ) || e.raw_os_error() == Some(libc::EIO)
}

/// Whether a durable write really flushes (`F_FULLFSYNC` on macOS).
pub(crate) const fn flushes_media() -> bool {
    !cfg!(test)
}

/// Flush a directory, so the entries a rename put in it survive a power loss.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    sync_dir_as(dir, aterm_durable::Flush::Durable)
}

/// [`sync_dir`] with `flush`.
pub(crate) fn sync_dir_as(dir: &Path, flush: aterm_durable::Flush) -> io::Result<()> {
    tolerate_dir_flush(sync_dir_required_as(dir, flush))
}

/// Go on where the directory's entries cannot be flushed here, by astream's one rule
/// (`astream_sys::dir_flush_unavailable`, the broker's own judgement); every other failure
/// is returned. A required flush ([`sync_dir_required_as`], the clean-stop path) takes no
/// tolerance.
fn tolerate_dir_flush(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if astream_sys::dir_flush_unavailable(&e) => Ok(()),
        other => other,
    }
}

fn open_dir_nonblock(dir: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(dir)
}

fn sync_dir_required_as(dir: &Path, flush: aterm_durable::Flush) -> io::Result<()> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    open_dir_nonblock(dir).and_then(|d| {
        if flushes_media() {
            flush.file(&d)
        } else {
            Ok(())
        }
    })
}

/// Keep the file at `path` as a second link under the first free `<name>.aside[.N]` beside it,
/// so it outlives `path` being replaced, and return that path. A link whose directory flush
/// fails is taken back ([`take_back_aside`]).
/// # Errors
/// The link's own error (a directory cannot be linked), the directory's flush, or every name up
/// to `.aside.999` taken.
pub(crate) fn keep_aside(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    for n in 0..1000u32 {
        let to = path.with_file_name(match n {
            0 => format!("{name}.aside"),
            n => format!("{name}.aside.{n}"),
        });
        match std::fs::hard_link(path, &to) {
            Ok(()) => {
                if let Err(e) = sync_dir(path.parent().unwrap_or(Path::new(""))) {
                    take_back_aside(path, &to);
                    return Err(e);
                }
                return Ok(to);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!(
                        "cannot keep {} aside as {}: {e}",
                        path.display(),
                        to.display()
                    ),
                ));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "cannot keep {} aside: {name}.aside to {name}.aside.999 are taken",
            path.display()
        ),
    ))
}

/// Take back the link [`keep_aside`] made at `aside` once replacing `path` failed: it goes only
/// while it is still a second link to the file at `path`, so what it kept is never lost.
pub(crate) fn take_back_aside(path: &Path, aside: &Path) {
    use std::os::unix::fs::MetadataExt as _;
    let id = |p: &Path| {
        std::fs::symlink_metadata(p)
            .ok()
            .map(|m| (m.dev(), m.ino()))
    };
    if id(aside).is_some() && id(aside) == id(path) {
        let _ = std::fs::remove_file(aside);
    }
}

/// The incarnation layout is astream's.
pub use astream_broker::{INC_MAX, WILL_SEQ_LOW};

fn incarnation_of_seq(seq: u64) -> u64 {
    astream_broker::incarnation_of(seq).unwrap_or(u64::MAX)
}

/// How many incarnations one flushed write of `inc` covers.
pub const INC_LEASE: u64 = 64;
const _: () = assert!(INC_LEASE >= 1 && INC_LEASE <= INC_MAX);

/// The token a session's presence row carries this state dir's mark under ([`StateDir::mark`]).
pub(crate) const PRESENCE_MARK: &str = "statedir";

/// What `inc-lease` holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IncLease {
    /// The incarnation the last attach took.
    pub used: u64,
    /// The lease's top, flushed into `inc` before `used` was presented.
    pub top: u64,
    /// The boot that wrote it, as the kernel names it on every volume (`None` only where the
    /// kernel names none, or in a lease an older build wrote off the boot volume). It is trusted
    /// only in that boot and on the boot volume, whose page cache vouches for it
    /// ([`StateDir::trusted_boot`]); the attach's earlier-boot rules read it on any volume
    /// ([`StateDir::last_ran_in_an_earlier_boot`]).
    pub boot: Option<[u8; 16]>,
    /// The `inc` the top was flushed into, `None` where none could be named
    /// ([`StateDir::inc_file`]): a lease that names none is never trusted.
    pub file: Option<IncFile>,
}

/// Which `inc` a lease vouches for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IncFile {
    /// The device both live on.
    pub dev: u64,
    /// The state dir's inode.
    pub dir: u64,
    /// `inc`'s inode.
    pub ino: u64,
}

impl IncLease {
    /// The one line it is stored as.
    pub(crate) fn render(&self) -> String {
        let boot = self
            .boot
            .map_or_else(|| "-".to_string(), |b| aterm_codec::hex::encode_lower(&b));
        let file = self.file.map_or_else(
            || "-".to_string(),
            |f| format!("{}:{}:{}", f.dev, f.dir, f.ino),
        );
        format!(
            "used={} top={} boot={boot} file={file}",
            self.used, self.top
        )
    }

    /// Parse one back: all four fields once each, `used` at or below `top`, a boot that is `-` or a boot id, and a file that is `-` or `<dev>:<dir>:<ino>`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (mut used, mut top, mut boot, mut file) = (None, None, None, None);
        let dec = |v: &str| {
            (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .then(|| v.parse::<u64>().ok())
                .flatten()
        };
        for tok in text.split_whitespace() {
            match tok.split_once('=')? {
                ("used", v) if used.is_none() => used = Some(dec(v)?),
                ("top", v) if top.is_none() => top = Some(dec(v)?),
                ("boot", "-") if boot.is_none() => boot = Some(None),
                ("boot", v) if boot.is_none() => {
                    boot = Some(Some(astream_sys::parse_boot_id(v.as_bytes())?));
                }
                ("file", "-") if file.is_none() => file = Some(None),
                ("file", v) if file.is_none() => {
                    let mut ids = v.split(':').map(dec);
                    let (dev, dir, ino) = (ids.next()??, ids.next()??, ids.next()??);
                    if ids.next().is_some() {
                        return None;
                    }
                    file = Some(Some(IncFile { dev, dir, ino }));
                }
                _ => return None,
            }
        }
        let (used, top, boot, file) = (used?, top?, boot?, file?);
        (used <= top).then_some(Self {
            used,
            top,
            boot,
            file,
        })
    }
}

/// What one attach does about its incarnation ([`plan_incarnation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IncPlan {
    /// The incarnation it takes.
    pub next: u64,
    /// The lease top `inc-lease` will name, flushed into `inc` first when `flush`.
    pub top: u64,
    /// Whether `inc` is flushed before `next` may be presented.
    pub flush: bool,
}

/// One past both the state dir's and the bus's incarnation, or `None` past [`INC_MAX`].
pub(crate) fn next_incarnation(local: u64, bus: u64) -> Option<u64> {
    local.max(bus).checked_add(1).filter(|inc| *inc <= INC_MAX)
}

/// The attach's decision, from what the disk and the bus say.
pub(crate) fn plan_incarnation(
    top: u64,
    file: Option<IncFile>,
    lease: Option<IncLease>,
    seq_inc: u64,
    boot: Option<[u8; 16]>,
    bus: u64,
    span: u64,
) -> Option<IncPlan> {
    let trusted = lease.filter(|l| {
        boot.is_some() && l.boot == boot && l.top == top && file.is_some() && l.file == file
    });
    let taken = trusted.map_or(top, |l| l.used).max(seq_inc);
    let next = next_incarnation(taken, bus)?;
    if trusted.is_some() && next <= top {
        return Some(IncPlan {
            next,
            top,
            flush: false,
        });
    }
    let span = if boot.is_some() { span.max(1) } else { 1 };
    Some(IncPlan {
        next,
        top: next.saturating_add(span - 1).min(INC_MAX),
        flush: true,
    })
}

/// Whether `dir`, opened following links, is on the boot volume: astream's
/// [`astream_broker::on_boot_volume`] of it, which compares the opened dir's `st_dev` with `/`'s
/// (on Apple, `/System/Volumes/Data`'s too). This crate builds only on unix (its root is
/// `cfg(unix)`), so on every OS the guard compiles on that answer is a device comparison: `/`
/// answers `true` on each by construction, and `/dev` (devfs on macOS, devtmpfs on Linux) `false`.
/// A dir that cannot be opened is off it, the safe direction: no lease is trusted there.
fn on_boot_volume(dir: &Path) -> bool {
    open_dir_nonblock(dir).is_ok_and(|opened| astream_broker::on_boot_volume(&opened))
}

/// The points of [`StateDir::take_incarnation`] a test can kill it at (`IncSeam::kill_at`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IncStep {
    /// `inc`'s flushed write has renamed its temp over `inc` and not yet flushed the directory.
    Renamed,
    /// `inc`'s flushed write returned.
    Flushed,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct IncSeam {
    /// This boot, as the walk stands it in; the volume probe then opens `/`, on the boot volume
    /// by construction, unless `off_volume` ([`StateDir::volume_probe`]).
    pub boot: Option<Option<[u8; 16]>>,
    /// The dir stands off the boot volume: the volume probe opens `/dev`, a device filesystem
    /// that is never the boot volume, so the shipped probe answers it ([`StateDir::volume_probe`]).
    pub off_volume: bool,
    /// The step the next take dies at.
    pub kill_at: Option<IncStep>,
    /// One defect, the walk's negative controls (the model's `Buggy`): 1 trusts a lease whatever
    /// boot it names, 2 whatever top it names, 3 writes the lease before `inc` is flushed, and 5
    /// trusts by the boot the kernel names, off the boot volume too.
    pub mutant: u8,
    /// The lease a flush covers, short enough for the walk to run one out (the model's `Span`).
    pub span: Option<u64>,
}

/// The post journal in one open ([`journal`]): what it holds, read at the first use, and where
/// its file stands.
#[derive(Debug, Default)]
struct Posts {
    /// What the journal holds; `None` until a read of it succeeds.
    index: Option<journal::Index>,
    /// Whether an older build's `sent/` and `inflight/` files were taken into it
    /// ([`StateDir::adopt_legacy_posts`]).
    adopted: bool,
    /// Whether the journal was judged against the dir's incarnation at its first read
    /// ([`StateDir::judge_posts`]).
    judged: bool,
    /// Whether an attach the journal does not name went past it: its records in flight are retired
    /// at the next use of a writing open ([`StateDir::posts`]).
    stale: bool,
    /// The index names an attach its file does not yet: the next record is written after it
    /// ([`StateDir::name_attach_in_posts`]).
    attach_unwritten: bool,
    /// An older build's `sent/<sid>.<id>` that did not read, by name: kept where it is and not
    /// taken in, so that one post's reservation is refused, naming the file, as that build
    /// refused it, and no other post's is.
    held_sent: std::collections::BTreeSet<String>,
    /// An older build's `inflight/<seq>` that did not read: kept where it is, and every read of
    /// the records in flight refused naming it, as that build refused them.
    held_in_flight: Option<String>,
    /// The journal, held open for appends until a rewrite replaces the file.
    file: Option<std::fs::File>,
    /// Whether the file exists.
    exists: bool,
    /// Where its whole records end: where the next one goes.
    len: u64,
    /// The step the next record carries.
    next_step: u64,
    /// The file may hold bytes past `len` (a torn tail, an append that failed): it is rewritten
    /// from the index before the next append.
    dirty: bool,
    /// Its directory entry is on the media as far as this open knows.
    entry_on_media: bool,
    /// The size from which the next append rewrites it first ([`journal::COMPACT_AT`]).
    compact_at: u64,
    /// What a stop's hand-off kept beside the files ([`journal::HANDED`]), where the journal's
    /// judgement found no attach went past it since: the heads the adoption gives those files'
    /// records ([`StateDir::adopt_legacy_posts`]).
    handed: Option<journal::Index>,
}

impl Posts {
    fn index(&self) -> &journal::Index {
        static EMPTY: journal::Index = journal::Index::EMPTY;
        self.index.as_ref().unwrap_or(&EMPTY)
    }

    /// `pin`, an older build's file's reservation of post `id` of `sid`, with the head the
    /// hand-off kept for that very reservation, if it kept one.
    fn handed_pin(&self, sid: &str, id: u64, pin: PublishPin) -> PublishPin {
        match self.handed.as_ref().and_then(|handed| handed.pin(sid, id)) {
            Some(kept) if kept.without_head() == pin => kept.clone(),
            _ => pin,
        }
    }

    /// `entry`, an older build's file's record in flight, with the heads the hand-off kept for
    /// that very record, if it kept them.
    fn handed_entry(&self, entry: InFlight) -> InFlight {
        let kept = self
            .handed
            .as_ref()
            .and_then(|handed| handed.in_flight.get(&entry.pin.seq));
        match kept {
            Some(kept) if kept.without_head() == entry => kept.clone(),
            _ => entry,
        }
    }
}

/// The asks journal in one open ([`asks`]): what it holds, read at the first use, what is
/// owed to it, and where its file stands.
#[derive(Debug, Default)]
struct Asks {
    /// What the journal holds, the tables of a build before it taken in, and every fact noted
    /// since, written or not; `None` until the first use reads it ([`StateDir::asks`]).
    index: Option<asks::Index>,
    /// Facts noted ([`StateDir::note_asks`]) that no write has taken yet: the next
    /// ([`StateDir::write_asks`]) appends them in one write.
    owed: Vec<asks::Frame>,
    /// The journal, held open for appends until a rewrite replaces the file.
    file: Option<std::fs::File>,
    /// Where its whole records end: where the next one goes.
    len: u64,
    /// The step the next record carries.
    next_step: u64,
    /// The file may not hold what the index does (a write refused, a cut or torn tail, damage,
    /// a table taken in): it is rewritten from the index at the next write. No file yet is not
    /// this: the first write appends, and makes it.
    dirty: bool,
    /// The size from which the next write rewrites it ([`journal::COMPACT_AT`]).
    compact_at: u64,
}

impl Asks {
    fn index(&self) -> &asks::Index {
        static EMPTY: asks::Index = asks::Index::EMPTY;
        self.index.as_ref().unwrap_or(&EMPTY)
    }
}

/// What a read of the asks journal found ([`StateDir::read_asks`]).
#[derive(Debug, Default)]
struct AsksRead {
    /// What it holds, reconciled with the tables of a build before it ([`asks::Index::take_in`]).
    index: asks::Index,
    /// Where its intact run of records ends, its whole length where it reads whole; 0 where it
    /// holds damage, or does not read at all.
    len: u64,
    /// The step its next record carries.
    next_step: u64,
    /// The file does not hold all of `index`, or holds more than its whole records: the next
    /// write rewrites it.
    dirty: bool,
    /// Its exact bytes, where it does not read whole.
    damaged: Option<Vec<u8>>,
}

/// Whether `now` is ANOTHER LOG than `was`, every record on it landed after anything read on
/// `was`: both named, under different ids, and told with a lineage, so by a broker that mints a
/// fresh id only for a log that holds no record (`logid::settle`). The same id that continues
/// nothing (a restored or shortened copy, a move older than the lineage's window) or a log told
/// without lineage (an older broker) may still hold earlier records, and is not.
pub(crate) fn another_log(
    was: Option<astream_broker::LogIdentity>,
    now: Option<astream_broker::LogIdentity>,
    lineage: Option<&astream_broker::Lineage>,
) -> bool {
    lineage.is_some() && matches!((was, now), (Some(was), Some(now)) if was.id != now.id)
}

/// The reservations a moved generation resets ([`StateDir::publication_offset_resets`]).
#[derive(Debug)]
struct OffsetResets {
    /// The `keys/<sid>` files to rewrite whole: path and text.
    keys: Vec<(String, String)>,
    /// The journal's reservations to record again: sid, post id and the pin without its offset.
    posts: Vec<(String, u64, PublishPin)>,
}

/// The bridge's state directory.
pub struct StateDir {
    root: PathBuf,
    disk: crate::fx::Disk,
    /// The post journal ([`journal`]).
    posts: std::sync::Mutex<Posts>,
    /// The asks journal ([`asks`]).
    asks: std::sync::Mutex<Asks>,
    /// The `acked` log, held open for appends from the first ([`StateDir::append_self_acked`])
    /// until a replace renames another file over its name ([`StateDir::set_self_acked`]) or an
    /// append's write fails (refused, made and failed, or cut short): the next append opens the
    /// file the name holds then.
    acked: std::sync::Mutex<Option<std::fs::File>>,
    /// Whether a read of the journal takes an older build's files into it: an open that may
    /// write, not a peek.
    adopts_legacy: bool,
    unflushed: std::sync::Mutex<std::collections::BTreeSet<String>>,
    logpin_fence: std::sync::Mutex<Option<std::fs::File>>,
    /// The pin [`StateDir::order_logpin`] wrote by barriers, held open, while its broker's settle
    /// is owed (critic item F07): [`StateDir::logpin_settled`] makes it the fence. Locked after
    /// `logpin_fence`, never before.
    logpin_ordered: std::sync::Mutex<Option<std::fs::File>>,
    run_fence: std::sync::Mutex<Option<std::fs::File>>,
    undurable: std::sync::Mutex<std::collections::BTreeSet<String>>,
    floor_admitted: std::sync::atomic::AtomicBool,
    /// The quarantine copy this open made of each source and has yet to see the source gone, by
    /// [`StateDir::quarantine_try`]: what a recovery tried again reuses.
    quarantine_tried: std::sync::Mutex<BTreeMap<String, QuarantineCopy>>,
    /// The records in flight this open retired into the quarantine, by sequence, from an older
    /// build's file or from the journal: one record, one copy
    /// ([`StateDir::retire_in_flight_of_an_earlier_boot`]).
    retired_in_flight: std::sync::Mutex<BTreeMap<u64, Vec<u8>>>,
    /// What a stale journal's retirement moved, for the bridge to tell
    /// ([`StateDir::take_stale_retired`]).
    stale_retired: std::sync::Mutex<Vec<SetAside>>,
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    times: crate::wait_tally::Kept,
    #[cfg(test)]
    ops: std::sync::Mutex<FileOps>,
    #[cfg(test)]
    inc_seam: std::sync::Mutex<IncSeam>,
    #[cfg(test)]
    refused: std::sync::Mutex<Option<String>>,
    #[cfg(test)]
    dir_flush_refusal: std::sync::Mutex<Option<(PathBuf, aterm_durable::Flush, io::ErrorKind)>>,
    #[cfg(test)]
    copy_on_every_try: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    one_memory_per_source: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    keep_refused_drops: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    receipt_heads_off: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    hand_off_without_heads: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    stop_leaves_the_heads_unflushed: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    adopt_after_attach: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    adopt_without_the_attach: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    keep_earlier_boot_reservations: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    skip_publication_resets: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    pin_head_gone_on_another_log: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    lease_names_no_boot_off_the_boot_volume: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    keep_acked_across_its_replace: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    proof_rule: std::sync::Mutex<ProofRule>,
}

/// The filesystem operations one [`StateDir`] made, by kind (counted only in a test build,
/// `StateDir::ops`), so a test states what a post costs as a count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FileOps {
    /// Reservation reads ([`StateDir::read_reservation`]) and reads of the post journal
    /// ([`StateDir::read_regular`]), hits and misses alike.
    pub reads: u64,
    /// Atomic replaces: a fresh temp written, then renamed over the target.
    pub replaces: u64,
    /// Renames, the replaces' own included.
    pub renames: u64,
    /// `fsync`s (`F_FULLFSYNC` on macOS).
    pub fsyncs: u64,
    /// Barriers (`F_BARRIERFSYNC` on macOS, `aterm_durable::ordered`).
    pub barriers: u64,
    /// Unlinks, of any file of the dir.
    pub unlinks: u64,
    /// Appends to an append-only file, by write: the one that carries an attach's name into the
    /// post journal with its record counts once.
    pub appends: u64,
    /// Opens of a file the dir holds open for its appends (the post journal, the asks journal,
    /// `acked`): one a descriptor taken, none for an append to a descriptor held.
    pub opens: u64,
}

#[cfg(test)]
impl std::ops::Sub for FileOps {
    type Output = Self;

    fn sub(self, before: Self) -> Self {
        Self {
            reads: self.reads - before.reads,
            replaces: self.replaces - before.replaces,
            renames: self.renames - before.renames,
            fsyncs: self.fsyncs - before.fsyncs,
            barriers: self.barriers - before.barriers,
            unlinks: self.unlinks - before.unlinks,
            appends: self.appends - before.appends,
            opens: self.opens - before.opens,
        }
    }
}

/// The original publication's face and semantic digest, persisted before send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishIdentity {
    pub subject: String,
    pub digest: String,
}

/// A sequence reservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishPin {
    pub seq: u64,
    pub via_key: bool,
    pub identity: Option<PublishIdentity>,
    /// A key's previous winner.
    pub original: Option<PublishIdentity>,
    /// The key winner's landed offset on the witnessed log, distinct from a later attempt's offset.
    pub original_off: Option<u64>,
    /// The original post within this session's key file, before any publish.
    pub owner: Option<u64>,
    /// The log's head as the bridge knew it when it made the pin: any attempt's record lands
    /// at or above it, where a retry reads it back from before it publishes again
    /// (`Bridge::landed_at_its_pin`; none for an older build's pin, which is not read back).
    /// NEVER IN THE TEXT FORM ([`PublishPin::render`]), which every build parses strictly: a
    /// journal record carries it after its body ([`journal`]'s tail, which a reader of an
    /// earlier build skips), and a stop's hand-off beside the files
    /// ([`StateDir::hand_posts_to_files`]); a receipt's is kept in a file beside its pin's,
    /// with its pin's sequence and the log it is an offset of ([`ACK_HEADS`]), and a key's pin
    /// file holds none.
    pub at: Option<u64>,
}

impl PublishPin {
    pub(crate) fn parse(text: &str) -> io::Result<Self> {
        let bad = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid publication reservation",
            )
        };
        let dec = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
        };
        let mut words = text.split_whitespace();
        let seq = words.next().and_then(dec).ok_or_else(bad)?;
        let mut tail: Vec<&str> = words.collect();
        let via_key = tail.first() == Some(&"key");
        if via_key {
            tail.remove(0);
        }
        let (mut identity, mut original, mut original_off, mut owner) = (None, None, None, None);
        let mut parts = tail.as_slice();
        while !parts.is_empty() {
            if parts[0] == "owner" && owner.is_none() {
                owner = Some(
                    parts
                        .get(1)
                        .copied()
                        .and_then(dec)
                        .filter(|id| *id > 0)
                        .ok_or_else(bad)?,
                );
                parts = &parts[2..];
                continue;
            }
            if parts[0] == "original-off" && original_off.is_none() {
                original_off = Some(parts.get(1).copied().and_then(dec).ok_or_else(bad)?);
                parts = &parts[2..];
                continue;
            }
            if parts.len() < 3 {
                return Err(bad());
            }
            let (label, subject, digest) = (parts[0], parts[1], parts[2]);
            if !is_face(subject) || !aterm_codec::hex::is_lower(digest, 64) {
                return Err(bad());
            }
            let slot = match label {
                "identity" if identity.is_none() => &mut identity,
                "original" if original.is_none() && via_key => &mut original,
                _ => return Err(bad()),
            };
            *slot = Some(PublishIdentity {
                subject: subject.to_string(),
                digest: digest.to_string(),
            });
            parts = &parts[3..];
        }
        if (original.is_some() || original_off.is_some()) && identity.is_none() {
            return Err(bad());
        }
        Ok(Self {
            seq,
            via_key,
            identity,
            original,
            original_off,
            owner,
            at: None,
        })
    }

    /// The text form every build reads: never the head [`PublishPin::at`], which an earlier
    /// build's strict parse refuses.
    fn render(&self) -> String {
        let mut line = self.seq.to_string();
        if self.via_key {
            line.push_str(" key");
        }
        if let Some(identity) = &self.identity {
            line.push_str(&format!(
                " identity {} {}",
                identity.subject, identity.digest
            ));
        }
        if let Some(original) = &self.original {
            line.push_str(&format!(
                " original {} {}",
                original.subject, original.digest
            ));
        }
        if let Some(off) = self.original_off {
            line.push_str(&format!(" original-off {off}"));
        }
        if let Some(owner) = self.owner {
            line.push_str(&format!(" owner {owner}"));
        }
        line
    }
}

/// `inbox-owed`'s lines, one `<node> <offset>` each, in node order; a line naming no node is no
/// one's ([`StateDir::inbox_owed`]).
fn parse_owed(text: &str) -> Vec<(String, u64)> {
    let owed: BTreeMap<String, u64> = text
        .lines()
        .filter_map(|line| {
            let (node, off) = line.trim().split_once(' ')?;
            Some((node.to_owned(), crate::parse_uint(off)?))
                .filter(|(node, _)| crate::subject::is_principal(node))
        })
        .collect();
    owed.into_iter().collect()
}

/// `node`'s handed-over inbox commit among `owed`.
fn owed_of(owed: &[(String, u64)], node: &str) -> Option<u64> {
    owed.iter().find(|(of, _)| of == node).map(|(_, off)| *off)
}

fn key_line(line: &str) -> io::Result<(&str, PublishPin)> {
    let (key, reservation) = line
        .split_once(char::is_whitespace)
        .filter(|(key, _)| !key.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid publication key"))?;
    Ok((key, PublishPin::parse(reservation)?))
}

fn parse_topic_floors(path: &Path, body: &str) -> io::Result<BTreeMap<String, u64>> {
    if body.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty broadcast refill floor file",
        ));
    }
    let mut out = BTreeMap::new();
    for line in body.lines() {
        let mut words = line.split_whitespace();
        let topic = words.next().filter(|t| crate::subject::is_topic(t));
        let off = words
            .next()
            .filter(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|w| w.parse::<u64>().ok());
        match (topic, off, words.next()) {
            (Some(topic), Some(off), None) => {
                if out.insert(topic.to_string(), off).is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "duplicate broadcast refill topic",
                    ));
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{}: `{}` is not a broadcast refill floor",
                        path.display(),
                        safe_line(line)
                    ),
                ));
            }
        }
    }
    Ok(out)
}

fn is_face(subject: &str) -> bool {
    subject.starts_with('/')
        && subject
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'*' | b'>'))
}

/// What the node still owes a publication ([`StateDir::owed_publications`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Owed {
    /// Sessions with a post or a receipt reserved.
    pub sids: BTreeSet<String>,
    /// Subjects a reservation or an in-flight entry names.
    pub subjects: BTreeSet<String>,
}

/// What [`StateDir::settle_posts`] writes for one landed post.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PostSettle<'a> {
    /// The sequence of its in-flight entry to end ([`journal::Frame::Landed`]).
    pub(crate) landed: Option<u64>,
    /// Its reservation to forget, by `(sid, post id)` ([`journal::Frame::Retire`]).
    pub(crate) retired: Option<(&'a str, u64)>,
}

/// One record in flight onto this node's own lanes ([`StateDir::in_flight`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InFlight {
    pub pin: PublishPin,
    pub subject: String,
    pub body: Vec<u8>,
    /// The log this body was prepared on, the only one where a `re=` in it points.
    pub log: Option<astream_broker::LogIdentity>,
    /// The log's head as the bridge knew it when it journaled the entry, on `log`: every
    /// attempt's record lands at or above it. Where a replay reads its record back from
    /// before it publishes again (`Bridge::landed_since`), since astream's dedup reaches
    /// only the records at or above the base retention leaves. Kept as [`PublishPin::at`] is:
    /// never in the text form.
    pub at: Option<u64>,
}

impl PublishPin {
    /// This pin as its text form holds it: without its head ([`PublishPin::at`]).
    pub(crate) fn without_head(&self) -> Self {
        Self {
            at: None,
            ..self.clone()
        }
    }
}

impl InFlight {
    /// This entry as its text form holds it: without its head or its pin's.
    pub(crate) fn without_head(&self) -> Self {
        Self {
            pin: self.pin.without_head(),
            at: None,
            ..self.clone()
        }
    }
}

impl InFlight {
    /// The three-line form, or the four with `log=`: what `inflight/<seq>` holds, and a journal
    /// record's body after its head. Never the head [`InFlight::at`] (nor its pin's), which an
    /// earlier build's strict parse refuses.
    pub(crate) fn render(&self) -> String {
        let mut text = format!(
            "{}\n{}\n{}",
            self.pin.render(),
            self.subject,
            aterm_codec::hex::encode_lower(&self.body)
        );
        if let Some(log) = self.log {
            text.push_str(&format!("\nlog={log}"));
        }
        text
    }

    /// Parse the three-line form, or the four with `log=`.
    pub(crate) fn parse(seq: u64, text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let pin = PublishPin::parse(lines.next()?).ok()?;
        let subject = lines.next()?;
        let body = aterm_codec::hex::decode_vec(lines.next()?.as_bytes())?;
        let log = match lines.next() {
            Some(line) => Some(astream_broker::LogIdentity::parse(
                line.strip_prefix("log=")?,
            )?),
            None => None,
        };
        (lines.next().is_none() && pin.seq == seq && is_face(subject)).then(|| Self {
            pin,
            subject: subject.to_string(),
            body,
            log,
            at: None,
        })
    }
}

/// The acked self-lane log as read back ([`StateDir::self_acked_log`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AckedLog {
    /// Every offset the file holds whole.
    pub offsets: std::collections::BTreeSet<u64>,
    /// The forged-self window's floor the file names, its `floor=<n>` entry (critic item F17's
    /// review): the lowest offset the window accuses, kept apart from the offsets it holds.
    /// Every file this build writes names one (each replace, and the first append into a file
    /// that names none); `None` for no file, one that cannot be read, or one an older build
    /// wrote, whose reader took the oldest offset it holds for the floor. A window with neither
    /// a floor nor an offset is floored at the head its next attach reads, and written so
    /// (critic item F20, `Bridge::floor_at_the_head`).
    pub floor: Option<u64>,
    /// How many offset entries the file holds, repeats included.
    pub entries: usize,
    /// Whether the file ends at a terminator (or is absent), so an append may go on after its last byte.
    pub appendable: bool,
}

/// The prefix of the `acked` log's floor entry ([`AckedLog::floor`]): no offset, so a build
/// before it reads past it, as it reads past every piece that is not one.
const ACKED_FLOOR: &str = "floor=";

impl AckedLog {
    fn parse(text: &str) -> Self {
        let mut log = Self {
            appendable: text.is_empty() || text.ends_with(','),
            ..Self::default()
        };
        let mut pieces: Vec<&str> = text.split(',').collect();
        let tail = pieces.pop().unwrap_or_default();
        for piece in pieces {
            if let Some(floor) = piece.trim().strip_prefix(ACKED_FLOOR) {
                if let Some(floor) = crate::parse_uint::<u64>(floor) {
                    log.floor = Some(log.floor.map_or(floor, |had| had.max(floor)));
                }
                continue;
            }
            log.entries += 1;
            if let Some(off) = crate::parse_uint(piece.trim()) {
                log.offsets.insert(off);
            }
        }
        if tail.ends_with('\n') {
            if let Some(off) = crate::parse_uint(tail.trim()) {
                log.entries += 1;
                log.offsets.insert(off);
            }
        }
        log
    }

    /// `offsets`, each ended by its comma, after the floor entry where `floor` names one.
    fn render(floor: Option<u64>, offsets: impl Iterator<Item = u64>) -> String {
        let mut text = String::new();
        if let Some(floor) = floor {
            text.push_str(ACKED_FLOOR);
            text.push_str(&floor.to_string());
            text.push(',');
        }
        for off in offsets {
            text.push_str(&off.to_string());
            text.push(',');
        }
        text
    }
}

/// A kind of file a power cut may tear and the next boot may set aside ([`StateDir::set_aside_torn`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TornKind {
    /// `inflight/<seq>`: an older build's record in flight, which this build retires or set
    /// aside as before and then takes into the journal ([`StateDir::in_flight`]).
    InFlight,
    /// `sent/<sid>.<id>`: an older build's post reservation, likewise ([`StateDir::post_pin`]).
    Sent,
    /// `acks/<sid>.<rid>` ([`StateDir::receipt_pin`]).
    Acks,
    /// `keys/<sid>` ([`StateDir::key_pin`]).
    Keys,
    /// `unhandled/<sid>` ([`StateDir::topic_floors`]).
    Unhandled,
    /// `journal/posts`, the post journal ([`journal`]), and `journal/asks`, the asks journal
    /// ([`asks`]): the sweep reads each by its records ([`StateDir::sweep_journal`]), never as
    /// text.
    Journal,
}

impl TornKind {
    /// Every kind, in the order a sweep visits them.
    pub(crate) const ALL: [Self; 6] = [
        Self::InFlight,
        Self::Sent,
        Self::Acks,
        Self::Keys,
        Self::Unhandled,
        Self::Journal,
    ];

    const fn place(self) -> &'static str {
        match self {
            Self::InFlight => "inflight",
            Self::Sent => "sent",
            Self::Acks => "acks",
            Self::Keys => "keys",
            Self::Unhandled => "unhandled",
            Self::Journal => journal::DIR,
        }
    }

    fn of(rel: &str) -> Option<Self> {
        let (place, name) = rel.split_once('/')?;
        Self::ALL
            .into_iter()
            .find(|kind| kind.place() == place && kind.names(name))
    }

    fn whole(self, rel: &Path, name: &str, text: &str) -> bool {
        text.ends_with('\n')
            && match self {
                Self::InFlight => Self::entry(name, text).is_some_and(|entry| entry.log.is_some()),
                _ => self.reads(rel, name, text),
            }
    }

    fn reads(self, rel: &Path, name: &str, text: &str) -> bool {
        match self {
            Self::InFlight => Self::entry(name, text).is_some(),
            Self::Sent | Self::Acks => PublishPin::parse(text).is_ok(),
            Self::Keys => text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .all(|line| key_line(line).is_ok()),
            Self::Unhandled => parse_topic_floors(rel, text).is_ok(),
            Self::Journal => matches!(
                if name == asks::NAME {
                    asks::read(text.as_bytes()).end
                } else {
                    journal::read(text.as_bytes()).end
                },
                journal::End::Clean | journal::End::Cut
            ),
        }
    }

    fn entry(name: &str, text: &str) -> Option<InFlight> {
        crate::parse_uint(name).and_then(|seq| InFlight::parse(seq, text))
    }

    fn names(self, name: &str) -> bool {
        !name.is_empty()
            && !name.contains('/')
            && !name.starts_with('.')
            && (self != Self::InFlight || name.bytes().all(|b| b.is_ascii_digit()))
            && (self != Self::Journal || name == journal::NAME || name == asks::NAME)
    }
}

/// Where the dir keeps files that serve one session each, by its sid (`<place>/<sid>`, and
/// `acks/<sid>.<rid>` and `ack-heads/<sid>.<rid>`), with the [`TornKind`] a torn one is set
/// aside as: [`StateDir::sweep_sessions_of_an_earlier_boot`] unlinks them.
const SESSION_PLACES: [(&str, Option<TornKind>); 6] = [
    ("keys", Some(TornKind::Keys)),
    ("acks", Some(TornKind::Acks)),
    (ACK_HEADS, None),
    ("unhandled", Some(TornKind::Unhandled)),
    ("seen", None),
    ("topics", None),
];

/// What a run record's write flushed ([`StateDir::begin_run`], [`StateDir::end_run_clean`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Flushed {
    /// Files of a [`TornKind`], the mirror's pin and the heads a hand-off kept
    /// ([`journal::HANDED`]), flushed where they are.
    pub files: u64,
    /// Directories whose entries moved, flushed before the record (the root's full flush for the record not counted).
    pub dirs: u64,
    /// Barriers: a file's, a directory's, the record's bytes'.
    pub ordered: u64,
    /// Full flushes: a file's or a directory's on another device than the root, the record's.
    pub full: u64,
    /// A start that wrote nothing: the record was the OPEN it would write, and its proof showed
    /// it on the media ([`RUN_PROOF`]).
    pub proven: bool,
}

impl Flushed {
    const fn note(&mut self, flush: Option<aterm_durable::Flush>, full: u64) {
        match flush {
            Some(aterm_durable::Flush::Ordered) => self.ordered += 1,
            Some(aterm_durable::Flush::Durable) => self.full += full,
            None => {}
        }
    }
}

/// The rules [`StateDir::end_run_clean`] keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StopRules {
    /// Each file the run left unflushed is flushed where it is.
    pub flush_files: bool,
    /// Every directory whose entries moved is flushed before the record.
    pub flush_dirs: bool,
    /// The record is written after what it vouches for.
    pub record_last: bool,
}

impl StopRules {
    /// Every rule on: what ships.
    pub(crate) const ALL: Self = Self {
        flush_files: true,
        flush_dirs: true,
        record_last: true,
    };
}

/// The rules [`StateDir::begin_run`] keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StartRules {
    /// The record's temp is flushed before its rename.
    pub flush_record: bool,
    /// The root, whose entry the rename moved, is flushed after it by the start's tier, and must succeed before the admission is cached.
    pub flush_root: bool,
    /// A record of this boot is replaced in full, an open one too: its visible OPEN may be a failed replacement of this boot's durable CLEAN. An OPEN a proof stands for is not replaced at all ([`RUN_PROOF`]).
    pub this_boot_in_full: bool,
}

impl StartRules {
    /// Every rule on: what ships.
    pub(crate) const ALL: Self = Self {
        flush_record: true,
        flush_root: true,
        this_boot_in_full: true,
    };
}

/// How the last bridge of this dir stopped, and in which boot (`run`). CLEAN is written durably. OPEN is written by the barrier only over an earlier boot's record, with this boot named too and every existing torn kind's directory on the root's device, and in full everywhere else, this boot's record among them, but not at all where the record already reads as that OPEN and a proof stands for it ([`StateDir::begin_run`], [`RUN_PROOF`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LastRun {
    /// The boot it ran in, as the kernel names it.
    pub boot: Option<[u8; 16]>,
    /// Whether it stopped cleanly, every file of a [`TornKind`], the mirror's pin and the heads
    /// a hand-off kept on the media.
    pub clean: bool,
}

impl LastRun {
    fn render(&self) -> String {
        let boot = self
            .boot
            .map_or_else(|| "-".to_string(), |b| aterm_codec::hex::encode_lower(&b));
        let state = if self.clean { "clean" } else { "open" };
        format!("{state} boot={boot}")
    }

    fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let clean = match words.next()? {
            "open" => false,
            "clean" => true,
            _ => return None,
        };
        let boot = match words.next()?.strip_prefix("boot=")? {
            "-" => None,
            id => Some(astream_sys::parse_boot_id(id.as_bytes())?),
        };
        words.next().is_none().then_some(Self { boot, clean })
    }
}

/// One damaged file [`StateDir::set_aside_torn`] moved aside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetAside {
    pub from: String,
    pub to: String,
}

#[derive(Debug)]
struct QuarantineCopy {
    name: String,
    file: std::fs::File,
}

/// Where [`StateDir::set_aside_torn`] keeps what it moves aside, inside the dir.
pub const QUARANTINE: &str = "quarantine";

/// WHERE AN OWED RECEIPT'S PIN KEEPS ITS HEAD ([`PublishPin::at`], ATERM-KI-1111-7):
/// `ack-heads/<sid>.<rid>`, beside the pin file `acks/<sid>.<rid>`, whose text form every build
/// parses strictly and holds none: the pin's sequence, the log the head is an offset of, and
/// the head ([`parse_head`]), so a file a build before it left in place past a re-pin or a log
/// move is never read for another pin or on another log as it stands
/// ([`StateDir::receipt_head`]). A retry reads its attempt back from it
/// (`Bridge::landed_at_its_pin`), on another node's lane too, where no
/// in-flight entry names one.
pub(crate) const ACK_HEADS: &str = "ack-heads";

/// A head file's text ([`ACK_HEADS`]): `<seq> <log> <at>`, the pin's sequence, the log its head
/// is an offset of and the head, and the newline every write ends with, or `None` for anything
/// else (a torn write among it).
fn parse_head(text: &str) -> Option<(u64, astream_broker::LogIdentity, u64)> {
    let mut words = text.strip_suffix('\n')?.split(' ');
    let seq = crate::parse_uint(words.next()?)?;
    let log = astream_broker::LogIdentity::parse(words.next()?)?;
    let at = crate::parse_uint(words.next()?)?;
    words.next().is_none().then_some((seq, log, at))
}

/// The bus mirror's pin ([`StateDir::logpin`]).
pub(crate) const LOGPIN: &str = "logpin";
/// The bus mirror's alias key, its durable copy ([`StateDir::logkey`]).
pub(crate) const LOGKEY: &str = "logkey";
/// The floor of the bus mirror's lifetime total ([`StateDir::logfloor`]).
pub(crate) const LOGFLOOR: &str = "logfloor";
/// The stem of the run record's proof ([`StateDir::proof_path`]), made only once the file `run`
/// names is on the media ([`StateDir::begin_run`], [`StateDir::take_incarnation`]).
pub(crate) const RUN_PROOF: &str = "run.durable";
/// The stem of the floor's proof ([`StateDir::proof_path`]), made only once a durable publication
/// of the file `logfloor` names returned ([`StateDir::raise_logfloor`]).
pub(crate) const LOGFLOOR_PROOF: &str = "logfloor.durable";

/// The stem of the proof kept beside `name` that its file is on the media, for the two files a
/// later process of the same boot admits by one: `run` and `logfloor`.
fn proof_of(name: &str) -> Option<&'static str> {
    match name {
        "run" => Some(RUN_PROOF),
        LOGFLOOR => Some(LOGFLOOR_PROOF),
        _ => None,
    }
}

/// A regular file as it stands: its device, its inode and the time of its last change
/// (`st_ctime`), which every later tick's write into it and change of its metadata moves and no
/// copying tool sets back: `cp -a`, `rsync` and `touch -r` restore a modification time and move
/// this one. A false mismatch (a `chmod`, a rename) costs a full write; only a write in the tick
/// of the volume's clock that the time already shows goes unseen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    changed: (i64, i64),
}

impl Identity {
    fn of(meta: &std::fs::Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        meta.is_file().then(|| Self {
            dev: meta.dev(),
            ino: meta.ino(),
            changed: (meta.ctime(), meta.ctime_nsec()),
        })
    }
}

/// The identity of the regular file at `path`, never followed through a link.
fn identity_at(path: &Path) -> Option<Identity> {
    Identity::of(&std::fs::symlink_metadata(path).ok()?)
}

/// The identity of an open regular file.
fn identity_of(file: &std::fs::File) -> Option<Identity> {
    Identity::of(&file.metadata().ok()?)
}

/// How a proof is made and read ([`StateDir::prove`], [`StateDir::proven`]): a test's controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ProofRule {
    /// What ships: an empty file named for the published file as it stands, made once a full flush
    /// that covers it returned, and read by its name.
    #[default]
    Ships,
    /// CONTROL: that proof made as soon as the rename is visible, whatever its flush then does.
    AtTheRename,
    /// CONTROL: a hard link under the bare stem whose presence vouches for whatever the name holds
    /// now.
    ByPresence,
    /// CONTROL: a hard link read by its inode, named by the stem alone whichever directory and
    /// inode it was made for (R6 as first merged), so a copy of the dir that keeps its hard links
    /// carries it along.
    AnyDirectory,
    /// CONTROL: a hard link read by its inode, named for the directory and not the inode, so a copy
    /// elsewhere proves nothing and a restore over the directory that made it still does.
    ThisDirectory,
    /// CONTROL: a hard link read by its inode, named for the directory and the inode and not the
    /// inode's state, so a new inode proves nothing and a restore written into the files a
    /// directory holds (`cp -R`, `cp -a`) keeps its proof over bytes no flush covered.
    ThisInode,
}

#[cfg(test)]
impl ProofRule {
    const fn links(self) -> bool {
        matches!(
            self,
            Self::ByPresence | Self::AnyDirectory | Self::ThisDirectory | Self::ThisInode
        )
    }
}

/// Wait until the clock of the volume `dir` is on past every change made there so far, for a test
/// whose next write must move a change time on a volume whose clock ticks coarsely ([`Identity`]):
/// a new directory's change time reads that clock. At once on APFS, whose clock reads nanoseconds.
/// A wait for something that must happen, so a one-minute hang detector.
#[cfg(test)]
pub(crate) fn past_the_clock_tick(dir: &Path) {
    use std::os::unix::fs::MetadataExt as _;
    let read_the_clock = || {
        let probe = aterm_tempfile::Builder::new()
            .prefix(".tick")
            .suffix(".noindex")
            .tempdir_in(dir)
            .expect("a probe of the volume's clock");
        let meta = std::fs::metadata(probe.path()).expect("the probe reads");
        (meta.ctime(), meta.ctime_nsec())
    };
    let (first, began) = (read_the_clock(), std::time::Instant::now());
    while read_the_clock() <= first {
        assert!(
            began.elapsed() < std::time::Duration::from_secs(60),
            "the clock of {} never moved on",
            dir.display()
        );
    }
}

/// The mode every file of the state directory is made with: the owner's alone, as its records
/// (`inflight/`, mode 0600) and its key (`logkey`) always were. A post's `key=` (`keys/<sid>`)
/// and a session's topic names (`topics/<sid>`) are caller text the session log keeps only in a
/// 0600 journal or as a fingerprint (DESIGN-session-log §10, Phase 6), so the bridge keeps them no
/// wider; the directory is 0700 besides (`aterm fabric on`'s root step).
pub(crate) const STATE_FILE_MODE: u32 = 0o600;

/// What [`StateDir::reconcile_logpin`] did with the mirror's pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reconciled {
    /// Nothing: no unclean stop in an earlier boot, no floor, a whole pin at or above the floor, or a torn one the copies cannot be shown to cover.
    Kept,
    /// A whole pin's total, below the floor, rewritten at it.
    Raised { from: u64, to: u64 },
    /// A torn pin moved into the quarantine, and a whole one written from the copies in its place.
    SetAside(SetAside),
    /// No pin: a whole one written from the copies.
    Restored,
}

/// The rules [`StateDir::reconcile_logpin`] applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PinRules {
    /// A torn pin is moved aside.
    pub set_aside_torn: bool,
    /// Where the pin cannot say, the total is the floor's.
    pub total_from_floor: bool,
    /// A torn pin is moved aside only where `logkey` holds a key.
    pub need_key: bool,
}

impl PinRules {
    /// Every rule on: what ships.
    pub(crate) const ALL: Self = Self {
        set_aside_torn: true,
        total_from_floor: true,
        need_key: true,
    };
}

/// What a whole in-flight entry retired after an unclean stop in an earlier boot ([`StateDir::retire_in_flight_of_an_earlier_boot`]) is named with in [`QUARANTINE`], before its path.
pub const RETIRED_PREFIX: &str = "retired.";

/// [`StateDir::lock_bridge`]'s hold, released by `LOCK_UN` before the close (`clippy.toml`'s flock release rule).
pub struct BridgeLock(std::fs::File);

impl Drop for BridgeLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The bridge lock's file on a descriptor of its own, not yet holding the lock
/// ([`StateDir::bridge_lock_wait`]).
pub(crate) struct BridgeLockWait(std::fs::File);

impl BridgeLockWait {
    /// Waits in the kernel's queue for the lock, with no timer: `flock(2)` without `LOCK_NB`
    /// returns once the holder lets go, by its `LOCK_UN` or by the close of the last
    /// descriptor on its open file description, which is how a crash, a `SIGKILL` and a clean
    /// exit all let go. An interrupted wait waits again. A file system that takes no lock
    /// answers as [`StateDir::lock_bridge`] does there: the take is the lock.
    ///
    /// # Errors
    /// `flock(2)`'s errno other than `EINTR` and an unsupported lock.
    pub(crate) fn wait(self) -> io::Result<BridgeLock> {
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: BridgeLock's Drop releases by LOCK_UN"
            )]
            let taken = self.0.lock();
            match taken {
                Ok(()) => return Ok(BridgeLock(self.0)),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::Unsupported => return Ok(BridgeLock(self.0)),
                Err(e) => return Err(e),
            }
        }
    }
}

impl StateDir {
    /// Open (creating) a state directory.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        Self::open_on(root, crate::fx::Disk::Os)
    }

    /// [`StateDir::open`], writing through `disk` (the bridge's door, [`crate::fx::Fx::disk`]).
    pub(crate) fn open_on(root: impl Into<PathBuf>, disk: crate::fx::Disk) -> io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("seen"))?;
        Ok(Self::at_on(root, disk, true))
    }

    fn at(root: PathBuf) -> Self {
        Self::at_on(root, crate::fx::Disk::Os, false)
    }

    fn at_on(root: PathBuf, disk: crate::fx::Disk, adopts_legacy: bool) -> Self {
        Self {
            root,
            disk,
            posts: std::sync::Mutex::default(),
            asks: std::sync::Mutex::default(),
            acked: std::sync::Mutex::default(),
            adopts_legacy,
            unflushed: std::sync::Mutex::default(),
            logpin_fence: std::sync::Mutex::default(),
            logpin_ordered: std::sync::Mutex::default(),
            run_fence: std::sync::Mutex::default(),
            undurable: std::sync::Mutex::default(),
            floor_admitted: std::sync::atomic::AtomicBool::new(false),
            quarantine_tried: std::sync::Mutex::default(),
            retired_in_flight: std::sync::Mutex::default(),
            stale_retired: std::sync::Mutex::default(),
            #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
            times: crate::wait_tally::Kept::default(),
            #[cfg(test)]
            ops: std::sync::Mutex::default(),
            #[cfg(test)]
            inc_seam: std::sync::Mutex::default(),
            #[cfg(test)]
            refused: std::sync::Mutex::default(),
            #[cfg(test)]
            dir_flush_refusal: std::sync::Mutex::default(),
            #[cfg(test)]
            copy_on_every_try: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            one_memory_per_source: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            keep_refused_drops: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            receipt_heads_off: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            hand_off_without_heads: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            stop_leaves_the_heads_unflushed: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            adopt_after_attach: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            adopt_without_the_attach: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            keep_earlier_boot_reservations: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            skip_publication_resets: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            pin_head_gone_on_another_log: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            lease_names_no_boot_off_the_boot_volume: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            keep_acked_across_its_replace: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            proof_rule: std::sync::Mutex::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn copy_on_every_try(&self, on: bool) {
        self.copy_on_every_try
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The negative control of a recovery that remembers one copy per source, whatever its
    /// prefix ([`crate::sim::world::Mutant::OneQuarantineMemoryPerSource`]).
    #[cfg(test)]
    pub(crate) fn one_quarantine_memory_per_source(&self, on: bool) {
        self.one_memory_per_source
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The negative control of a drop the disk refuses: the open keeps the record
    /// ([`crate::sim::world::Mutant::KeepARefusedDrop`]).
    #[cfg(test)]
    pub(crate) fn keep_refused_drops(&self, on: bool) {
        self.keep_refused_drops
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// No receipt's head is kept, written or unlinked ([`ACK_HEADS`]), as before
    /// ATERM-KI-1111-7 (`crate::sim::world::Mutant::ReceiptPinKeepsNoHead`).
    #[cfg(test)]
    pub(crate) fn receipt_heads_off(&self, on: bool) {
        self.receipt_heads_off
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether receipts keep their heads ([`StateDir::receipt_heads_off`]).
    fn keeps_receipt_heads(&self) -> bool {
        #[cfg(test)]
        let off = self
            .receipt_heads_off
            .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let off = false;
        !off
    }

    /// The negative control of a hand-off whose heads the disk refused: the journal goes all
    /// the same ([`crate::sim::world::Mutant::HandOffWithoutTheHeads`]).
    #[cfg(test)]
    pub(crate) fn hand_off_without_heads(&self, on: bool) {
        self.hand_off_without_heads
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The negative control of a clean stop that vouches for a hand-off's files and leaves the
    /// heads kept beside them unflushed
    /// ([`crate::sim::world::Mutant::StopLeavesTheHeadsUnflushed`]).
    #[cfg(test)]
    pub(crate) fn stop_leaves_the_heads_unflushed(&self, on: bool) {
        self.stop_leaves_the_heads_unflushed
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The negative control of an attach that takes its incarnation before it takes a
    /// hand-off's files into the journal ([`crate::sim::world::Mutant::AdoptAfterTheAttach`]).
    #[cfg(test)]
    pub(crate) fn adopt_after_attach(&self, on: bool) {
        self.adopt_after_attach
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The negative control of an adoption that names no attach where the hand-off it takes
    /// named one ([`crate::sim::world::Mutant::AdoptWithoutTheAttach`]).
    #[cfg(test)]
    pub(crate) fn adopt_without_the_attach(&self, on: bool) {
        self.adopt_without_the_attach
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The negative control of the sweep of an earlier boot's sessions that retires none of
    /// their post reservations ([`crate::sim::world::Mutant::KeepEarlierBootReservations`]).
    #[cfg(test)]
    pub(crate) fn keep_earlier_boot_reservations(&self, on: bool) {
        self.keep_earlier_boot_reservations
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The negative control of a drop of another log's offsets that leaves each key file and
    /// post pin naming the old log's offsets
    /// ([`crate::sim::world::Mutant::ForgetPinResetsOnAnotherLog`]).
    #[cfg(test)]
    pub(crate) fn skip_publication_resets(&self, on: bool) {
        self.skip_publication_resets
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The negative control of a pin's head on another log, which takes the head out as every
    /// build before ATERM-KI-1111-4 did, so the pin's retry reads nothing back
    /// ([`crate::sim::world::Mutant::PinHeadGoneOnAnotherLog`]).
    #[cfg(test)]
    pub(crate) fn pin_head_gone_on_another_log(&self, on: bool) {
        self.pin_head_gone_on_another_log
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The negative control of a lease that names its boot on every volume, and the stand-in for
    /// an older build's attach: the lease names [`StateDir::trusted_boot`], so off the boot
    /// volume no boot, as before item RZ2
    /// ([`crate::sim::world::Mutant::LeaseNamesNoBootOffTheBootVolume`]).
    #[cfg(test)]
    pub(crate) fn lease_names_no_boot_off_the_boot_volume(&self, on: bool) {
        self.lease_names_no_boot_off_the_boot_volume
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// The dir stands off the boot volume from now on, in whichever boot the seam stands in
    /// ([`IncSeam::off_volume`]).
    #[cfg(test)]
    pub(crate) fn stand_off_the_boot_volume(&self) {
        let seam = self.inc_seam();
        self.set_inc_seam(IncSeam {
            off_volume: true,
            ..seam
        });
    }

    /// The negative control of a replace of `acked` that keeps the descriptor its appends held
    /// ([`StateDir::set_self_acked`]): the next append writes into the file the rename unlinked.
    #[cfg(test)]
    pub(crate) fn keep_acked_across_its_replace(&self, on: bool) {
        self.keep_acked_across_its_replace
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// How this open makes and reads the proofs of `run` and `logfloor` ([`ProofRule`]).
    #[cfg(test)]
    pub(crate) fn set_proof_rule(&self, rule: ProofRule) {
        *self.proof_rule.lock().unwrap_or_else(|e| e.into_inner()) = rule;
    }

    #[cfg(test)]
    fn proof_rule(&self) -> ProofRule {
        *self.proof_rule.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where this directory keeps the proof of the file `made` at `name`: the stem ([`proof_of`]),
    /// then the device, the directory's inode, `made`'s inode and the time of `made`'s last change
    /// to the nanosecond. THE NAME SAYS WHICH STATE OF WHICH FILE OF WHICH DIRECTORY THE FLUSH
    /// COVERED, and that is all the proof holds: it is an empty file. A copy carries the name along
    /// and what it copied has another inode (`tar`, `ditto`, `rsync -a`, `rsync -aH`, `cp -cR`). A
    /// restore written into the file a name holds (`cp -R`, `cp -a`, `rsync --inplace`) keeps the
    /// inode and moves the change time, which `cp -a` cannot set back, its own bytes and
    /// modification time included. In a copy elsewhere and in a restore over the very directory
    /// that made it alike, the name carried names another file or another state. A directory moved
    /// on its volume is the same directory, its files where the media holds them, and keeps its
    /// proofs. `None` where the directory does not read.
    fn proof_path(&self, name: &str, made: Identity) -> Option<PathBuf> {
        use std::os::unix::fs::MetadataExt as _;
        let stem = proof_of(name)?;
        let dir = std::fs::metadata(&self.root).ok()?;
        let Identity { dev, ino, changed } = made;
        #[cfg(test)]
        match self.proof_rule() {
            ProofRule::AnyDirectory | ProofRule::ByPresence => return Some(self.path(stem)),
            ProofRule::ThisDirectory => {
                return Some(self.path(&format!("{stem}.{}.{}", dir.dev(), dir.ino())));
            }
            ProofRule::ThisInode => {
                return Some(self.path(&format!("{stem}.{dev}.{}.{ino}", dir.ino())));
            }
            ProofRule::Ships | ProofRule::AtTheRename => {}
        }
        let (secs, nanos) = changed;
        Some(self.path(&format!("{stem}.{dev}.{}.{ino}.{secs}.{nanos}", dir.ino())))
    }

    /// The proof of the file `name` holds now as it stands, where the proof is kept or would be.
    #[cfg(test)]
    pub(crate) fn proof_of_what_reads(&self, name: &str) -> Option<PathBuf> {
        identity_at(&self.path(name)).and_then(|made| self.proof_path(name, made))
    }

    /// Make `name`'s proof ([`StateDir::proof_path`]) of `made`: every earlier proof of it unlinked
    /// (of a file this directory held before, one a copy came with, or a hard link an earlier
    /// build made, none of which vouches for anything here now), and an empty file made in their
    /// place. The caller knows `made` is on the media at `name`: a durable publication of it
    /// returned, or a full flush issued after its ordered one did. Nothing is flushed, and nothing
    /// fails: the proof speaks to a later process of this boot, and a proof lost or not made costs
    /// that process the full publication it would pay without one.
    fn prove(&self, name: &str, made: Identity) {
        use std::os::unix::fs::OpenOptionsExt as _;
        let (Some(stem), Some(proof)) = (proof_of(name), self.proof_path(name, made)) else {
            return;
        };
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let named = entry.file_name();
                if named.as_encoded_bytes().starts_with(stem.as_bytes()) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        #[cfg(test)]
        if self.proof_rule().links() {
            if std::fs::hard_link(self.path(name), &proof).is_ok()
                && identity_at(&proof).map(|linked| linked.ino) != Some(made.ino)
            {
                let _ = std::fs::remove_file(&proof);
            }
            return;
        }
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(STATE_FILE_MODE)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(&proof);
    }

    /// Whether a proof stands for `made`, what `name` names now as it stands now: then it is on the
    /// media at `name`. The proof is made only once it is ([`StateDir::prove`]), and every later
    /// write of `name` changes what the proof's name says: a rename over it gives `name` another
    /// inode, and bytes written into the inode move its change time ([`Identity`]). A power cut
    /// changes nothing of this: what reads after it is what the media held. No copy or restore of
    /// the dir holds a proof of a file it rewrote ([`StateDir::proof_path`]).
    fn proven(&self, name: &str, made: Identity) -> bool {
        let Some(proof) = self.proof_path(name, made) else {
            return false;
        };
        #[cfg(test)]
        if self.proof_rule().links() && self.proof_rule() != ProofRule::ByPresence {
            return identity_at(&proof).is_some_and(|linked| linked.ino == made.ino);
        }
        identity_at(&proof).is_some()
    }

    #[cfg(test)]
    pub(crate) fn ops(&self) -> FileOps {
        *self.ops.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn set_inc_seam(&self, seam: IncSeam) {
        *self.inc_seam.lock().unwrap_or_else(|e| e.into_inner()) = seam;
    }

    /// Refuse every write of `name` (none, at `None`) from now on, in place of the refusal set
    /// before, which it returns.
    #[cfg(test)]
    pub(crate) fn refuse_writes_of(&self, name: Option<&str>) -> Option<String> {
        std::mem::replace(
            &mut *self.refused.lock().unwrap_or_else(|e| e.into_inner()),
            name.map(str::to_string),
        )
    }

    #[cfg(test)]
    pub(crate) fn refuse_directory_flush(
        &self,
        at: Option<(&Path, aterm_durable::Flush, io::ErrorKind)>,
    ) {
        *self
            .dir_flush_refusal
            .lock()
            .unwrap_or_else(|e| e.into_inner()) =
            at.map(|(path, flush, kind)| (path.to_path_buf(), flush, kind));
    }

    #[cfg(test)]
    fn refused_by_a_test(&self, name: &str) -> io::Result<()> {
        let refused = self.refused.lock().unwrap_or_else(|e| e.into_inner());
        if refused.as_deref() == Some(name) {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                format!("{name}: no space left on device (a test's refusal)"),
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    fn inc_seam(&self) -> IncSeam {
        *self.inc_seam.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn kill_point(&self, _step: IncStep) -> io::Result<()> {
        #[cfg(test)]
        if self.inc_seam().kill_at == Some(_step) {
            return Err(io::Error::other(format!("killed at {_step:?}")));
        }
        Ok(())
    }

    /// Count this dir's writes for the wait tally from now on (`true`), or no more.
    pub(crate) fn time_ops(&self, _on: bool) {
        #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
        self.times.turn_on(_on);
    }

    /// Copy this dir's rows into the wait tally ([`crate::wait_tally::Kept`]).
    pub(crate) fn copy_dir_rows(&self, _t: &mut crate::wait_tally::Tally) {
        #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
        self.times.copy_into(_t);
    }

    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    fn timed(&self, row: crate::wait_tally::Row) -> crate::wait_tally::Timer<'_> {
        self.times.timer(row)
    }

    #[cfg(not(any(test, debug_assertions, feature = "dev-seams")))]
    fn timed(&self, _row: crate::wait_tally::Row) -> std::marker::PhantomData<&'_ ()> {
        std::marker::PhantomData
    }

    fn count(&self, _note: impl FnOnce(&mut FileOps)) {
        #[cfg(test)]
        _note(&mut self.ops.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// Unlink one file of the dir. An unlinked file leaves the set the clean stop flushes in
    /// place: the stop's flush of every [`TornKind`]'s directory puts the unlink on the media,
    /// and a later write of the name puts it back in the set.
    fn remove_path(&self, path: &Path) -> io::Result<()> {
        let w = crate::fx::Write::Remove { path };
        let _timed = self.timed(crate::wait_tally::Row::DirUnlink);
        self.count(|ops| ops.unlinks += 1);
        self.disk.before(w)?;
        std::fs::remove_file(path)?;
        if let Some(rel) = path.strip_prefix(&self.root).ok().and_then(Path::to_str) {
            self.unflushed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(rel);
        }
        self.disk.after(w)?;
        Ok(())
    }

    /// A state directory to read, creating nothing.
    #[must_use]
    pub fn peek(root: impl Into<PathBuf>) -> Self {
        Self::at(root.into())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn read(&self, name: &str) -> Option<String> {
        crate::enable::read_bounded_string(&self.path(name), 64 << 20)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// `name`'s text as [`Self::read`] reads it (trimmed, an empty file none), but an `Err` where
    /// the file exists and cannot be read: `None` only where it truly is not there.
    fn read_present(&self, name: &str) -> io::Result<Option<String>> {
        Ok(self
            .read_reservation(name)?
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty()))
    }

    fn read_reservation(&self, name: &str) -> io::Result<Option<String>> {
        self.count(|ops| ops.reads += 1);
        match crate::enable::read_bounded_string(&self.path(name), 64 << 20) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(self.path(name)) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(other) => Err(other),
                    Ok(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// The bus mirror's pin (`logpin`, [`crate::mirror::Pin`]).
    pub(crate) fn logpin(&self) -> io::Result<Option<String>> {
        self.read_reservation(LOGPIN)
    }

    /// Replace `logpin` whole, atomically, 0600 (it holds record bytes, as `inflight/` does).
    pub(crate) fn set_logpin(&self, text: &str) -> io::Result<()> {
        let mut known = self.logpin_fence.lock().unwrap_or_else(|e| e.into_inner());
        let durable = !self.logpin_fence_known(&mut known);
        self.replace_logpin(text, &mut known, durable)
    }

    /// Establish the current pin's v=2 fence before a copy, take or send, by two full flushes of
    /// the bridge's own: where no broker's flush settles the state dir's device
    /// ([`StateDir::order_logpin`], critic item F07).
    pub(crate) fn ensure_logpin_durable(&self, text: impl FnOnce() -> String) -> io::Result<()> {
        let mut known = self.logpin_fence.lock().unwrap_or_else(|e| e.into_inner());
        if self.logpin_fence_known(&mut known) {
            return Ok(());
        }
        self.replace_logpin(&text(), &mut known, true)
    }

    /// THE PIN'S FENCE BY BARRIERS, FOR A BROKER'S FULL FLUSH TO SETTLE (critic item F07): `logpin`
    /// replaced with its temp's bytes ordered by a barrier ahead of the rename, then a barrier of
    /// the state dir over the rename, and no full flush of the bridge's own. Apple's fcntl(2):
    /// `F_BARRIERFSYNC` "does the same thing as fsync(2) then issues a barrier command to the
    /// drive", and `F_FULLFSYNC` "drains the entire queue of the device and acts as a barrier,
    /// data that had been fsync'd on the same device before is guaranteed to be persisted when
    /// this call returns". So a full flush of any file of this device issued after this returns
    /// puts the pin and its entry on the media. The caller asks its broker for one (astream's
    /// `Client::send_settle`) only where the broker's `Hello` names the state dir's device
    /// ([`StateDir::device`]) and says its flush drains it, and holds the fence only once the
    /// broker answered it ([`StateDir::logpin_settled`]); a pin ordered here is no fence before.
    ///
    /// Answers whether this open holds the fence already, so that nothing waits on a settle. A
    /// pin this ordered that the name still holds is not written again: its barriers returned.
    ///
    /// # Errors
    /// The replace's, or the directory barrier's other than one the file system cannot make
    /// (`astream_sys::dir_flush_unavailable`, as a durable replace takes it).
    pub(crate) fn order_logpin(&self, text: impl FnOnce() -> String) -> io::Result<bool> {
        use aterm_durable::Flush;
        let mut known = self.logpin_fence.lock().unwrap_or_else(|e| e.into_inner());
        if self.logpin_fence_known(&mut known) {
            return Ok(true);
        }
        let mut ordered = self
            .logpin_ordered
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if ordered
            .as_ref()
            .is_some_and(|held| self.names_this_builds_pin(held))
        {
            return Ok(false);
        }
        let previous = ordered.take();
        let text = text();
        let _fence = self.timed(crate::wait_tally::Row::LogpinFence);
        self.replace_bytes_as(LOGPIN, text.as_bytes(), Some(Flush::Ordered), 0o600)?;
        self.flush_dir_with(&self.root, Flush::Ordered, tolerate_dir_flush)?;
        if crate::mirror::Pin::by_this_build(text.as_bytes()) {
            *ordered = self.retained_logpin();
        }
        drop(previous);
        Ok(false)
    }

    /// THE BROKER SETTLED THE PIN (critic item F07): its answer to a settle sent after
    /// [`StateDir::order_logpin`] returned, so its full flush, issued after the broker read the
    /// request, drained the device after the pin's barriers and put the pin on the media whole.
    /// Where the name still holds that pin, it is this open's fence. Answers whether this open
    /// holds the fence now.
    pub(crate) fn logpin_settled(&self) -> bool {
        let mut known = self.logpin_fence.lock().unwrap_or_else(|e| e.into_inner());
        let mut ordered = self
            .logpin_ordered
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let held = ordered.take();
        if self.logpin_fence_known(&mut known) {
            return true;
        }
        match held {
            Some(held) if self.names_this_builds_pin(&held) => {
                *known = Some(held);
                true
            }
            _ => false,
        }
    }

    /// The device the state dir is on (`st_dev`), which holds every file the pin's fence
    /// writes, to compare with the one its broker's `Hello` names (critic item F07, astream's
    /// `LogDevice::settles`).
    pub(crate) fn device(&self) -> Option<u64> {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(&self.root).ok().map(|meta| meta.dev())
    }

    /// Whether this open holds the current pin's v=2 fence, so the next
    /// [`StateDir::ensure_logpin_durable`] writes nothing (critic item D06: while it does not,
    /// the bus mirror's first pass after an attach is held, for at most `FIRST_PASS_HOLD`, for
    /// what the loop holds queued and for the records of the posts the attach's drains listed,
    /// and the pass sends the inbox commit its deliveries owe ahead of the fence).
    pub(crate) fn logpin_fence_held(&self) -> bool {
        let mut known = self.logpin_fence.lock().unwrap_or_else(|e| e.into_inner());
        self.logpin_fence_known(&mut known)
    }

    fn logpin_fence_known(&self, known: &mut Option<std::fs::File>) -> bool {
        if known
            .as_ref()
            .is_some_and(|held| self.names_this_builds_pin(held))
        {
            return true;
        }
        *known = None;
        false
    }

    /// Whether `held` is the file `logpin` names now, and begins as a pin of this build.
    fn names_this_builds_pin(&self, held: &std::fs::File) -> bool {
        use std::os::unix::fs::{FileExt as _, MetadataExt as _};
        let Ok(meta) = held.metadata() else {
            return false;
        };
        let mut head = [0u8; crate::mirror::PIN_HEADER.len() + 1];
        meta.is_file()
            && Some((meta.dev(), meta.ino())) == self.logpin_identity()
            && held
                .read_at(&mut head, 0)
                .ok()
                .is_some_and(|n| crate::mirror::Pin::by_this_build(&head[..n]))
    }

    fn logpin_identity(&self) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::symlink_metadata(self.path(LOGPIN)).ok()?;
        meta.is_file().then(|| (meta.dev(), meta.ino()))
    }

    fn retained_logpin(&self) -> Option<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(self.path(LOGPIN))
            .ok()?;
        file.metadata().ok()?.is_file().then_some(file)
    }

    fn replace_logpin(
        &self,
        text: &str,
        known: &mut Option<std::fs::File>,
        durable: bool,
    ) -> io::Result<()> {
        let previous = known.take();
        let _fence = durable.then(|| self.timed(crate::wait_tally::Row::LogpinFence));
        self.replace_bytes(LOGPIN, text.as_bytes(), durable, 0o600)?;
        if crate::mirror::Pin::by_this_build(text.as_bytes()) {
            *known = self.retained_logpin();
        }
        // A pin ordered for a settle is replaced: no settle makes it a fence.
        drop(
            self.logpin_ordered
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
        );
        drop(previous);
        Ok(())
    }

    /// Whether `logpin`, whole or torn, begins as an older build's pin ([`crate::mirror::Pin::by_an_older_build`]), which the durable copies need not cover.
    pub(crate) fn logpin_by_an_older_build(&self) -> bool {
        self.logpin_head()
            .is_some_and(|head| crate::mirror::Pin::by_an_older_build(&head))
    }

    fn logpin_head(&self) -> Option<Vec<u8>> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = self.path(LOGPIN);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)
            .ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() {
            return None;
        }
        let max = crate::mirror::PIN_HEADER.len() as u64 + 1;
        let mut head =
            Vec::with_capacity(usize::try_from(meta.len().min(max).saturating_add(1)).unwrap_or(0));
        file.take(max).read_to_end(&mut head).ok()?;
        Some(head)
    }

    /// The bus mirror's alias key, its durable copy (`logkey`).
    pub(crate) fn logkey(&self) -> io::Result<Option<[u8; 16]>> {
        let Some(text) = self.read_reservation(LOGKEY)? else {
            return Ok(None);
        };
        let mut key = [0u8; 16];
        match text.strip_suffix('\n') {
            Some(hex)
                if hex.len() == 32 && aterm_codec::hex::decode_into(hex.as_bytes(), &mut key) =>
            {
                Ok(Some(key))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} does not read as one alias key",
                    self.path(LOGKEY).display()
                ),
            )),
        }
    }

    /// Write `logkey` durably, 0600 (it is the secret the edges' aliases are keyed by).
    pub(crate) fn set_logkey(&self, key: &[u8; 16]) -> io::Result<()> {
        self.replace(LOGKEY, &aterm_codec::hex::encode_lower(key), true, 0o600)
    }

    /// The floor of the mirror's lifetime total (`logfloor`).
    pub(crate) fn logfloor(&self) -> io::Result<Option<u64>> {
        let Some(text) = self.read_reservation(LOGFLOOR)? else {
            return Ok(None);
        };
        text.strip_suffix('\n')
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse().ok())
            .map(Some)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} does not read as one floor",
                        self.path(LOGFLOOR).display()
                    ),
                )
            })
    }

    /// Raise `logfloor` to `top`, durably, and prove it on the media ([`LOGFLOOR_PROOF`]). A floor
    /// at or above `top` that this open admitted, or whose proof admits it now
    /// ([`StateDir::admit_logfloor_by_its_proof`]), is left as it reads.
    pub(crate) fn raise_logfloor(&self, top: u64) -> io::Result<u64> {
        let admitted = self.logfloor_admitted() || self.admit_logfloor_by_its_proof();
        match self.logfloor()? {
            Some(floor) if floor >= top && admitted => Ok(floor),
            floor => {
                let top = floor.map_or(top, |floor| floor.max(top));
                let bytes = format!("{top}\n");
                let made = self.publish(
                    LOGFLOOR,
                    bytes.as_bytes(),
                    Some(aterm_durable::Flush::Durable),
                    STATE_FILE_MODE,
                    std::convert::identity,
                )?;
                self.floor_admitted
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                if let Some(made) = made {
                    self.prove(LOGFLOOR, made);
                }
                Ok(top)
            }
        }
    }

    /// Admit the floor that reads, for this open, where a proof stands for the file `logfloor`
    /// names as it stands ([`StateDir::proven`]): a process published it durably and its flush
    /// returned, so it is on the media, whichever process reads it now. True where it admitted the
    /// floor now: not where this open had already, nor where a write of the floor this open made
    /// did not finish (the proof then names the file before it, if any).
    pub(crate) fn admit_logfloor_by_its_proof(&self) -> bool {
        use std::sync::atomic::Ordering;
        if self.floor_admitted.load(Ordering::SeqCst) || !self.on_the_media(LOGFLOOR) {
            return false;
        }
        let proven =
            identity_at(&self.path(LOGFLOOR)).is_some_and(|made| self.proven(LOGFLOOR, made));
        if proven {
            self.floor_admitted.store(true, Ordering::SeqCst);
        }
        proven
    }

    #[cfg(test)]
    pub(crate) fn raise_logfloor_trusting_what_reads(&self, top: u64) -> io::Result<u64> {
        match self.logfloor()? {
            Some(floor) if floor >= top => Ok(floor),
            _ => {
                self.replace(LOGFLOOR, &top.to_string(), true, STATE_FILE_MODE)?;
                Ok(top)
            }
        }
    }

    /// Whether the floor that reads is on the media as far as this open can say: this open wrote it durably, or admitted it by its proof ([`StateDir::admit_logfloor_by_its_proof`]), with no failed write of it since.
    pub(crate) fn logfloor_admitted(&self) -> bool {
        self.floor_admitted
            .load(std::sync::atomic::Ordering::SeqCst)
            && self.on_the_media(LOGFLOOR)
    }

    /// After an unclean stop in an earlier boot ([`StateDir::torn_files_possible`]), the mirror's pin is reconciled with its durable copies.
    pub(crate) fn reconcile_logpin(&self) -> io::Result<Reconciled> {
        self.reconcile_logpin_by(PinRules::ALL)
    }

    #[cfg(test)]
    pub(crate) fn reconcile_logpin_with(&self, rules: PinRules) -> io::Result<Reconciled> {
        self.reconcile_logpin_by(rules)
    }

    fn reconcile_logpin_by(&self, rules: PinRules) -> io::Result<Reconciled> {
        if !self.torn_files_possible()? {
            return Ok(Reconciled::Kept);
        }
        let Some(floor) = self.logfloor()? else {
            return Ok(Reconciled::Kept);
        };
        let key = self.logkey()?;
        let from_the_copies = || crate::mirror::Pin {
            key,
            total: if rules.total_from_floor { floor } else { 0 },
            ..crate::mirror::Pin::default()
        };
        let path = self.path(LOGPIN);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.set_logpin(&from_the_copies().render())?;
                return Ok(Reconciled::Restored);
            }
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_file() => return Ok(Reconciled::Kept),
            Ok(_) => {}
        }
        let bytes = crate::enable::read_bounded_bytes(&path, 64 << 20)?;
        match std::str::from_utf8(&bytes)
            .ok()
            .map(crate::mirror::Pin::parse)
        {
            Some(Ok(mut pin)) => {
                if !rules.total_from_floor || pin.total >= floor {
                    return Ok(Reconciled::Kept);
                }
                let from = pin.total;
                pin.total = floor;
                self.set_logpin(&pin.render())?;
                Ok(Reconciled::Raised { from, to: floor })
            }
            _ if !rules.set_aside_torn
                || (rules.need_key && key.is_none())
                || crate::mirror::Pin::by_an_older_build(&bytes) =>
            {
                Ok(Reconciled::Kept)
            }
            _ => {
                let moved = self.move_into_quarantine(&[(LOGPIN.to_string(), bytes)], "")?;
                let Some(moved) = moved.into_iter().next() else {
                    return Ok(Reconciled::Kept);
                };
                self.set_logpin(&from_the_copies().render())?;
                Ok(Reconciled::SetAside(moved))
            }
        }
    }

    /// Post `id` of `sid`'s sequence reservation, as the journal holds it ([`journal`]), unless an
    /// older build's `sent/<sid>.<id>` the journal could not take in is there: that file is read
    /// where it is, as that build read it (it would replace the journal's at its adoption), and
    /// refused naming it while it does not read; once it is gone the journal's answers.
    pub(crate) fn post_pin(&self, sid: &str, id: u64) -> io::Result<Option<PublishPin>> {
        let mut posts = self.posts()?;
        let name = format!("{sid}.{id}");
        if posts.held_sent.contains(&name) {
            let rel = format!("sent/{name}");
            let read = self
                .read_reservation(&rel)
                .and_then(|text| text.map(|text| PublishPin::parse(&text)).transpose());
            match read {
                Ok(None) => {
                    posts.held_sent.remove(&name);
                }
                Ok(pin) => return Ok(pin),
                Err(e) => {
                    return Err(io::Error::new(
                        e.kind(),
                        format!("{}: {e}", self.path(&rel).display()),
                    ))
                }
            }
        }
        Ok(posts.index().pin(sid, id).cloned())
    }

    /// Reserve `pin` for post `id` of `sid`, before it is published: one journal record.
    pub(crate) fn set_post_pin(&self, sid: &str, id: u64, pin: &PublishPin) -> io::Result<()> {
        let _timed = self.timed(crate::wait_tally::Row::SetPostPin);
        let mut posts = self.posts()?;
        self.append_post_frame(
            &mut posts,
            journal::Frame::Reserve {
                sid: sid.to_string(),
                id,
                pin: pin.clone(),
            },
        )
    }

    /// The post journal, read and judged if this open has not read it yet
    /// ([`StateDir::judge_posts`]); where this open may write, a journal an attach it does not
    /// name went past has its records in flight retired ([`StateDir::retire_journal_in_flight`]) and
    /// then an older build's files are taken into it ([`StateDir::adopt_legacy_posts`]).
    ///
    /// # Errors
    /// The journal unreadable, not a regular file, or damaged (`InvalidData`, naming where: it
    /// is kept for repair, as a damaged `inflight/<seq>` was), or the retirement's or the
    /// adoption's write. An older build's file that does not read is no error here: it is held
    /// ([`Posts::held_sent`], [`Posts::held_in_flight`]).
    fn posts(&self) -> io::Result<std::sync::MutexGuard<'_, Posts>> {
        let mut posts = self.judge_posts()?;
        if self.adopts_legacy && posts.stale {
            let retired = self.retire_journal_in_flight(&mut posts)?;
            posts.stale = false;
            self.stale_retired
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(retired);
        }
        if self.adopts_legacy && !posts.adopted {
            self.adopt_legacy_posts(&mut posts)?;
            posts.adopted = true;
        }
        Ok(posts)
    }

    /// THE JOURNAL JUDGED AGAINST THE DIR'S INCARNATION, once an open: read if it has not been,
    /// and stale where it names an attach ([`journal::Index::attached`]) and the dir has taken
    /// an incarnation above it since ([`StateDir::incarnation`]: the lease's, or that of a
    /// sequence reserved). Every attach of this build names the incarnation it takes in the
    /// journal before the lease says so ([`StateDir::take_incarnation`]); a bridge of an older
    /// build takes one too, and names nothing. So a stale journal is one an older build's attach
    /// went past after this build's last unclean stop (a rollback, or a dev bundle sharing the
    /// dir): that bridge published what it found in `sent/` and `inflight/`, never the
    /// journal's records, and may have published a post the journal holds in flight again under
    /// a fresh sequence and retired it, so the journal's records in flight are retired, not
    /// replayed ([`StateDir::posts`]). Its reservations stay: a post that bridge did not retire
    /// keeps its number, and one it reserved afresh comes back in its file and replaces it at the
    /// adoption. An older build's bridge that never attached took nothing and published
    /// nothing. A journal no attach named is not judged stale, and nor is one after an unclean
    /// stop in an earlier boot ([`StateDir::torn_files_possible`]): the start retires every
    /// record in flight then anyway ([`StateDir::retire_in_flight_of_an_earlier_boot`]), and a
    /// power cut can take a name the device had not yet put on the media.
    fn judge_posts(&self) -> io::Result<std::sync::MutexGuard<'_, Posts>> {
        let mut posts = self.posts.lock().unwrap_or_else(|e| e.into_inner());
        if posts.index.is_none() {
            self.read_posts(&mut posts)?;
        }
        if !posts.judged {
            posts.stale = posts
                .index()
                .attached
                .is_some_and(|named| self.incarnation() > named)
                && !matches!(self.torn_files_possible(), Ok(true));
            posts.handed = self.read_handed();
            posts.judged = true;
        }
        Ok(posts)
    }

    /// WHAT A STOP'S HAND-OFF KEPT ([`journal::HANDED`]), judged as the journal is: kept where it
    /// names the attach the dir's incarnation is still at, so no attach went past the files it
    /// wrote since (a bridge of an older build takes an incarnation and names nothing, and may
    /// have published a post the hand-off held in flight again, and retired it, so its head no
    /// longer bounds only that post's copies); `None` where it is absent, damaged, names no
    /// attach, or an attach went past it. Judged before the attach that adopts the files takes
    /// its incarnation ([`StateDir::name_attach_in_posts`]).
    fn read_handed(&self) -> Option<journal::Index> {
        let bytes = self.read_regular(&self.path(journal::HANDED)).ok()??;
        let read = journal::read(&bytes);
        if matches!(read.end, journal::End::Fault { .. }) {
            return None;
        }
        let named = read.index.attached?;
        (self.incarnation() <= named).then_some(read.index)
    }

    /// An attach of this build takes incarnation `inc`: the journal names it
    /// ([`journal::Frame::Attached`]) before the lease does, so the next open judges the
    /// journal current ([`StateDir::judge_posts`]), a stop between the two included. Written at
    /// once where the journal holds a reservation or a record in flight; otherwise held in the
    /// open and written before the journal's next record, so an attach writes nothing to a
    /// journal that holds nothing (a stale verdict on it retires nothing).
    ///
    /// Where a stop's hand-off kept heads that are still current, its files are taken into the
    /// journal first ([`StateDir::posts`]), heads and all, before the lease moves past the attach
    /// that hand-off names (item F4): an attach that took its incarnation and stopped before it
    /// read the journal (its run record refused) left those heads stale for the next, whose
    /// replay then published each record in flight again at its pin unread, twice past a cut
    /// that dropped its key.
    ///
    /// # Errors
    /// The journal unreadable or damaged, the adoption's write (the heads still current for
    /// the next attach), or the name's write where it is written at once: the open holds it
    /// all the same and rewrites the journal before its next record
    /// ([`StateDir::append_post_frame`]), and the attach does not take the incarnation.
    fn name_attach_in_posts(&self, inc: u64) -> io::Result<()> {
        #[cfg(test)]
        let adopts_first = !self
            .adopt_after_attach
            .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let adopts_first = true;
        let mut posts = self.judge_posts()?;
        if adopts_first && posts.handed.is_some() {
            drop(posts);
            posts = self.posts()?;
        }
        if posts.exists && !posts.index().holds_nothing() {
            return self.append_post_frame(&mut posts, journal::Frame::Attached(inc));
        }
        if let Some(index) = posts.index.as_mut() {
            index.apply(journal::Frame::Attached(inc));
            posts.attach_unwritten = true;
        }
        Ok(())
    }

    /// The records in flight a stale journal held, retired at this open's first use
    /// ([`StateDir::judge_posts`]), for the bridge to tell; each taken once.
    pub(crate) fn take_stale_retired(&self) -> Vec<SetAside> {
        std::mem::take(&mut *self.stale_retired.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn read_posts(&self, posts: &mut Posts) -> io::Result<()> {
        let path = self.path(journal::FILE);
        let Some(bytes) = self.read_regular(&path)? else {
            *posts = Posts {
                index: Some(journal::Index::default()),
                compact_at: journal::COMPACT_AT,
                ..Posts::default()
            };
            return Ok(());
        };
        let read = journal::read(&bytes);
        let damage = match &read.end {
            journal::End::Clean | journal::End::Cut => None,
            journal::End::Torn => Some((read.valid_len, "a broken last record".to_string())),
            journal::End::Fault { at, why } => Some((*at, why.clone())),
        };
        if let Some((at, why)) = damage {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}: damaged at byte {at} of {} ({why}); kept for repair",
                    path.display(),
                    bytes.len()
                ),
            ));
        }
        *posts = Posts {
            index: Some(read.index),
            adopted: posts.adopted,
            file: None,
            exists: true,
            len: read.valid_len,
            next_step: read.next_step,
            dirty: read.end != journal::End::Clean,
            entry_on_media: false,
            compact_at: journal::COMPACT_AT,
            ..Posts::default()
        };
        Ok(())
    }

    /// The bytes of the regular file at `path`, never followed through a link, or `None` where
    /// nothing is there.
    fn read_regular(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        self.count(|ops| ops.reads += 1);
        match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(path)
        {
            Ok(file) => {
                let meta = file.metadata()?;
                if !meta.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} is not a regular file", path.display()),
                    ));
                }
                const MAX_REGULAR_BYTES: u64 = 64 << 20;
                if meta.len() > MAX_REGULAR_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} exceeds size limit", path.display()),
                    ));
                }
                let mut bytes = Vec::with_capacity(
                    usize::try_from(meta.len().min(MAX_REGULAR_BYTES).saturating_add(1))
                        .unwrap_or(0),
                );
                (&file)
                    .take(MAX_REGULAR_BYTES + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_REGULAR_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} exceeds size limit", path.display()),
                    ));
                }
                Ok(Some(bytes))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(path) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(other) => Err(other),
                    Ok(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Append one record to the post journal and apply it to the index. A journal that may hold
    /// bytes past its last whole record, or what the index no longer holds, or has grown past its
    /// bound, is rewritten from the index first ([`StateDir::rewrite_posts`]); one this open
    /// creates, or one whose last attach the open holds unwritten, takes the attach the index
    /// names in the same write ([`journal::Index::attached`]). On a failure the
    /// file is rewritten from the index before the next record, and the index is as it was,
    /// except that a fact the open holds unwritten ([`journal::Frame::holds_unwritten`]: a
    /// retirement, a drop, an attach named) is applied all the same, as the unlink of an older
    /// build's file was done on a disk too full to write: a record the open dropped is never
    /// published again by it, nor handed to the files at its stop.
    fn append_post_frame(&self, posts: &mut Posts, frame: journal::Frame) -> io::Result<()> {
        self.append_post_frames(posts, vec![frame])
    }

    /// Append `frames` to the post journal in one write, in order, and apply each to the index
    /// as [`StateDir::append_post_frame`] applies one (critic item E07). One `write_all` sends
    /// their encoded records in order; interruption can leave a complete prefix and a partial
    /// final record. The next open keeps the complete records and treats that partial record
    /// as the journal's end ([`journal`]). On a failure none is applied but the facts the open
    /// holds unwritten.
    fn append_post_frames(&self, posts: &mut Posts, frames: Vec<journal::Frame>) -> io::Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let written = self.write_post_frames(posts, &frames);
        for frame in frames {
            #[cfg(test)]
            let held = frame.holds_unwritten()
                && !(matches!(frame, journal::Frame::Dropped(_))
                    && self
                        .keep_refused_drops
                        .load(std::sync::atomic::Ordering::SeqCst));
            #[cfg(not(test))]
            let held = frame.holds_unwritten();
            if written.is_ok() || held {
                if written.is_err() {
                    posts.dirty = true;
                }
                if let Some(index) = posts.index.as_mut() {
                    index.apply(frame);
                }
            }
        }
        written
    }

    /// Open `path` to append to it, created where it is not there and never through a link at
    /// the name: an owned regular single-link file, so no append lands in a file another name
    /// holds. Answers the file and its length.
    ///
    /// # Errors
    /// The open, or a file that is none of those (`InvalidData`, naming it as `what`).
    fn open_for_appends(&self, path: &Path, what: &str) -> io::Result<(std::fs::File, u64)> {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        self.count(|ops| ops.opens += 1);
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(STATE_FILE_MODE)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != aterm_uds::peer::our_uid() || meta.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the {what} is not an owned regular single-link file"),
            ));
        }
        Ok((file, meta.len()))
    }

    /// Open the journal at `path` to append to it, created where it is not there: an owned
    /// regular single-link file of `len` bytes, the bytes this open holds it to.
    ///
    /// # Errors
    /// Its directory or its open, or a file that is none of those (`InvalidData`, naming the
    /// journal as `what`): it is rewritten from what the open holds before its next record.
    fn open_journal(&self, path: &Path, what: &str, len: u64) -> io::Result<std::fs::File> {
        std::fs::create_dir_all(self.path(journal::DIR))?;
        let (file, holds) = self.open_for_appends(path, what)?;
        if holds != len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the {what} holds {holds} bytes where this open wrote {len}; it is rewritten \
                     from what this open holds before the next record",
                ),
            ));
        }
        Ok(file)
    }

    fn write_post_frames(&self, posts: &mut Posts, frames: &[journal::Frame]) -> io::Result<()> {
        if posts.dirty || posts.len >= posts.compact_at.max(journal::COMPACT_AT) {
            self.rewrite_posts(posts, None)?;
        }
        #[cfg(test)]
        {
            self.refused_by_a_test(journal::FILE)?;
            for frame in frames {
                if let Some(file) = frame.written_file() {
                    self.refused_by_a_test(&file)?;
                }
            }
        }
        let (mut bytes, mut step) = if posts.exists && !posts.attach_unwritten {
            (Vec::new(), posts.next_step)
        } else {
            journal::Index {
                attached: posts.index().attached,
                ..journal::Index::EMPTY
            }
            .encode_from(posts.next_step)?
        };
        for frame in frames {
            bytes.extend_from_slice(&journal::encode(step, frame)?);
            step += 1;
        }
        let path = self.path(journal::FILE);
        let w = crate::fx::Write::Append {
            path: &path,
            bytes: &bytes,
        };
        let _timed = self.timed(crate::wait_tally::Row::DirAppend);
        self.count(|ops| ops.appends += 1);
        self.disk.before(w)?;
        let written = (|| -> io::Result<()> {
            if posts.file.is_none() {
                let file = self.open_journal(&path, "post journal", posts.len)?;
                if !posts.exists {
                    posts.entry_on_media = false;
                }
                posts.exists = true;
                posts.file = Some(file);
            }
            posts
                .file
                .as_mut()
                .map_or(Ok(()), |file| file.write_all(&bytes))
        })();
        if let Err(e) = written.and_then(|()| self.disk.after(w)) {
            posts.dirty = true;
            posts.file = None;
            return Err(e);
        }
        posts.len += bytes.len() as u64;
        posts.next_step = step;
        posts.attach_unwritten = false;
        let mut unflushed = self.unflushed.lock().unwrap_or_else(|e| e.into_inner());
        if !unflushed.contains(journal::FILE) {
            unflushed.insert(journal::FILE.to_string());
        }
        Ok(())
    }

    /// Replace the post journal by one that holds exactly the index ([`journal::Index::encode`]),
    /// flushed by `flush`: an atomic replace, so a reader sees the old journal or the new.
    fn rewrite_posts(
        &self,
        posts: &mut Posts,
        flush: Option<aterm_durable::Flush>,
    ) -> io::Result<()> {
        let (bytes, next_step) = posts.index().encode()?;
        posts.file = None;
        if let Err(e) = self.replace_bytes_as(journal::FILE, &bytes, flush, STATE_FILE_MODE) {
            posts.dirty = true;
            return Err(e);
        }
        posts.exists = true;
        posts.len = bytes.len() as u64;
        posts.next_step = next_step;
        posts.dirty = false;
        posts.attach_unwritten = false;
        posts.entry_on_media = flush == Some(aterm_durable::Flush::Durable);
        posts.compact_at = journal::COMPACT_AT.max(2 * posts.len);
        Ok(())
    }

    /// Put every record of the post journal on the media by `flush`, its directory entry too
    /// where this open does not know it is there. A journal that holds what the index no longer
    /// does, or bytes past its last whole record, is rewritten from the index under that flush
    /// instead ([`StateDir::rewrite_posts`]).
    fn sync_posts(&self, posts: &mut Posts, flush: aterm_durable::Flush) -> io::Result<()> {
        if posts.dirty {
            self.rewrite_posts(posts, Some(flush))?;
        } else if !posts.exists {
            return Ok(());
        } else {
            self.flush_in_place(journal::FILE, flush)?;
        }
        if !posts.entry_on_media {
            self.flush_dir_with(&self.path(journal::DIR), flush, tolerate_dir_flush)?;
            posts.entry_on_media = flush.is_durable();
        }
        if flush.is_durable() {
            self.unflushed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(journal::FILE);
        }
        Ok(())
    }

    /// FOR WHICHEVER BUILD RUNS NEXT: every record the journal holds written as the file a build
    /// before the journal reads it from (`sent/<sid>.<id>`, `inflight/<seq>`), then the journal
    /// unlinked, so a bridge of an older build on this dir (a rollback, or a dev bundle sharing
    /// it) finds each post's reservation and each record in flight where it looks, and publishes
    /// none of them again under a fresh sequence. A bridge of this build takes them back at its
    /// first read ([`StateDir::adopt_legacy_posts`]), this one too if it goes on. A cut between
    /// the files and the unlink leaves both, which that read takes as the same records; so does
    /// a write of the heads the disk refuses, the journal kept for them. A stop
    /// calls it before its clean stop, which then flushes the files, the heads kept beside them
    /// and the journal's directory under its record. The attach the journal named stays named in
    /// the open, for a journal created after. Returns how many records it wrote.
    ///
    /// # Errors
    /// The journal unreadable, or a write: what is written stays, and the journal keeps every
    /// record, rewritten from what the open holds where it held more.
    pub(crate) fn hand_posts_to_files(&self) -> io::Result<usize> {
        let mut posts = self.posts()?;
        let index = posts.index().clone();
        if index.holds_nothing() {
            return Ok(0);
        }
        let written = (|| -> io::Result<()> {
            for (sid, id, pin) in index.pins() {
                self.write(&format!("sent/{sid}.{id}"), &pin.render())?;
            }
            for (seq, entry) in &index.in_flight {
                self.write(&format!("inflight/{seq}"), &entry.render())?;
            }
            Ok(())
        })();
        if let Err(e) = written {
            if posts.dirty {
                let _ = self.rewrite_posts(&mut posts, None);
            }
            return Err(e);
        }
        posts.file = None;
        // The heads the files cannot hold, beside them for this build's adoption: a hint one an
        // older build's attach went past is not taken. Where it cannot be written the journal
        // stays beside the files, as a cut before the unlink leaves it, so a bridge of this
        // build reads each record's head from the journal and reads it back before it publishes
        // it again (item F4: unlinked, the next bridge published each again at its pin, and a
        // cut that had dropped its key put a post or a receipt on the bus twice).
        #[cfg(test)]
        let keeps_journal = !self
            .hand_off_without_heads
            .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let keeps_journal = true;
        if let Ok((handed, _)) = index.encode() {
            if let Err(e) = self.replace_bytes_as(journal::HANDED, &handed, None, STATE_FILE_MODE) {
                if keeps_journal {
                    eprintln!(
                        "aterm-link: the post journal's heads could not be kept beside the \
                         files: {e}; the journal stays beside them for the next bridge to read \
                         them back from"
                    );
                    return Ok(index.pins().count() + index.in_flight.len());
                }
                eprintln!(
                    "aterm-link: the post journal's heads could not be kept beside the files: \
                     {e}; the next bridge publishes those records again without reading them back"
                );
            }
        }
        match self.remove_path(&self.path(journal::FILE)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                posts.dirty = true;
                return Err(e);
            }
            _ => {}
        }
        *posts = Posts {
            index: Some(journal::Index {
                attached: index.attached,
                ..journal::Index::EMPTY
            }),
            judged: true,
            compact_at: journal::COMPACT_AT,
            handed: Some(index.clone()),
            ..Posts::default()
        };
        Ok(index.pins().count() + index.in_flight.len())
    }

    /// TAKE AN OLDER BUILD'S FILES INTO THE JOURNAL: every `sent/<sid>.<id>` and
    /// `inflight/<seq>` an older bridge left, or a stop's hand-off wrote for whichever build ran
    /// next ([`StateDir::hand_posts_to_files`]; a seamless update's successor continues its run),
    /// appended as a record where the journal does not hold that very record already, the
    /// journal flushed where anything was appended, and only then the files unlinked, so a cut at
    /// any point leaves each reservation and entry in the files, in the journal, or in both
    /// (taken again, the same records). The flush has only to put the records on the media
    /// before the unlinks: a barrier where the journal shares a device with the files'
    /// directories, a full flush otherwise. The unlinks are not flushed (a clean stop flushes
    /// those directories): a file a power cut brings back holds a record the journal took, and
    /// the next boot retires an earlier boot's records in flight and holds its reservations, of
    /// sessions that boot ended, as an older build's unflushed unlink left them
    /// ([`StateDir::retire_in_flight_of_an_earlier_boot`]). A file that does not read is held
    /// where it is ([`Posts::held_sent`], [`Posts::held_in_flight`]) and the others taken in.
    /// After an unclean stop in an earlier boot the sweep and the retirement have dealt with the
    /// files first ([`StateDir::set_aside_torn`]).
    ///
    /// # Errors
    /// A directory that does not list, or a write.
    fn adopt_legacy_posts(&self, posts: &mut Posts) -> io::Result<()> {
        let sent = self.kind_files(TornKind::Sent)?;
        let in_flight = self.kind_files(TornKind::InFlight)?;
        posts.held_sent.clear();
        posts.held_in_flight = None;
        let mut frames = Vec::new();
        let mut taken: Vec<&str> = Vec::new();
        for (name, rel) in &sent {
            let pin = match self.read_reservation(rel) {
                Ok(None) => continue,
                Ok(Some(text)) => PublishPin::parse(&text).ok(),
                Err(_) => None,
            };
            let post = name
                .rsplit_once('.')
                .and_then(|(sid, id)| Some((sid, crate::parse_uint::<u64>(id)?)));
            let (Some(pin), Some((sid, id))) = (pin, post) else {
                posts.held_sent.insert(name.clone());
                continue;
            };
            // The journal may hold the very record with its head (a cut between the hand-off's
            // files and its unlink), kept as it is; one it does not hold takes the hand-off's.
            if posts.index().pin(sid, id).map(PublishPin::without_head) != Some(pin.clone()) {
                frames.push(journal::Frame::Reserve {
                    sid: sid.to_string(),
                    id,
                    pin: posts.handed_pin(sid, id, pin),
                });
            }
            taken.push(rel);
        }
        for (name, rel) in &in_flight {
            let entry = match self.read_reservation(rel) {
                Ok(None) => continue,
                Ok(Some(text)) => {
                    crate::parse_uint(name).and_then(|seq| InFlight::parse(seq, &text))
                }
                Err(_) => None,
            };
            let Some(entry) = entry else {
                posts
                    .held_in_flight
                    .get_or_insert_with(|| self.path(rel).display().to_string());
                continue;
            };
            let held = posts.index().in_flight.get(&entry.pin.seq);
            if held.map(InFlight::without_head) != Some(entry.clone()) {
                frames.push(journal::Frame::InFlight(posts.handed_entry(entry)));
            }
            taken.push(rel);
        }
        // THE HAND-OFF'S ATTACH, NAMED WITH THE RECORDS IT KEPT HEADS FOR (item F4): a journal an
        // adoption starts names no attach until this open's own takes its incarnation, and a
        // hand-off of it before then (an attach that never came up) kept its heads under no
        // attach, which the next attach refuses as naming nothing, publishing each record again
        // at its pin unread, twice past a cut that dropped its key. The hand-off's own attach
        // took the incarnation the dir is still at (its heads are current), so it is named.
        #[cfg(test)]
        let names_the_attach = !self
            .adopt_without_the_attach
            .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let names_the_attach = true;
        if names_the_attach && !frames.is_empty() && posts.index().attached.is_none() {
            if let Some(named) = posts.handed.as_ref().and_then(|handed| handed.attached) {
                frames.insert(0, journal::Frame::Attached(named));
            }
        }
        if !frames.is_empty() || (posts.dirty && !taken.is_empty()) {
            for frame in frames {
                self.append_post_frame(posts, frame)?;
            }
            let journal_dir = self.path(journal::DIR);
            let shared = [(TornKind::Sent, &sent), (TornKind::InFlight, &in_flight)]
                .into_iter()
                .filter(|(_, files)| !files.is_empty())
                .all(|(kind, _)| {
                    aterm_durable::Flush::ordered_if_same_device(
                        &journal_dir,
                        &self.path(kind.place()),
                    )
                    .is_ordered()
                });
            let tier = if shared {
                aterm_durable::Flush::Ordered
            } else {
                aterm_durable::Flush::Durable
            };
            self.sync_posts(posts, tier)?;
        }
        for rel in taken {
            match self.remove_path(&self.path(rel)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        // Every file the hand-off kept heads for is the journal's now. One left behind (a write
        // that failed) is judged stale by the next attach's incarnation, never taken.
        posts.handed = None;
        let handed = self.path(journal::HANDED);
        if std::fs::symlink_metadata(&handed).is_ok() {
            let _ = self.remove_path(&handed);
        }
        Ok(())
    }

    /// WHAT THE ASKS JOURNAL SAYS, read from the disk now ([`asks`]): its whole records, or,
    /// where it does not read whole (damage, or a broken last record), every whole record
    /// before and after the damage ([`journal::salvage_as`]); then reconciled with the tables a
    /// build before the journal reads ([`asks::Index::take_in`]), an empty one included, since
    /// that build writes an empty table where it holds nothing. Nothing is written: a journal
    /// that does not read whole, or that took a table in, is rewritten whole at the next write
    /// ([`AsksRead::dirty`]). A journal that cannot be read at all is read as empty, as a build
    /// before the journal read a table it could not.
    fn read_asks(&self) -> AsksRead {
        let mut read = AsksRead::default();
        let mut lost = false;
        match self.read_regular(&self.path(asks::FILE)) {
            Ok(None) => {}
            Ok(Some(bytes)) => {
                let whole = asks::read(&bytes);
                match whole.end {
                    journal::End::Clean | journal::End::Cut => {
                        read.dirty = whole.end != journal::End::Clean;
                        read.index = whole.index;
                        read.len = whole.valid_len;
                        read.next_step = whole.next_step;
                    }
                    journal::End::Torn | journal::End::Fault { .. } => {
                        read.index = journal::salvage_as(&bytes);
                        read.dirty = true;
                        read.damaged = Some(bytes);
                        lost = true;
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "aterm-link: the asks journal does not read ({e}); it is read as empty and \
                     written whole from what this bridge notes"
                );
                read.dirty = true;
                lost = true;
            }
        }
        let table =
            |name: &str| crate::enable::read_bounded_string(&self.path(name), 64 << 20).ok();
        let (asked, deadlines) = (table(asks::ASKED), table(asks::DEADLINES));
        read.dirty |= read
            .index
            .take_in(asked.as_deref(), deadlines.as_deref(), lost);
        read
    }

    /// The asks journal as this open holds it, read at the first use ([`StateDir::read_asks`]).
    /// A journal that does not read whole is copied into the quarantine first, its exact bytes
    /// kept for whoever repairs it. A refused copy is said and stops nothing, since a bridge
    /// cannot go on without its asks: the next write replaces the journal with its whole
    /// records all the same, and those bytes are gone. The sweep after an earlier boot, which
    /// a refused copy does stop ([`StateDir::sweep_asks`]), has already kept them where it ran.
    fn asks(&self) -> std::sync::MutexGuard<'_, Asks> {
        let mut held = self.asks.lock().unwrap_or_else(|e| e.into_inner());
        if held.index.is_none() {
            let read = self.read_asks();
            if let Some(bytes) = read.damaged {
                match self.copy_into_quarantine(&[(asks::FILE.to_string(), bytes)], "") {
                    Ok(moved) => {
                        for SetAside { from, to } in moved {
                            eprintln!(
                                "aterm-link: {from} does not read whole: a copy is kept as {to}, \
                                 and it is written again from its whole records"
                            );
                        }
                    }
                    Err(e) => eprintln!(
                        "aterm-link: {} does not read whole and could not be copied into the \
                         quarantine ({e}); it is written again from its whole records",
                        asks::FILE
                    ),
                }
            }
            *held = Asks {
                index: Some(read.index),
                owed: Vec::new(),
                file: None,
                len: read.len,
                next_step: read.next_step,
                dirty: read.dirty,
                compact_at: journal::COMPACT_AT,
            };
        }
        held
    }

    /// Every ask's recipient and every deadline owed a verdict, oldest offset first, as this
    /// open holds them: what the bridge starts from.
    pub(crate) fn asks_tables(&self) -> (Vec<Asked>, Vec<Deadline>) {
        let asks = self.asks();
        (asks.index().asked_rows(), asks.index().deadline_rows())
    }

    /// Whether the deadline of `sid`'s ask at `off` is settled, as this open holds the journal:
    /// a settle written, or one this open noted that no write has taken yet.
    pub(crate) fn deadline_settled(&self, off: u64, sid: &str) -> bool {
        self.asks().index().settled(off, sid)
    }

    /// Note `frames` in the asks journal as this open holds it, for the next write
    /// ([`StateDir::write_asks`]) to append in one. The open holds each as true from now on,
    /// written or not: a write the disk refuses leaves the journal to be rewritten whole from
    /// what the open holds.
    ///
    /// # Errors
    /// A fact whose record would not read back (`InvalidInput`, [`asks::Frame::carried`]): it is
    /// noted nowhere, and those after it are not noted.
    pub(crate) fn note_asks(
        &self,
        frames: impl IntoIterator<Item = asks::Frame>,
    ) -> io::Result<()> {
        let mut asks = self.asks();
        for frame in frames {
            if !frame.carried() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("an asks journal record would not read back: {frame:?}"),
                ));
            }
            if let Some(index) = asks.index.as_mut() {
                index.apply(frame.clone());
            }
            asks.owed.push(frame);
        }
        Ok(())
    }

    /// Write what the asks journal owes as this open holds it: every fact noted since the last
    /// write, appended in one write, or, where the file may not hold what the open does (a write
    /// refused, a broken tail, a table taken in) or has grown past its bound, the file rewritten
    /// from what the open holds ([`StateDir::rewrite_asks`]), noted facts or none. A file that
    /// holds what the open does, with nothing noted, is not written.
    /// Ok means the file holds everything the open does. Never flushed: a clean stop flushes
    /// it with the other journal ([`StateDir::end_run_clean`]).
    ///
    /// # Errors
    /// The append or the rewrite: the open still holds every fact, and the file is rewritten
    /// from it at the next write.
    pub(crate) fn write_asks(&self) -> io::Result<()> {
        let mut asks = self.asks();
        self.write_owed_asks(&mut asks)
    }

    fn write_owed_asks(&self, asks: &mut Asks) -> io::Result<()> {
        let owed = std::mem::take(&mut asks.owed);
        let written = self.write_ask_records(asks, &owed);
        if written.is_err() {
            asks.dirty = true;
            asks.file = None;
        }
        written
    }

    fn write_ask_records(&self, asks: &mut Asks, owed: &[asks::Frame]) -> io::Result<()> {
        #[cfg(test)]
        {
            self.refused_by_a_test(asks::FILE)?;
            for frame in owed {
                if let Some(file) = frame.written_file() {
                    self.refused_by_a_test(file)?;
                }
            }
        }
        if asks.dirty || asks.len >= asks.compact_at.max(journal::COMPACT_AT) {
            return self.rewrite_asks(asks, None);
        }
        if owed.is_empty() {
            return Ok(());
        }
        let (bytes, step) = asks::encode_from(asks.next_step, owed)?;
        let path = self.path(asks::FILE);
        let w = crate::fx::Write::Append {
            path: &path,
            bytes: &bytes,
        };
        let _timed = self.timed(crate::wait_tally::Row::DirAppend);
        self.count(|ops| ops.appends += 1);
        self.disk.before(w)?;
        if asks.file.is_none() {
            asks.file = Some(self.open_journal(&path, "asks journal", asks.len)?);
        }
        asks.file
            .as_mut()
            .map_or(Ok(()), |file| file.write_all(&bytes))?;
        self.disk.after(w)?;
        asks.len += bytes.len() as u64;
        asks.next_step = step;
        let mut unflushed = self.unflushed.lock().unwrap_or_else(|e| e.into_inner());
        if !unflushed.contains(asks::FILE) {
            unflushed.insert(asks::FILE.to_string());
        }
        Ok(())
    }

    /// Replace the asks journal by one that holds exactly what the open does
    /// ([`asks::Index::encode`]), flushed by `flush`: an atomic replace, so a reader sees the old
    /// journal or the new. The next rewrite at the size bound waits for twice this one's size.
    fn rewrite_asks(&self, asks: &mut Asks, flush: Option<aterm_durable::Flush>) -> io::Result<()> {
        #[cfg(test)]
        {
            // A rewrite writes both tables a build before the journal kept apart.
            self.refused_by_a_test(asks::DEADLINES)?;
            self.refused_by_a_test(asks::ASKED)?;
        }
        let (bytes, next_step) = asks.index().encode()?;
        asks.file = None;
        self.replace_bytes_as(asks::FILE, &bytes, flush, STATE_FILE_MODE)?;
        asks.len = bytes.len() as u64;
        asks.next_step = next_step;
        asks.dirty = false;
        asks.compact_at = journal::COMPACT_AT.max(2 * asks.len);
        Ok(())
    }

    /// FOR WHICHEVER BUILD RUNS NEXT: what the asks journal holds as this open holds it, written
    /// back as the tables a build before the journal reads ([`asks::ASKED`], [`asks::DEADLINES`]),
    /// one replace each, unflushed as that build wrote them, each opened by the line that names
    /// this hand-off ([`asks::legacy_text`]); then the hand-off recorded in the journal
    /// ([`asks::Frame::Handed`]), so the next open of this build takes nothing in from tables that
    /// name it ([`StateDir::read_asks`]). A rollback, or a dev bundle sharing the dir, finds every
    /// ask's recipient and every deadline where it looks. A stop calls it, and a log reset after
    /// it clamps the journal ([`StateDir::forget_log_offsets`]). A dir that never asked, with no
    /// table, writes nothing.
    ///
    /// # Errors
    /// A table's write, or the journal's: the open still holds everything, and the tables are
    /// written again at the next hand-off.
    pub(crate) fn hand_asks_to_files(&self) -> io::Result<()> {
        let mut asks = self.asks();
        let index = asks.index();
        let holds_nothing =
            index.asked.is_empty() && index.deadlines.is_empty() && index.gone.is_empty();
        let tables = [asks::ASKED, asks::DEADLINES]
            .iter()
            .any(|name| std::fs::symlink_metadata(self.path(name)).is_ok());
        if holds_nothing && !tables {
            return Ok(());
        }
        let handed = index.handed.map_or(1, |n| n.saturating_add(1));
        let asked = asks::legacy_text(handed, index.asked_rows().iter().map(Asked::render));
        let deadlines =
            asks::legacy_text(handed, index.deadline_rows().iter().map(Deadline::render));
        self.write(asks::ASKED, &asked)?;
        self.write(asks::DEADLINES, &deadlines)?;
        let frame = asks::Frame::Handed(handed);
        if let Some(index) = asks.index.as_mut() {
            index.apply(frame.clone());
        }
        asks.owed.push(frame);
        self.write_owed_asks(&mut asks)
    }

    /// A log reset's clamp of the asks journal: every ask's recipient, deadline and settle at an
    /// offset `kept` does not take is forgotten, the journal rewritten without them, and the
    /// tables handed back without them where a stop wrote them or an older build did
    /// ([`StateDir::hand_asks_to_files`]), so no build reads an offset of the old log as one of
    /// the new. The rewrite comes first and stands alone: a reset that leaves the journal
    /// holding nothing, in a dir with no table, hands nothing back, and the journal on disk
    /// must still lose the old log's offsets.
    fn forget_asks_where(&self, kept: impl Fn(u64) -> bool) -> io::Result<()> {
        {
            let mut asks = self.asks();
            if let Some(index) = asks.index.as_mut() {
                let before = index.clone();
                index.retain(kept);
                if *index != before {
                    asks.dirty = true;
                }
            }
            self.write_owed_asks(&mut asks)?;
        }
        self.hand_asks_to_files()
    }

    /// Owed receipt `rid` of `sid`'s sequence reservation, as its pin file holds it: with no head
    /// ([`PublishPin::render`]), which is kept beside it ([`StateDir::receipt_head`]).
    pub(crate) fn receipt_pin(&self, sid: &str, rid: u64) -> io::Result<Option<PublishPin>> {
        self.read_reservation(&format!("acks/{sid}.{rid}"))?
            .map(|s| PublishPin::parse(&s))
            .transpose()
    }

    /// The head kept beside owed receipt `rid` of `sid`'s pin ([`ACK_HEADS`]), for its pin at
    /// sequence `seq`: the log it was written on and its offset there, which the bridge reads on
    /// the log it reads now as an in-flight entry's (`Bridge::read_back_from`). `None` for no
    /// head, one that does not read whole, and one written for another sequence: a build
    /// before the heads neither moves nor removes the file, so it can outlive its pin, and name
    /// an offset of a log that build has since moved past.
    pub(crate) fn receipt_head(
        &self,
        sid: &str,
        rid: u64,
        seq: u64,
    ) -> io::Result<Option<(astream_broker::LogIdentity, u64)>> {
        Ok(self
            .read_reservation(&format!("{ACK_HEADS}/{sid}.{rid}"))?
            .as_deref()
            .and_then(parse_head)
            .and_then(|(of, log, at)| (of == seq).then_some((log, at))))
    }

    /// Reserve `pin` for owed receipt `rid` of `sid`'s, before it is published: its head first,
    /// where it names one and the log it is an offset of (`log`), beside the pin file
    /// ([`ACK_HEADS`]), then the pin. A stop between them leaves a head with no pin, or one
    /// written for the pin before, each naming a sequence the pin file does not, so read as no
    /// head ([`StateDir::receipt_head`]).
    pub(crate) fn set_receipt_pin(
        &self,
        sid: &str,
        rid: u64,
        pin: &PublishPin,
        log: Option<astream_broker::LogIdentity>,
    ) -> io::Result<()> {
        let head = format!("{ACK_HEADS}/{sid}.{rid}");
        match (pin.at, log) {
            _ if !self.keeps_receipt_heads() => {}
            (Some(at), Some(log)) => self.write(&head, &format!("{} {log} {at}", pin.seq))?,
            _ => self.remove_head(&head)?,
        }
        self.write(&format!("acks/{sid}.{rid}"), &pin.render())
    }

    /// Unlink a receipt's head file ([`ACK_HEADS`]) where it is there.
    fn remove_head(&self, rel: &str) -> io::Result<()> {
        match self.remove_path(&self.path(rel)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    pub(crate) fn key_pin(&self, sid: &str, key: &str) -> io::Result<Option<PublishPin>> {
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        text.lines()
            .find(|line| line.split_whitespace().next() == Some(key))
            .map(|line| {
                let rest = line.strip_prefix(key).unwrap_or_default().trim();
                PublishPin::parse(rest)
            })
            .transpose()
    }

    pub(crate) fn set_key_pin(
        &self,
        sid: &str,
        key: &str,
        pin: &PublishPin,
    ) -> io::Result<PublishPin> {
        if let Some(have) = self.key_pin(sid, key)? {
            return Ok(have);
        }
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines.push(format!("{key} {}", pin.render()));
        let over = lines.len().saturating_sub(KEYS_KEEP);
        self.write(&format!("keys/{sid}"), &lines[over..].join("\n"))?;
        Ok(pin.clone())
    }

    /// WHERE AN ATTACH'S REPLAY LANDED A KEY'S WINNER (ATERM-KI-1111-4): the key of `sid`'s
    /// whose pin is `seq` and names `identity` its winner, and whose winner's offset the new log
    /// does not vouch for (a move past the fork, or another log, took it out:
    /// [`StateDir::publication_offset_resets`]), is told the replay's `off`. The key's next
    /// re-post reads its winner back there (`Bridge::keyed_winner_at`): past a cut that carried
    /// the record and dropped its dedup key, a re-post published at the pin unread lands a
    /// second copy of its key (the lossy-log sim's seed 216 of the keyed world). A key another
    /// post won since, or one whose offset stands, is left as it is.
    ///
    /// # Errors
    /// Reading or rewriting `keys/<sid>`.
    pub(crate) fn land_key_winner(
        &self,
        sid: &str,
        seq: u64,
        identity: &PublishIdentity,
        off: u64,
    ) -> io::Result<bool> {
        let Some(text) = self.read_reservation(&format!("keys/{sid}"))? else {
            return Ok(false);
        };
        let mut changed = false;
        let mut lines = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                lines.push(line.to_string());
                continue;
            }
            let (key, mut pin) = key_line(line)?;
            if !changed
                && pin.seq == seq
                && pin.identity.as_ref() == Some(identity)
                && pin.original_off.is_none()
            {
                pin.original_off = Some(off);
                changed = true;
                lines.push(format!("{key} {}", pin.render()));
            } else {
                lines.push(line.to_string());
            }
        }
        if changed {
            self.write(&format!("keys/{sid}"), &lines.join("\n"))?;
        }
        Ok(changed)
    }

    /// Reconcile a retained key to the bytes actually found/appended, before retiring the post.
    pub(crate) fn reconcile_key(
        &self,
        sid: &str,
        key: &str,
        seq: u64,
        identity: &PublishIdentity,
        off: u64,
        owner: Option<u64>,
    ) -> io::Result<()> {
        let have = match self.key_pin(sid, key)? {
            Some(have) if have.seq == seq => have,
            _ => return Ok(()),
        };
        let owner = owner.or(have.owner);
        if have.identity.as_ref() == Some(identity)
            && have.original_off == Some(off)
            && have.owner == owner
        {
            return Ok(());
        }
        let next = PublishPin {
            seq,
            via_key: false,
            identity: Some(identity.clone()),
            original: None,
            original_off: Some(off),
            owner,
            at: have.at,
        };
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        let lines: Vec<String> = text
            .lines()
            .map(|line| {
                if line.split_whitespace().next() == Some(key) {
                    format!("{key} {}", next.render())
                } else {
                    line.to_string()
                }
            })
            .collect();
        self.write(&format!("keys/{sid}"), &lines.join("\n"))
    }

    /// What a moved generation resets in the reservations: each `original-off` the new log does
    /// not vouch for (at or above `keep_below`, or any where it is `None`), taken out; and each
    /// head ([`PublishPin::at`]; not a receipt's, kept with its log, [`ACK_HEADS`]) past where the new log continues the old, brought down to it,
    /// brought to the log's start on `another_log`, and taken out where the log continues none
    /// and is not another (aterm-link B05 review; ATERM-KI-1111-4). A head is an offset of the
    /// log the pin was made on, and past the fork the new log's offsets name records published
    /// since, a copy a retry published there among them, so its read back starts at the fork.
    /// ANOTHER LOG is one under another id, told with its lineage ([`another_log`]): every
    /// record on it landed after the pin was made, so its read back starts at 0. Taken out
    /// there, the retry read nothing back, and an attempt the attach's replay published on the
    /// new log, its answer unknown (a poisoned broker's), its dedup key cut, was published a
    /// second time (the lossy-log sim's seed 2201). A log that keeps its id and continues
    /// nothing (a restored or shortened copy, a move older than the lineage's window) or whose
    /// lineage is unknown may still hold records from before the pin, and a read back from 0
    /// would settle the pin on an earlier identical record and never publish it, so its head is
    /// taken out and the retry publishes unread, as before.
    fn publication_offset_resets(
        &self,
        keep_below: Option<u64>,
        another_log: bool,
    ) -> io::Result<OffsetResets> {
        #[cfg(test)]
        let head_gone = self
            .pin_head_gone_on_another_log
            .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let head_gone = false;
        let fork = keep_below.or((another_log && !head_gone).then_some(0));
        let reset = |pin: &mut PublishPin| -> bool {
            let stale = pin
                .original_off
                .is_some_and(|off| keep_below.is_none_or(|f| off >= f));
            let off_reset = stale && pin.original_off.take().is_some();
            let past = pin.at.is_some_and(|at| fork.is_none_or(|f| at > f));
            if past {
                pin.at = fork;
            }
            off_reset || past
        };
        let mut rewrites = Vec::new();
        let path = self.path("keys");
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries.collect::<io::Result<Vec<_>>>()?,
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(&path)
                        .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
            {
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid publication filename")
            })?;
            if name.starts_with('.') {
                continue;
            }
            if !entry.file_type()?.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "publication reservation is not a regular file",
                ));
            }
            let text = crate::enable::read_bounded_string(&entry.path(), 64 << 20)?;
            let mut changed = false;
            let mut lines = Vec::new();
            for line in text.lines() {
                if line.trim().is_empty() {
                    lines.push(line.to_string());
                    continue;
                }
                let (key, mut pin) = key_line(line)?;
                if reset(&mut pin) {
                    changed = true;
                    lines.push(format!("{key} {}", pin.render()));
                } else {
                    lines.push(line.to_string());
                }
            }
            if changed {
                rewrites.push((format!("keys/{name}"), lines.join("\n")));
            }
        }
        let mut posts = Vec::new();
        for (sid, id, pin) in self.posts()?.index().pins() {
            let mut pin = pin.clone();
            if reset(&mut pin) {
                posts.push((sid.to_string(), id, pin));
            }
        }
        Ok(OffsetResets {
            keys: rewrites,
            posts,
        })
    }

    fn write(&self, name: &str, value: &str) -> io::Result<()> {
        self.replace(name, value, false, STATE_FILE_MODE)
    }

    fn write_durable(&self, name: &str, value: &str) -> io::Result<()> {
        self.replace(name, value, true, STATE_FILE_MODE)
    }

    /// Whether what reads at `name` is on the media as far as this open knows.
    pub(crate) fn on_the_media(&self, name: &str) -> bool {
        !self
            .undurable
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(name)
    }

    fn replace(&self, name: &str, value: &str, durable: bool, mode: u32) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(value.len() + 1);
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(b'\n');
        self.replace_bytes(name, &bytes, durable, mode)
    }

    fn replace_bytes(&self, name: &str, bytes: &[u8], durable: bool, mode: u32) -> io::Result<()> {
        let flush = durable.then_some(aterm_durable::Flush::Durable);
        self.replace_bytes_as(name, bytes, flush, mode)
    }

    fn replace_bytes_as(
        &self,
        name: &str,
        bytes: &[u8],
        flush: Option<aterm_durable::Flush>,
        mode: u32,
    ) -> io::Result<()> {
        self.replace_bytes_as_with(name, bytes, flush, mode, tolerate_dir_flush)
    }

    fn replace_bytes_as_with(
        &self,
        name: &str,
        bytes: &[u8],
        flush: Option<aterm_durable::Flush>,
        mode: u32,
        finish_dir: fn(io::Result<()>) -> io::Result<()>,
    ) -> io::Result<()> {
        self.publish(name, bytes, flush, mode, finish_dir).map(drop)
    }

    /// [`StateDir::replace_bytes_as_with`], answering the identity of the file it published as its
    /// rename left it, where that reads ([`StateDir::prove`]): a rename itself moves a file's
    /// change time, on APFS as on ext4.
    fn publish(
        &self,
        name: &str,
        bytes: &[u8],
        flush: Option<aterm_durable::Flush>,
        mode: u32,
        finish_dir: fn(io::Result<()>) -> io::Result<()>,
    ) -> io::Result<Option<Identity>> {
        #[cfg(test)]
        self.refused_by_a_test(name)?;
        let target = self.path(name);
        let parent = target.parent().unwrap_or(&self.root);
        let w = crate::fx::Write::Replace {
            path: &target,
            bytes,
            flush,
            mode,
        };
        let _timed = self.timed(crate::wait_tally::Row::DirReplace);
        self.disk.before(w)?;
        std::fs::create_dir_all(parent)?;
        let (tmp, mut f) = crate::enable::create_temp_beside(&target, mode)?;
        let durable = flush == Some(aterm_durable::Flush::Durable);
        self.count(|ops| {
            ops.replaces += 1;
            ops.renames += 1;
            ops.fsyncs += 2 * u64::from(durable);
            ops.barriers += u64::from(flush == Some(aterm_durable::Flush::Ordered));
        });
        let staged = (|| -> io::Result<_> {
            f.write_all(bytes)?;
            if let Some(flush) = flush.filter(|_| flushes_media()) {
                flush.file(&f)?;
            }
            std::fs::rename(&tmp, &target)?;
            Ok(identity_of(&f))
        })();
        drop(f);
        let made = match staged {
            Ok(made) => made,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        #[cfg(test)]
        if let Some(made) = made.filter(|_| self.proof_rule() == ProofRule::AtTheRename) {
            self.prove(name, made);
        }
        let finished = (|| {
            if durable {
                self.kill_point(IncStep::Renamed)?;
                finish_dir(self.directory_flush_result(parent, aterm_durable::Flush::Durable))?;
            }
            self.disk.after(w)
        })();
        if durable {
            let mut undurable = self.undurable.lock().unwrap_or_else(|e| e.into_inner());
            if finished.is_ok() {
                undurable.remove(name);
            } else {
                undurable.insert(name.to_string());
            }
        }
        if self.a_clean_stop_flushes(name) {
            let mut unflushed = self.unflushed.lock().unwrap_or_else(|e| e.into_inner());
            if durable && finished.is_ok() {
                unflushed.remove(name);
            } else {
                unflushed.insert(name.to_string());
            }
        }
        finished.map(|()| made)
    }

    /// Whether the clean stop's record vouches for `name` ([`StateDir::end_run_clean`]): a file of
    /// a [`TornKind`], the mirror's pin, and the heads a stop's hand-off kept ([`journal::HANDED`]).
    /// Those heads are no torn kind, since a torn copy costs no sweep, only its heads
    /// ([`StateDir::read_handed`]); but unflushed under a clean record, a power cut took them and
    /// kept the hand-off's files, and the next bridge published each record in flight again at
    /// its pin unread, twice past a cut that dropped its key (the control:
    /// `Mutant::StopLeavesTheHeadsUnflushed`).
    fn a_clean_stop_flushes(&self, name: &str) -> bool {
        #[cfg(test)]
        if name == journal::HANDED
            && self
                .stop_leaves_the_heads_unflushed
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return false;
        }
        TornKind::of(name).is_some() || name == LOGPIN || name == journal::HANDED
    }

    /// The bridge lock's file, opened (created) for one take: a regular file, never followed
    /// through a link, and opened without waiting on whatever else the name might hold.
    fn open_bridge_lock(&self) -> io::Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.path("lock");
        let file = aterm_uds::kept_out(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
                .open(&path)
        })?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the bridge lock {} is not a regular file", path.display()),
            ));
        }
        Ok(file)
    }

    /// [`StateDir::lock_bridge`]'s lock, opened on a descriptor of its own for a wait in the
    /// kernel's queue ([`BridgeLockWait::wait`]): what a bridge that found the dir held stands
    /// by on.
    pub(crate) fn bridge_lock_wait(&self) -> io::Result<BridgeLockWait> {
        self.open_bridge_lock().map(BridgeLockWait)
    }

    /// Take this state dir for one bridge's life.
    pub fn lock_bridge(&self) -> io::Result<BridgeLock> {
        let file = self.open_bridge_lock()?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: BridgeLock's Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(BridgeLock(file)),
            Err(std::fs::TryLockError::Error(e)) if e.kind() == io::ErrorKind::Unsupported => {
                Ok(BridgeLock(file))
            }
            Err(std::fs::TryLockError::Error(e)) => Err(e),
            Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "the state dir {} is held by another bridge{}",
                    self.root.display(),
                    self.holder()
                        .map_or_else(String::new, |pid| format!(" (pid {pid})"))
                ),
            )),
        }
    }

    /// The pid the `pid` file names.
    #[must_use]
    pub fn holder(&self) -> Option<u32> {
        self.read("pid").and_then(|s| crate::parse_uint(&s))
    }

    /// This node's id, minted once and kept ([`StateDir::provision_node`]).
    pub fn node_id(&self, mint: impl FnOnce() -> String) -> io::Result<String> {
        self.provision_node(mint).map(|(id, _)| id)
    }

    /// This node's id, minted once and kept, and where a `node` file that held anything but a node
    /// id went: [`keep_aside`] keeps a non-empty one beside the new one, never overwritten, and
    /// takes that link back ([`take_back_aside`]) when the new id's write fails before it replaced
    /// the file.
    pub(crate) fn provision_node(
        &self,
        mint: impl FnOnce() -> String,
    ) -> io::Result<(String, Option<PathBuf>)> {
        if let Some(id) = self.node().filter(|id| crate::enable::is_node_id(id)) {
            return Ok((id, None));
        }
        let path = self.path("node");
        let aside = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Ok(held) if held.len() == 0 => None,
            _ => Some(keep_aside(&path)?),
        };
        let id = mint();
        if let Err(e) = self.write_durable("node", &id) {
            if let Some(aside) = &aside {
                take_back_aside(&path, aside);
            }
            return Err(e);
        }
        sync_dir(self.root.parent().unwrap_or(Path::new("")))?;
        Ok((id, aside))
    }

    /// Record `id`, which this Mac's node key names with the mesh on (§11 C3), as this dir's node,
    /// durably, and answer the node it recorded before when that was another: the key rotated, or
    /// this dir last served before the mesh. A `node` file that held anything but a node id is kept
    /// aside first, as [`StateDir::provision_node`] keeps it.
    ///
    /// # Errors
    /// The file cannot be kept aside, written or synced.
    pub fn name_node(&self, id: &str) -> io::Result<Option<String>> {
        let held = self.node();
        if held.as_deref() == Some(id) {
            return Ok(None);
        }
        let path = self.path("node");
        let aside = match std::fs::symlink_metadata(&path) {
            Ok(meta)
                if meta.len() > 0 && !held.as_deref().is_some_and(crate::enable::is_node_id) =>
            {
                Some(keep_aside(&path)?)
            }
            _ => None,
        };
        if let Err(e) = self.write_durable("node", id) {
            if let Some(aside) = &aside {
                take_back_aside(&path, aside);
            }
            return Err(e);
        }
        sync_dir(self.root.parent().unwrap_or(Path::new("")))?;
        Ok(held.filter(|old| crate::enable::is_node_id(old)))
    }

    /// The node id this dir records, if it records one.
    #[must_use]
    pub fn node(&self) -> Option<String> {
        self.read("node")
    }

    /// [`Self::node`] for a reader that must not take a file it could not read for one that is not
    /// there (the own-inbox move's gate, §11 C3b): `Err` where `node` exists and cannot be read.
    pub(crate) fn node_present(&self) -> io::Result<Option<String>> {
        self.read_present("node")
    }

    /// The incarnation this node last used, as far as the local disk knows.
    #[must_use]
    pub fn incarnation(&self) -> u64 {
        let used = self.lease().map_or(0, |lease| lease.used);
        used.max(incarnation_of_seq(self.sequence()))
    }

    fn incarnation_top(&self) -> u64 {
        self.read("inc")
            .and_then(|s| crate::parse_uint(&s))
            .unwrap_or(0)
    }

    /// What `inc-lease` holds, if it holds a whole one ([`IncLease::parse`]).
    pub(crate) fn lease(&self) -> Option<IncLease> {
        self.read("inc-lease").and_then(|s| IncLease::parse(&s))
    }

    /// Which `inc` this dir holds now ([`IncFile`]): `None` unless it is a regular file on the
    /// dir's own device.
    pub(crate) fn inc_file(&self) -> Option<IncFile> {
        use std::os::unix::fs::MetadataExt;
        let dir = std::fs::metadata(&self.root).ok()?;
        let inc = std::fs::symlink_metadata(self.path("inc")).ok()?;
        (inc.is_file() && inc.dev() == dir.dev()).then(|| IncFile {
            dev: dir.dev(),
            dir: dir.ino(),
            ino: inc.ino(),
        })
    }

    /// The mark this state dir's bridges put on every presence row of a session they write
    /// ([`PRESENCE_MARK`]).
    #[must_use]
    pub(crate) fn mark(&self, node: &str) -> Option<String> {
        use std::os::unix::fs::MetadataExt;
        let boot = self.kernel_boot()?;
        let dir = std::fs::metadata(&self.root).ok()?;
        let named = format!(
            "aterm-link boot state dir {} {node} {}:{}",
            aterm_codec::hex::encode_lower(&boot),
            dir.dev(),
            dir.ino()
        );
        Some(format!("{:016x}", aterm_hash::fnv1a_64(named.as_bytes())))
    }

    /// This boot, when the page cache can vouch for this state dir: the kernel names it and the
    /// dir is on the boot volume ([`on_boot_volume`]). Off it a removable or network disk can
    /// vanish while the kernel runs and take an unflushed `inc-lease` with it, so no lease is
    /// trusted there (e4e7f00368). That line is the reader's, never the writer's: a lease names
    /// the boot it was written in on every volume ([`IncLease::boot`]), and one written off the
    /// boot volume is never trusted on it either, as the `inc` it names is on another device.
    /// The model's `Plan` trusts by it (`IncarnationLease`, `OffVolume`), and its `Buggy = 5`,
    /// a lease trusted by the boot it names, presents an incarnation twice after a vanish.
    pub(crate) fn trusted_boot(&self) -> Option<[u8; 16]> {
        #[cfg(test)]
        if self.inc_seam().mutant == 5 {
            return self.kernel_boot();
        }
        self.kernel_boot().filter(|_| self.on_the_boot_volume())
    }

    /// Whether this dir is on the boot volume ([`on_boot_volume`] of [`StateDir::volume_probe`]).
    fn on_the_boot_volume(&self) -> bool {
        on_boot_volume(self.volume_probe())
    }

    /// The dir the volume probe opens: this dir, which a test with no seam judges both ways by
    /// pointing a linked state dir at `/` and at `/dev`. A test's seam picks another dir whose
    /// answer is known, never the answer itself, so every test runs the shipped probe: `/dev` off
    /// the boot volume (`IncSeam::off_volume`), and `/` where it stands in the boot on it, as a
    /// scratch dir can sit on a separate `/tmp` device.
    fn volume_probe(&self) -> &Path {
        #[cfg(test)]
        {
            let seam = self.inc_seam();
            if seam.off_volume {
                return Path::new("/dev");
            }
            if seam.boot.is_some() {
                return Path::new("/");
            }
        }
        &self.root
    }

    /// This boot, as the kernel names it (`astream_sys::boot_session`: macOS's
    /// `kern.bootsessionuuid`, Linux's `boot_id`), on any volume.
    fn kernel_boot(&self) -> Option<[u8; 16]> {
        #[cfg(test)]
        if let Some(boot) = self.inc_seam().boot {
            return boot;
        }
        crate::durability::this_boot()
    }

    /// Take the incarnation an attach runs under, before its will is registered: named in the post
    /// journal before the lease says it ([`StateDir::name_attach_in_posts`]). Where it writes `inc`
    /// in full (the boot's first incarnation, or a lease spent), that flush covers the OPEN this
    /// open admitted, and a proof is made of it ([`StateDir::begin_run`], [`RUN_PROOF`]). The
    /// lease trusts and spans by [`StateDir::trusted_boot`] and names the kernel's boot on every
    /// volume, so a dir off the boot volume can tell an earlier boot's attach from this boot's
    /// ([`StateDir::last_ran_in_an_earlier_boot`]).
    pub fn take_incarnation(&self, bus: u64) -> io::Result<u64> {
        let (top, file) = (self.incarnation_top(), self.inc_file());
        let boot = self.trusted_boot();
        let named = self.kernel_boot();
        #[cfg(test)]
        let named = if self
            .lease_names_no_boot_off_the_boot_volume
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            boot
        } else {
            named
        };
        let (lease, seq_inc, span) = (self.lease(), incarnation_of_seq(self.sequence()), INC_LEASE);
        #[cfg(test)]
        let (lease, span) = {
            let seam = self.inc_seam();
            let lease = match seam.mutant {
                1 => lease.map(|l| IncLease { boot, ..l }),
                2 => lease.map(|l| IncLease { top, ..l }),
                _ => lease,
            };
            (lease, seam.span.unwrap_or(span))
        };
        let plan =
            plan_incarnation(top, file, lease, seq_inc, boot, bus, span).ok_or_else(|| {
                io::Error::other(format!(
                    "this node's incarnation space is exhausted (inc {} of {INC_MAX})",
                    top.max(seq_inc).max(bus)
                ))
            })?;
        self.name_attach_in_posts(plan.next)?;
        let leased = |file| IncLease {
            used: plan.next,
            top: plan.top,
            boot: named,
            file,
        };
        #[cfg(test)]
        if plan.flush && self.inc_seam().mutant == 3 {
            self.write("inc-lease", &leased(file).render())?;
        }
        let file = if plan.flush {
            self.write_durable("inc", &plan.top.to_string())?;
            self.kill_point(IncStep::Flushed)?;
            self.prove_the_admitted_run();
            self.inc_file()
        } else {
            file
        };
        self.write("inc-lease", &leased(file).render())?;
        Ok(plan.next)
    }

    /// The highest producer sequence this bridge has reserved (not necessarily published).
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.read("seq")
            .and_then(|s| crate::parse_uint(&s))
            .unwrap_or(0)
    }

    /// Reserve the next producer sequence of incarnation `inc` and persist it before it is used.
    pub fn reserve_next_sequence(&self, inc: u64) -> io::Result<u64> {
        self.reserve_sequence_block(inc, 1)
            .map(|block| *block.start())
    }

    /// Reserve a block of up to `want` producer sequences of incarnation `inc`.
    pub fn reserve_sequence_block(
        &self,
        inc: u64,
        want: u64,
    ) -> io::Result<std::ops::RangeInclusive<u64>> {
        let (Some(base), Some(top)) = (
            astream_broker::incarnation_base(inc),
            astream_broker::will_seq(inc),
        ) else {
            return Err(io::Error::other(format!(
                "incarnation {inc} is past this node's incarnation space ({INC_MAX})"
            )));
        };
        let first = self
            .sequence()
            .max(base)
            .checked_add(1)
            .filter(|n| *n < top)
            .ok_or_else(|| io::Error::other("this incarnation's sequence space is exhausted"))?;
        let last = first.saturating_add(want.max(1) - 1).min(top - 1);
        self.write("seq", &last.to_string())?;
        Ok(first..=last)
    }

    /// Where one session's addressed refill starts after.
    #[must_use]
    pub fn seen_off(&self, sid: &str) -> Option<u64> {
        self.read(&format!("seen/{sid}"))
            .and_then(|s| crate::parse_uint(&s))
    }

    /// Record a session's refill watermark ([`Self::seen_off`]).
    pub fn set_seen_off(&self, sid: &str, off: u64) -> io::Result<()> {
        if self.seen_off(sid).is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write(&format!("seen/{sid}"), &off.to_string())
    }

    /// Every offset this bridge holds a `PublishAck` for on its own lanes: a test's read of
    /// the file; the bridge reads it whole ([`StateDir::self_acked_log`]).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn self_acked(&self) -> std::collections::BTreeSet<u64> {
        self.self_acked_log().offsets
    }

    /// The `acked` file as it stands.
    pub(crate) fn self_acked_log(&self) -> AckedLog {
        match crate::enable::read_bounded_string(&self.path("acked"), 64 << 20) {
            Ok(text) => AckedLog::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => AckedLog {
                appendable: true,
                ..AckedLog::default()
            },
            Err(_) => AckedLog::default(),
        }
    }

    /// Replace the acked self-lane set with the newest `keep` of `set`, under the window floor
    /// `floor` ([`AckedLog::floor`]), raised to the oldest kept where `keep` drops any. The
    /// descriptor the appends held goes first, under its lock, so no append lands in the file
    /// the rename unlinks: the next one opens the file this puts at the name.
    pub fn set_self_acked(
        &self,
        set: &std::collections::BTreeSet<u64>,
        floor: u64,
        keep: usize,
    ) -> io::Result<()> {
        let mut held = self.acked.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(test)]
        let keeps = self
            .keep_acked_across_its_replace
            .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let keeps = false;
        if !keeps {
            *held = None;
        }
        let over = set.len().saturating_sub(keep);
        let kept = set.iter().skip(over).copied();
        let floor = match set.iter().nth(over) {
            Some(&oldest) if over > 0 => floor.max(oldest),
            _ => floor,
        };
        self.replace_bytes(
            "acked",
            AckedLog::render(Some(floor), kept).as_bytes(),
            false,
            STATE_FILE_MODE,
        )
    }

    /// Append `offs` to the acked self-lane log, each ended by its comma, in one `write` to the
    /// file held open `O_APPEND` ([`StateDir::acked`]): an ack pass's landed posts are one append
    /// (critic item E07). It is opened at the first append, after a
    /// replace ([`StateDir::set_self_acked`]) and after an append whose write failed (refused
    /// before it was made, made and failed, or cut short), and refused there unless it is an
    /// owned regular single-link file ([`StateDir::open_for_appends`]). `floor`, where it names
    /// one, goes ahead of them: the window floor of a file that names none and holds no offset
    /// ([`AckedLog::floor`]).
    pub(crate) fn append_self_acked(&self, floor: Option<u64>, offs: &[u64]) -> io::Result<()> {
        let _call = self.timed(crate::wait_tally::Row::AppendSelfAcked);
        self.count(|ops| ops.appends += 1);
        let path = self.path("acked");
        let line = AckedLog::render(floor, offs.iter().copied());
        let w = crate::fx::Write::Append {
            path: &path,
            bytes: line.as_bytes(),
        };
        let _timed = self.timed(crate::wait_tally::Row::DirAppend);
        let mut held = self.acked.lock().unwrap_or_else(|e| e.into_inner());
        // Out of the lock's slot until the append is made, taken before the seams that refuse
        // the write: an append that fails, a refused one too, leaves none held.
        let kept = held.take();
        #[cfg(test)]
        self.refused_by_a_test("acked")?;
        self.disk.before(w)?;
        let mut file = match kept {
            Some(file) => file,
            None => self.open_for_appends(&path, "acked log")?.0,
        };
        file.write_all(line.as_bytes())?;
        self.disk.after(w)?;
        *held = Some(file);
        Ok(())
    }

    /// Every record in flight onto this node's own lanes, by sequence, as the journal holds them.
    ///
    /// # Errors
    /// The journal unreadable or damaged, or an older build's `inflight/<seq>` that does not read
    /// and is still there (`InvalidData` naming it, as that build refused it: a record whose fate
    /// it holds is not read as absent).
    pub(crate) fn in_flight(&self) -> io::Result<Vec<(u64, InFlight)>> {
        let mut posts = self.posts()?;
        self.refuse_a_held_in_flight(&mut posts)?;
        Ok(posts
            .index()
            .in_flight
            .iter()
            .map(|(seq, entry)| (*seq, entry.clone()))
            .collect())
    }

    /// An older build's `inflight/<seq>` the journal could not take in, while it is still there.
    fn refuse_a_held_in_flight(&self, posts: &mut Posts) -> io::Result<()> {
        let Some(path) = posts.held_in_flight.clone() else {
            return Ok(());
        };
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                posts.held_in_flight = None;
                posts.adopted = false;
                Ok(())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{path}: malformed in-flight entry"),
            )),
        }
    }

    /// WHAT THE NODE STILL OWES A PUBLICATION: the sessions with a post reserved in the journal
    /// or a receipt reserved (`acks/<sid>.<rid>`), and every subject such a reservation or an
    /// in-flight record names. A departed session's subjects are given back to the subject
    /// budget only once none of this names them (`Bridge::reclaim_subjects`). A reservation that
    /// does not parse (a receipt's, or an older build's `sent/<sid>.<id>` the journal held where
    /// it is) still names its session. What a session of an earlier boot left reserved, which no
    /// drain retires, goes at the first start of a later boot that finds the last bridge ran in
    /// an earlier one ([`StateDir::sweep_sessions_of_an_earlier_boot`]): the next boot's, but
    /// a reservation whose sequence a record in flight still holds there waits for a later
    /// boot's sweep, and a start whose first roster does not read, or that comes after an
    /// attach of its own boot, leaves it to a later boot's too. So does a start over a lease an
    /// older build wrote off the boot volume, which names no boot: the sweep waits for a boot
    /// after one in which this build attached and its lease named that boot.
    ///
    /// # Errors
    /// A directory or an entry that cannot be read, the journal damaged, or an older build's
    /// in-flight entry that does not read ([`StateDir::in_flight`]).
    pub(crate) fn owed_publications(&self) -> io::Result<Owed> {
        let mut owed = Owed::default();
        let name_pin = |owed: &mut Owed, pin: Option<PublishPin>| {
            for identity in pin.into_iter().flat_map(|p| [p.identity, p.original]) {
                owed.subjects.extend(identity.map(|i| i.subject));
            }
        };
        for (name, rel) in self.kind_files(TornKind::Acks)? {
            if let Some((sid, _)) = name.rsplit_once('.') {
                owed.sids.insert(sid.to_string());
            }
            let pin = self
                .read_reservation(&rel)?
                .and_then(|text| PublishPin::parse(&text).ok());
            name_pin(&mut owed, pin);
        }
        let mut posts = self.posts()?;
        self.refuse_a_held_in_flight(&mut posts)?;
        for (sid, _, pin) in posts.index().pins() {
            owed.sids.insert(sid.to_string());
            name_pin(&mut owed, Some(pin.clone()));
        }
        for name in &posts.held_sent {
            if let Some((sid, _)) = name.rsplit_once('.') {
                owed.sids.insert(sid.to_string());
            }
        }
        owed.subjects.extend(
            posts
                .index()
                .in_flight
                .values()
                .map(|entry| entry.subject.clone()),
        );
        Ok(owed)
    }

    /// Record one own-lane record as in flight, before it is published: one journal record.
    pub(crate) fn set_in_flight(&self, entry: &InFlight) -> io::Result<()> {
        let _timed = self.timed(crate::wait_tally::Row::SetInFlight);
        let mut posts = self.posts()?;
        self.append_post_frame(&mut posts, journal::Frame::InFlight(entry.clone()))
    }

    /// Forget the in-flight record at `seq`, once it landed: one journal record, none where the
    /// journal holds no record at `seq`.
    pub(crate) fn clear_in_flight(&self, seq: u64) -> io::Result<()> {
        let _timed = self.timed(crate::wait_tally::Row::ClearInFlight);
        let mut posts = self.posts()?;
        if !posts.index().in_flight.contains_key(&seq) {
            return Ok(());
        }
        self.append_post_frame(&mut posts, journal::Frame::Landed(seq))
    }

    /// Drop the in-flight record at `seq` for good: its record, then the journal flushed in full.
    /// The open forgets it even where its record cannot be written, as an older build's unlink of
    /// its file did on a disk too full to write ([`StateDir::append_post_frame`]), so no later
    /// attach of this open publishes it; the flush then rewrites the journal without it, and the
    /// drop is on the media once either is.
    ///
    /// # Errors
    /// Neither the record nor the rewrite on the media: the open has forgotten the record, and a
    /// stop or crash before the journal's next write leaves it in the file.
    pub(crate) fn drop_in_flight(&self, seq: u64) -> io::Result<()> {
        let mut posts = self.posts()?;
        let appended = if posts.index().in_flight.contains_key(&seq) {
            self.append_post_frame(&mut posts, journal::Frame::Dropped(seq))
        } else {
            Ok(())
        };
        match self.sync_posts(&mut posts, aterm_durable::Flush::Durable) {
            Ok(()) => Ok(()),
            Err(e) => Err(appended.err().unwrap_or(e)),
        }
    }

    fn flush_dir(&self, dir: &Path) -> io::Result<()> {
        self.flush_dir_with(dir, aterm_durable::Flush::Durable, tolerate_dir_flush)
    }

    fn flush_in_place(&self, rel: &str, flush: aterm_durable::Flush) -> io::Result<bool> {
        use std::os::unix::fs::OpenOptionsExt as _;
        #[cfg(test)]
        self.refused_by_a_test(rel)?;
        let path = self.path(rel);
        let w = crate::fx::Write::Flush { path: &path, flush };
        let _timed = self.timed(crate::wait_tally::Row::DirFlush);
        self.disk.before(w)?;
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        if !file.metadata()?.is_file() {
            return Ok(false);
        }
        self.flush_opened(&file, flush)?;
        self.disk.after(w)?;
        Ok(true)
    }

    fn flush_opened(&self, file: &std::fs::File, flush: aterm_durable::Flush) -> io::Result<()> {
        self.count(|ops| match flush {
            aterm_durable::Flush::Durable => ops.fsyncs += 1,
            aterm_durable::Flush::Ordered => ops.barriers += 1,
        });
        if flushes_media() {
            flush.file(file)?;
        }
        Ok(())
    }

    fn flush_dir_as(&self, dir: &Path, flush: aterm_durable::Flush) -> io::Result<()> {
        self.flush_dir_with(dir, flush, std::convert::identity)
    }

    fn flush_dir_with(
        &self,
        dir: &Path,
        flush: aterm_durable::Flush,
        finish: fn(io::Result<()>) -> io::Result<()>,
    ) -> io::Result<()> {
        let w = crate::fx::Write::SyncDir { path: dir, flush };
        let _timed = self.timed(crate::wait_tally::Row::DirFlush);
        self.disk.before(w)?;
        self.count(|ops| match flush {
            aterm_durable::Flush::Durable => ops.fsyncs += 1,
            aterm_durable::Flush::Ordered => ops.barriers += 1,
        });
        finish(self.directory_flush_result(dir, flush))?;
        self.disk.after(w)?;
        Ok(())
    }

    fn directory_flush_result(&self, dir: &Path, flush: aterm_durable::Flush) -> io::Result<()> {
        #[cfg(test)]
        if let Some((path, tier, kind)) = &*self
            .dir_flush_refusal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            if path == dir && *tier == flush {
                return Err(io::Error::new(*kind, "a test's directory flush refusal"));
            }
        }
        sync_dir_required_as(dir, flush)
    }

    /// How the last bridge of this dir stopped, and in which boot (`run`, [`LastRun`]).
    pub(crate) fn last_run(&self) -> io::Result<Option<LastRun>> {
        self.run_file().map(|record| record.map(|(_, run)| run))
    }

    fn run_file(&self) -> io::Result<Option<(std::fs::File, LastRun)>> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = self.path("run");
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return match std::fs::symlink_metadata(&path) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(other) => Err(other),
                    Ok(_) => Err(e),
                };
            }
            Err(e) => return Err(e),
        };
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "invalid bridge run record");
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(bad());
        }
        let mut text = String::with_capacity(
            usize::try_from(meta.len().min(128).saturating_add(1)).unwrap_or(0),
        );
        (&file).take(128).read_to_string(&mut text)?;
        if text.len() >= 128 {
            return Err(bad());
        }
        LastRun::parse(&text)
            .map(|run| Some((file, run)))
            .ok_or_else(bad)
    }

    /// Whether a power cut can have torn a file of a [`TornKind`] since the last bridge wrote one.
    pub(crate) fn torn_files_possible(&self) -> io::Result<bool> {
        let last = self.last_run()?;
        let Some(now) = self.kernel_boot() else {
            return Ok(false);
        };
        Ok(match last {
            Some(run) => !run.clean && run.boot.is_some_and(|then| then != now),
            None => self
                .lease()
                .and_then(|lease| lease.boot)
                .is_some_and(|then| then != now),
        })
    }

    /// Whether the last bridge of this dir ran in an earlier boot, however it stopped: the kernel
    /// names this boot, and the run record names another, or, where there is no record, the
    /// lease does ([`StateDir::torn_files_possible`]'s boot rule, a clean stop included). A
    /// record that names no boot, or does not read, is no earlier boot. AN ATTACH OF THIS BOOT
    /// WINS OVER THE RECORD: a build older than the record attaches without writing it, so where
    /// a lease reads it must name an earlier boot too. One that names this boot is that attach's
    /// (its sessions may still run, as [`StateDir::retire_in_flight_of_an_earlier_boot`] holds),
    /// and one that names no boot cannot rule it out: an older build's off the boot volume,
    /// where it named only a boot whose page cache vouched for the dir. This build's lease names
    /// the kernel's boot on every volume ([`IncLease::boot`]), so a dir on an external disk, or
    /// off Apple a `/home` on its own filesystem, finds an earlier boot as the boot volume does.
    /// Read before the start's [`StateDir::begin_run`] names this boot and before its own attach
    /// writes the lease ([`StateDir::sweep_sessions_of_an_earlier_boot`]).
    pub(crate) fn last_ran_in_an_earlier_boot(&self) -> bool {
        let Some(now) = self.kernel_boot() else {
            return false;
        };
        let earlier = |boot: Option<[u8; 16]>| boot.is_some_and(|then| then != now);
        let lease = self.lease();
        let then = match self.last_run() {
            Ok(Some(run)) => run.boot,
            Ok(None) => lease.and_then(|lease| lease.boot),
            Err(_) => None,
        };
        earlier(then) && lease.is_none_or(|lease| earlier(lease.boot))
    }

    /// AN EARLIER BOOT'S SESSIONS' FILES AND RESERVATIONS GO: every `keys/<sid>`,
    /// `acks/<sid>.<rid>`, `unhandled/<sid>`, `seen/<sid>` and `topics/<sid>` of a sid `live`
    /// does not name is unlinked, then each post reservation the journal holds for such a sid
    /// is retired ([`journal::Frame::Retire`]), and how many of both returned. aterm mints a
    /// new sid at every launch, a restore reopens a tab under a new one, and no successor keeps
    /// a sid across a reboot, so nothing reads the files again and no drain retires the posts
    /// (only aterm's `outbox sent` did, [`StateDir::clear_post_seq`]); left, each file is one a
    /// later stop flushes and a replaced log's ([`StateDir::forget_log_offsets`]) rewrites, and
    /// a receipt's reservation or a post's holds its session and the subjects it names back
    /// from the budget ([`StateDir::owed_publications`]), boot after boot, the producer id
    /// being the node's. `live` is the roster aterm answered at the start: a bridge of this
    /// boot whose run record could not be written may have left files and reservations of the
    /// sessions that still run. A file of a [`TornKind`] that does not read whole stays where
    /// it is, for the sweep of torn files or a person. A reservation whose sequence a record in
    /// flight holds stays too: an entry carries no sid and is tied to its post's reservation by
    /// its pin's sequence alone, the attach publishes it again at that pin
    /// (`Bridge::settle_in_flight`), and the next boot's sweep retires the reservation once it
    /// settled. Nothing of `inflight/` or `sent/` is touched (an older build's reservation the
    /// journal could not take in is left for a person, [`Posts::held_sent`]), nor any record in
    /// flight: an earlier boot's records in flight follow their own rules
    /// ([`StateDir::retire_in_flight_of_an_earlier_boot`]). Neither the unlinks nor the
    /// retires are flushed: a file or a reservation a power cut brings back is whole and no
    /// session reads or retires it, and the next boot's sweep takes it again. A retire whose
    /// record the disk refuses is still forgotten by the open, and the journal is rewritten
    /// without it before its next record ([`StateDir::append_post_frame`]). The bridge sweeps
    /// once, at its start's first roster where that reads, and where
    /// [`StateDir::last_ran_in_an_earlier_boot`] said so before the run record named this boot;
    /// no later roster sweeps. A dir off the boot volume is swept as one on it is, its lease
    /// naming its boot there too; only after an older build's attach there, whose lease names
    /// none, does the sweep wait for a later boot. NOT COVERED: off the boot volume the
    /// never-flushed lease and a barrier-ordered run record can both be lost with the disk
    /// within one boot, so a start then finds an earlier boot that this boot's attach already
    /// followed and sweeps a session this boot ended; `live` still keeps every session that
    /// runs.
    ///
    /// # Errors
    /// A directory that does not list, a file that does not read, an unlink, the post journal
    /// unreadable or damaged, or a retire's record the disk refused. None stops the rest of the
    /// sweep: every other file and reservation still goes, and the first error is returned.
    /// What it left stays until a later boot's sweep, and for good where the error repeats.
    pub(crate) fn sweep_sessions_of_an_earlier_boot(
        &self,
        live: &BTreeSet<String>,
    ) -> io::Result<u64> {
        let mut refused = None;
        let removed = self.sweep_files_of_an_earlier_boot(live, &mut refused);
        let retired = self.retire_reservations_of_an_earlier_boot(live, &mut refused);
        refused.map_or(Ok(removed + retired), Err)
    }

    /// The files' part of [`StateDir::sweep_sessions_of_an_earlier_boot`]: how many it unlinked.
    /// A place that does not list, a file that does not read and an unlink refused each stop
    /// nothing else of the sweep; the first such error is kept in `refused`.
    fn sweep_files_of_an_earlier_boot(
        &self,
        live: &BTreeSet<String>,
        refused: &mut Option<io::Error>,
    ) -> u64 {
        let mut removed = 0;
        for (place, kind) in SESSION_PLACES {
            let files = match self.files_in(place, |name| {
                !name.is_empty() && !name.contains('/') && !name.starts_with('.')
            }) {
                Ok(files) => files,
                Err(e) => {
                    refused.get_or_insert(e);
                    continue;
                }
            };
            for (name, rel) in files {
                let sid = if place == "acks" || place == ACK_HEADS {
                    name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s)
                } else {
                    name.as_str()
                };
                if live.contains(sid) {
                    continue;
                }
                match self.sweep_session_file(kind, &name, &rel) {
                    Ok(true) => removed += 1,
                    Ok(false) => {}
                    Err(e) => {
                        refused.get_or_insert(e);
                    }
                }
            }
        }
        removed
    }

    /// One file of an earlier boot's session unlinked, where it is there and, for a file of a
    /// [`TornKind`], reads whole: whether it was.
    fn sweep_session_file(
        &self,
        kind: Option<TornKind>,
        name: &str,
        rel: &str,
    ) -> io::Result<bool> {
        let path = self.path(rel);
        if let Some(kind) = kind {
            match crate::enable::read_bounded_bytes(&path, 64 << 20) {
                Ok(bytes) => {
                    if !std::str::from_utf8(&bytes).is_ok_and(|text| kind.whole(&path, name, text))
                    {
                        return Ok(false);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        match self.remove_path(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The reservations' part of [`StateDir::sweep_sessions_of_an_earlier_boot`]: every post
    /// reservation of a sid `live` does not name whose sequence no record in flight holds,
    /// retired, and how many. Each is retired in the open even where its record is refused
    /// ([`journal::Frame::holds_unwritten`]), so a refusal stops none of the others; the first
    /// error, a journal that does not read included, is kept in `refused` where none is.
    fn retire_reservations_of_an_earlier_boot(
        &self,
        live: &BTreeSet<String>,
        refused: &mut Option<io::Error>,
    ) -> u64 {
        #[cfg(test)]
        if self
            .keep_earlier_boot_reservations
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return 0;
        }
        let mut posts = match self.posts() {
            Ok(posts) => posts,
            Err(e) => {
                refused.get_or_insert(e);
                return 0;
            }
        };
        let leftover: Vec<(String, u64)> = {
            let index = posts.index();
            // The records in flight are keyed by their pin's sequence (`journal::Index::apply`).
            index
                .pins()
                .filter(|(sid, _, pin)| {
                    !live.contains(*sid) && !index.in_flight.contains_key(&pin.seq)
                })
                .map(|(sid, id, _)| (sid.to_string(), id))
                .collect()
        };
        let mut retired = 0;
        for (sid, id) in leftover {
            match self.append_post_frame(&mut posts, journal::Frame::Retire { sid, id }) {
                Ok(()) => retired += 1,
                Err(e) => {
                    refused.get_or_insert(e);
                }
            }
        }
        retired
    }

    /// Whether an incarnation lease names this boot: an attach of this boot took an incarnation,
    /// on whichever volume ([`IncLease::boot`]). Only an attach of this boot writes one that
    /// names it, so it needs no trust in the page cache. An older build's lease off the boot
    /// volume names no boot, and is not one.
    fn attached_in_this_boot(&self) -> bool {
        self.kernel_boot()
            .is_some_and(|now| self.lease().and_then(|lease| lease.boot) == Some(now))
    }

    fn torn_kind_dir(&self, kind: TornKind) -> io::Result<Option<PathBuf>> {
        self.dir_at(kind.place())
    }

    /// The directory `place` names in the dir, `None` where there is none.
    fn dir_at(&self, place: &str) -> io::Result<Option<PathBuf>> {
        let path = self.path(place);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
            Ok(meta) if meta.is_dir() => Ok(Some(path)),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not a directory", path.display()),
            )),
        }
    }

    fn torn_kind_files(&self) -> io::Result<Vec<(TornKind, String, String)>> {
        let mut out = Vec::new();
        for kind in TornKind::ALL {
            for (name, rel) in self.kind_files(kind)? {
                out.push((kind, name, rel));
            }
        }
        Ok(out)
    }

    fn kind_files(&self, kind: TornKind) -> io::Result<Vec<(String, String)>> {
        self.files_in(kind.place(), |name| kind.names(name))
    }

    /// The regular files directly in `place` whose names `names` takes, by name, as
    /// `(name, place/name)`; none where there is no `place`.
    fn files_in(
        &self,
        place: &str,
        names: impl Fn(&str) -> bool,
    ) -> io::Result<Vec<(String, String)>> {
        let Some(dir) = self.dir_at(place)? else {
            return Ok(Vec::new());
        };
        let entries = std::fs::read_dir(dir)?;
        let mut found = Vec::new();
        for entry in entries {
            if let Some(name) = entry?.file_name().to_str().filter(|n| names(n)) {
                found.push(name.to_string());
            }
        }
        found.sort_unstable();
        let mut out = Vec::new();
        for name in found {
            let rel = format!("{place}/{name}");
            match std::fs::symlink_metadata(self.path(&rel)) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
                Ok(meta) if !meta.is_file() => {}
                Ok(_) => out.push((name, rel)),
            }
        }
        Ok(out)
    }

    /// After an unclean stop in an earlier boot ([`StateDir::torn_files_possible`]), set every torn file aside.
    pub(crate) fn set_aside_torn(&self) -> io::Result<Vec<SetAside>> {
        self.sweep_torn(false)
    }

    #[cfg(test)]
    pub(crate) fn set_aside_torn_by_the_reader_alone(&self) -> io::Result<Vec<SetAside>> {
        self.sweep_torn(true)
    }

    fn sweep_torn(&self, by_the_reader: bool) -> io::Result<Vec<SetAside>> {
        if !self.torn_files_possible()? {
            return Ok(Vec::new());
        }
        let whole = if by_the_reader {
            TornKind::reads
        } else {
            TornKind::whole
        };
        let mut torn: Vec<(String, Vec<u8>)> = Vec::new();
        for (kind, name, rel) in self.torn_kind_files()? {
            if kind == TornKind::Journal {
                continue;
            }
            let path = self.path(&rel);
            let bytes = match crate::enable::read_bounded_bytes(&path, 64 << 20) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let whole =
                std::str::from_utf8(&bytes).is_ok_and(|text| whole(kind, &path, &name, text));
            if !whole {
                torn.push((rel, bytes));
            }
        }
        let mut moved = self.move_into_quarantine(&torn, "")?;
        moved.extend(self.sweep_journal()?);
        Ok(moved)
    }

    /// THE SWEEP OF THE POST JOURNAL, after an unclean stop in an earlier boot: a journal whose
    /// last record is broken, or that holds damage, has its exact bytes copied into the
    /// quarantine (`journal.posts`) and is then replaced, in full, by one that holds what its
    /// whole records say ([`journal::salvage`]: the records after damage too, never truncated
    /// at it), what the damaged bytes held being unknown. A power cut tears what the device was
    /// given after the last flush; the run whose records they were ended with that boot's
    /// sessions, its records in flight are retired next
    /// ([`StateDir::retire_in_flight_of_an_earlier_boot`]), and the bytes are kept for whoever
    /// repairs it. A journal that ends at a whole record, or in the next record cut short (an
    /// append cut, which its reader drops as the `acked` log's reader does), stays. The asks
    /// journal is swept the same way ([`StateDir::sweep_asks`]).
    fn sweep_journal(&self) -> io::Result<Vec<SetAside>> {
        let mut moved = self.sweep_posts()?;
        moved.extend(self.sweep_asks()?);
        Ok(moved)
    }

    /// THE SWEEP OF THE ASKS JOURNAL ([`StateDir::sweep_journal`]): one whose last record is
    /// broken, or that holds damage, has its exact bytes copied into the quarantine
    /// (`journal.asks`) and is then replaced, in full and flushed, by one that holds what its
    /// whole records say and what a table of a build before it holds that they do not
    /// ([`StateDir::read_asks`]). The deadlines the damaged bytes held are unknown: an ask whose
    /// deadline they held does not expire, and one whose settle they held is swept again, the
    /// sweep reading its answer, or its `expired`, back from the bus before it publishes one.
    fn sweep_asks(&self) -> io::Result<Vec<SetAside>> {
        let Some(bytes) = self.read_regular(&self.path(asks::FILE))? else {
            return Ok(Vec::new());
        };
        if matches!(
            asks::read(&bytes).end,
            journal::End::Clean | journal::End::Cut
        ) {
            return Ok(Vec::new());
        }
        let moved = self.copy_into_quarantine(&[(asks::FILE.to_string(), bytes)], "")?;
        let read = self.read_asks();
        let mut held = self.asks.lock().unwrap_or_else(|e| e.into_inner());
        *held = Asks {
            index: Some(read.index),
            dirty: true,
            compact_at: journal::COMPACT_AT,
            ..Asks::default()
        };
        self.rewrite_asks(&mut held, Some(aterm_durable::Flush::Durable))?;
        drop(held);
        self.quarantine_tried
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(asks::FILE);
        Ok(moved)
    }

    /// The post journal's part of [`StateDir::sweep_journal`].
    fn sweep_posts(&self) -> io::Result<Vec<SetAside>> {
        let Some(bytes) = self.read_regular(&self.path(journal::FILE))? else {
            return Ok(Vec::new());
        };
        if matches!(
            journal::read(&bytes).end,
            journal::End::Clean | journal::End::Cut
        ) {
            return Ok(Vec::new());
        }
        let moved = self.copy_into_quarantine(&[(journal::FILE.to_string(), bytes.clone())], "")?;
        let mut posts = self.posts.lock().unwrap_or_else(|e| e.into_inner());
        *posts = Posts {
            index: Some(journal::salvage(&bytes)),
            ..Posts::default()
        };
        self.rewrite_posts(&mut posts, Some(aterm_durable::Flush::Durable))?;
        drop(posts);
        self.quarantine_tried
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(journal::FILE);
        Ok(moved)
    }

    /// After an unclean stop in an earlier boot ([`StateDir::torn_files_possible`]), every whole
    /// in-flight entry is retired: an older build's `inflight/<seq>` moved into the quarantine as
    /// before, and each record the journal holds in flight retired with it
    /// ([`StateDir::retire_journal_in_flight`]).
    pub(crate) fn retire_in_flight_of_an_earlier_boot(&self) -> io::Result<Vec<SetAside>> {
        if !self.torn_files_possible()? || self.attached_in_this_boot() {
            return Ok(Vec::new());
        }
        let mut whole: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, rel) in self.kind_files(TornKind::InFlight)? {
            let path = self.path(&rel);
            let bytes = match crate::enable::read_bounded_bytes(&path, 64 << 20) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if std::str::from_utf8(&bytes)
                .is_ok_and(|text| TornKind::InFlight.whole(&path, &name, text))
            {
                whole.push((rel, bytes));
            }
        }
        let copies = self.copy_into_quarantine(&whole, RETIRED_PREFIX)?;
        // Noted once the copy is durable, before the unlink: a move whose unlink or directory
        // flush fails has made its one copy all the same.
        self.note_retired(&copies, &whole);
        let mut retired = self.remove_moved(copies, RETIRED_PREFIX)?;
        let mut posts = self.posts()?;
        retired.extend(self.retire_journal_in_flight(&mut posts)?);
        Ok(retired)
    }

    fn note_retired(&self, copies: &[SetAside], files: &[(String, Vec<u8>)]) {
        let mut done = self
            .retired_in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (from, bytes) in files {
            if copies.iter().any(|m| &m.from == from) {
                if let Some(seq) = from
                    .strip_prefix("inflight/")
                    .and_then(crate::parse_uint::<u64>)
                {
                    done.insert(seq, bytes.clone());
                }
            }
        }
    }

    /// RETIRE WHAT THE JOURNAL HOLDS IN FLIGHT, never to be published again: each record copied
    /// into the quarantine under the name of the file it would have been
    /// (`retired.inflight.<seq>`, its bytes those of that file) and dropped, the journal flushed
    /// in full after the drops. What an unclean stop in an earlier boot left
    /// ([`StateDir::retire_in_flight_of_an_earlier_boot`]), and what a stale journal holds
    /// ([`StateDir::judge_posts`]).
    fn retire_journal_in_flight(&self, posts: &mut Posts) -> io::Result<Vec<SetAside>> {
        let entries: Vec<(u64, String, Vec<u8>)> = posts
            .index()
            .in_flight
            .iter()
            .map(|(seq, entry)| {
                let mut text = entry.render();
                text.push('\n');
                (*seq, format!("inflight/{seq}"), text.into_bytes())
            })
            .collect();
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        // ONE RECORD, ONE COPY: a stop's hand-off and a cut can leave a record both in an
        // older build's file and in the journal ([`StateDir::hand_posts_to_files`]); the file's
        // move, or an earlier try of this open, copied it already.
        let files: Vec<(String, Vec<u8>)> = {
            let done = self
                .retired_in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            entries
                .iter()
                .filter(|(seq, _, bytes)| done.get(seq) != Some(bytes))
                .map(|(_, from, bytes)| (from.clone(), bytes.clone()))
                .collect()
        };
        let copied = self.copy_into_quarantine(&files, RETIRED_PREFIX)?;
        self.note_retired(&copied, &files);
        for (seq, _, _) in &entries {
            self.append_post_frame(posts, journal::Frame::Dropped(*seq))?;
        }
        self.sync_posts(posts, aterm_durable::Flush::Durable)?;
        let mut tried = self
            .quarantine_tried
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (_, from, _) in &entries {
            tried.remove(&self.quarantine_try(RETIRED_PREFIX, from));
        }
        Ok(copied)
    }

    /// How many torn files the quarantine still holds.
    pub(crate) fn quarantined_torn(&self) -> io::Result<Option<u64>> {
        let quarantine = self.path(QUARANTINE);
        match std::fs::symlink_metadata(&quarantine) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a directory", quarantine.display()),
                ));
            }
            Ok(_) => {}
        }
        let mut torn = 0u64;
        for entry in std::fs::read_dir(&quarantine)? {
            let entry = entry?;
            let named = entry
                .file_name()
                .to_str()
                .is_some_and(|n| !n.starts_with('.') && !n.starts_with(RETIRED_PREFIX));
            if named && entry.file_type()?.is_file() {
                torn += 1;
            }
        }
        Ok(Some(torn))
    }

    fn move_into_quarantine(
        &self,
        files: &[(String, Vec<u8>)],
        prefix: &str,
    ) -> io::Result<Vec<SetAside>> {
        let moved = self.copy_into_quarantine(files, prefix)?;
        self.remove_moved(moved, prefix)
    }

    /// What this open remembers its copy of `from` under: `prefix` with it, since one source has
    /// two copies in one recovery. A stop's hand-off cut before its unlink leaves a record in the
    /// journal and its `inflight/<seq>` torn: the sweep copies the file and the disk refuses its
    /// unlink, the retirement then copies the record under that name (`retired.`), and the
    /// sweep tried again must still find the file's copy, or it makes a second.
    fn quarantine_try(&self, prefix: &str, from: &str) -> String {
        #[cfg(test)]
        if self
            .one_memory_per_source
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return from.to_string();
        }
        format!("{prefix}{from}")
    }

    /// The sources of `moved`, whose copies are durable in the quarantine under `prefix`, unlinked
    /// and their directories flushed.
    fn remove_moved(&self, moved: Vec<SetAside>, prefix: &str) -> io::Result<Vec<SetAside>> {
        let mut dirs = std::collections::BTreeSet::new();
        for SetAside { from, .. } in &moved {
            let path = self.path(from);
            match self.remove_path(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            self.quarantine_tried
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.quarantine_try(prefix, from));
            dirs.insert(path.parent().unwrap_or(&self.root).to_path_buf());
        }
        for dir in dirs {
            self.flush_dir(&dir)?;
        }
        Ok(moved)
    }

    /// Publish a durable copy of each `(from, bytes)` in the quarantine, as
    /// `<prefix><from with / as .>[.N]`, reusing a copy this open made of the same bytes.
    fn copy_into_quarantine(
        &self,
        files: &[(String, Vec<u8>)],
        prefix: &str,
    ) -> io::Result<Vec<SetAside>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let quarantine = self.path(QUARANTINE);
        match std::fs::symlink_metadata(&quarantine) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a directory", quarantine.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&quarantine)?;
            }
            Err(e) => return Err(e),
        }
        self.flush_dir_as(&self.root, aterm_durable::Flush::Durable)?;
        #[cfg(test)]
        let one_copy = !self
            .copy_on_every_try
            .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let one_copy = true;
        let mut moved = Vec::new();
        for (from, bytes) in files {
            let tried_as = self.quarantine_try(prefix, from);
            let tried = self
                .quarantine_tried
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&tried_as)
                .map(|copy| -> io::Result<QuarantineCopy> {
                    Ok(QuarantineCopy {
                        name: copy.name.clone(),
                        file: copy.file.try_clone()?,
                    })
                })
                .transpose()?;
            let reused = match tried {
                Some(copy) if one_copy && self.quarantined_copy_of(&copy, bytes)? => {
                    Some(copy.name)
                }
                _ => None,
            };
            let to = match reused {
                Some(name) => name,
                None => {
                    let base = format!("{QUARANTINE}/{prefix}{}", from.replace('/', "."));
                    let mut to = base.clone();
                    let mut n = 0u32;
                    loop {
                        match self.publish_quarantine_file(&tried_as, &to, bytes) {
                            Ok(()) => break to,
                            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                                n = n.checked_add(1).ok_or_else(|| {
                                    io::Error::other("quarantine suffix space exhausted")
                                })?;
                                to = format!("{base}.{n}");
                            }
                            Err(e) => return Err(e),
                        }
                    }
                }
            };
            moved.push(SetAside {
                from: from.clone(),
                to,
            });
        }
        Ok(moved)
    }

    fn quarantined_copy_of(&self, copy: &QuarantineCopy, bytes: &[u8]) -> io::Result<bool> {
        use std::os::unix::fs::{FileExt as _, MetadataExt as _, OpenOptionsExt as _};
        let path = self.path(&copy.name);
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)
        {
            Ok(file) => file,
            Err(e)
                if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ELOOP) =>
            {
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        let meta = file.metadata()?;
        let owned = copy.file.metadata()?;
        if !meta.is_file()
            || meta.len() != bytes.len() as u64
            || (meta.dev(), meta.ino()) != (owned.dev(), owned.ino())
            || !self.quarantine_copy_current(copy)?
        {
            return Ok(false);
        }
        let mut held = vec![0; bytes.len()];
        file.read_exact_at(&mut held, 0)?;
        if held != bytes {
            return Ok(false);
        }
        let flush = aterm_durable::Flush::Durable;
        let w = crate::fx::Write::Flush { path: &path, flush };
        {
            let _timed = self.timed(crate::wait_tally::Row::DirFlush);
            self.disk.before(w)?;
            self.flush_opened(&file, flush)?;
            self.disk.after(w)?;
        }
        self.flush_dir_as(path.parent().unwrap_or(&self.root), flush)?;
        self.quarantine_copy_current(copy)
    }

    fn quarantine_copy_current(&self, copy: &QuarantineCopy) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt as _;
        let current = match std::fs::symlink_metadata(self.path(&copy.name)) {
            Ok(current) => current,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        let held = copy.file.metadata()?;
        Ok(current.is_file()
            && held.is_file()
            && (current.dev(), current.ino()) == (held.dev(), held.ino()))
    }

    fn publish_quarantine_file(&self, tried_as: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        self.refused_by_a_test(name)?;
        let target = self.path(name);
        let parent = target.parent().unwrap_or(&self.root);
        let w = crate::fx::Write::Replace {
            path: &target,
            bytes,
            flush: Some(aterm_durable::Flush::Durable),
            mode: 0o600,
        };
        let _timed = self.timed(crate::wait_tally::Row::DirReplace);
        self.disk.before(w)?;
        let (tmp, mut file) = crate::enable::create_temp_beside(&target, 0o600)?;
        let staged = (|| {
            file.write_all(bytes)?;
            // Quarantine evidence is flushed in full before its source may be unlinked:
            // through aterm-durable, so astream-sys's tally counts it as every other flush.
            if flushes_media() {
                aterm_durable::durable(&file)?;
            }
            std::fs::hard_link(&tmp, &target)
        })();
        if staged.is_ok() {
            self.quarantine_tried
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    tried_as.to_string(),
                    QuarantineCopy {
                        name: name.to_string(),
                        file,
                    },
                );
        }
        let cleaned = std::fs::remove_file(&tmp);
        cleaned?;
        staged?;
        self.count(|ops| {
            ops.replaces += 1;
            ops.fsyncs += 1;
        });
        self.directory_flush_result(parent, aterm_durable::Flush::Durable)?;
        self.count(|ops| ops.fsyncs += 1);
        self.disk.after(w)?;
        Ok(())
    }

    /// A bridge's run begins, after its sweep ([`StateDir::set_aside_torn`]) and before it writes any file of a [`TornKind`]: `run` says OPEN of this boot, its temp flushed before its rename and the root flushed after it. Both flushes are barriers only where the record it replaces names an earlier boot, this boot is named too, and every torn kind's directory that exists is on the root's device; anywhere else, both are full flushes: over a record of this boot (open or clean), over no record, where either boot is unnamed, and where a torn kind's directory is on another device or does not read. Nothing is written where the record already reads as the OPEN this start would write and a proof stands for that file as it stands (R6, [`RUN_PROOF`]): a process published it and a full flush that covers it returned (its publication's own, or a full write of `inc` after one the barrier ordered), and nothing has replaced it or written into it since. A start in full proves its record at once, one the barrier ordered once the incarnation's flush returns ([`StateDir::take_incarnation`]).
    pub(crate) fn begin_run(&self) -> io::Result<Flushed> {
        self.begin_run_by(StartRules::ALL)
    }

    #[cfg(test)]
    pub(crate) fn begin_run_with(&self, rules: StartRules) -> io::Result<Flushed> {
        self.begin_run_by(rules)
    }

    fn begin_run_by(&self, rules: StartRules) -> io::Result<Flushed> {
        use aterm_durable::Flush;
        use std::os::unix::fs::MetadataExt as _;
        // The post journal is judged while the record still says how the last run stopped
        // ([`StateDir::judge_posts`]): after an unclean stop in an earlier boot never stale. One
        // that does not read is refused where it is used.
        drop(self.judge_posts());
        let mut known = self.run_fence.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.kernel_boot();
        let current = match self.run_file() {
            Ok(current) => current,
            Err(e) => {
                *known = None;
                return Err(e);
            }
        };
        let last = current.as_ref().map(|(_, run)| *run);
        let open = LastRun {
            boot: now,
            clean: false,
        };
        if last == Some(open)
            && known.as_ref().is_some_and(|held| {
                let (Ok(held), Some(Ok(current))) = (
                    held.metadata(),
                    current.as_ref().map(|(file, _)| file.metadata()),
                ) else {
                    return false;
                };
                (held.dev(), held.ino()) == (current.dev(), current.ino())
            })
        {
            return Ok(Flushed::default());
        }
        let previous = known.take();
        let on_the_media = last.is_some_and(|run| {
            run.clean || (now.is_some() && run.boot.is_some() && run.boot != now)
        });
        if !on_the_media {
            let here = self.torn_kind_files()?;
            let kept = [LOGPIN, journal::HANDED]
                .into_iter()
                .filter(|rel| {
                    self.a_clean_stop_flushes(rel)
                        && std::fs::symlink_metadata(self.path(rel))
                            .is_ok_and(|meta| meta.is_file())
                })
                .map(str::to_string);
            self.unflushed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(here.into_iter().map(|(_, _, rel)| rel).chain(kept));
        }
        // THE OPEN ON THE MEDIA ALREADY (R6): a record that reads as the OPEN this start would
        // write, with a proof standing for that file as it stands, was published by a process of
        // this boot, a full flush that covers it returned, and nothing has replaced it or written
        // into it since; the media holds it, so writing the same bytes again would add nothing. A
        // handoff's successor and a restart after a crash find their predecessor's.
        if let Some((file, _)) = current.filter(|(file, run)| {
            *run == open && identity_of(file).is_some_and(|made| self.proven("run", made))
        }) {
            *known = Some(file);
            drop(previous);
            return Ok(Flushed {
                proven: true,
                ..Flushed::default()
            });
        }
        let tier = self.open_tier(last, now, rules.this_boot_in_full);
        let mut flushed = Flushed::default();
        let mut bytes = open.render().into_bytes();
        bytes.push(b'\n');
        let flush = match (rules.flush_record, rules.flush_root) {
            (false, _) => None,
            (true, false) => Some(Flush::Ordered),
            (true, true) => Some(tier),
        };
        let made = self.publish(
            "run",
            &bytes,
            flush,
            STATE_FILE_MODE,
            std::convert::identity,
        )?;
        match flush {
            Some(Flush::Durable) => flushed.note(flush, 2),
            Some(Flush::Ordered) => {
                flushed.note(flush, 1);
                if rules.flush_root {
                    self.flush_dir_as(&self.root, Flush::Ordered)?;
                    flushed.note(flush, 1);
                }
            }
            None => {}
        }
        let (file, record) = self.run_file()?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the published run record is missing",
            )
        })?;
        if record != open {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the published run record changed",
            ));
        }
        *known = Some(file);
        drop(previous);
        if let Some(made) = made.filter(|_| flush == Some(Flush::Durable)) {
            self.prove("run", made);
        }
        Ok(flushed)
    }

    /// After a full flush this open made, the OPEN this open admitted ([`StateDir::begin_run`]'s
    /// fence) is on the media whatever tier published it, where `run` still names it: a proof is
    /// made of it ([`StateDir::prove`]). The boot's first incarnation is that flush for a start the
    /// barrier ordered ([`StateDir::take_incarnation`]).
    fn prove_the_admitted_run(&self) {
        let known = self.run_fence.lock().unwrap_or_else(|e| e.into_inner());
        let Some(made) = known.as_ref().and_then(identity_of) else {
            return;
        };
        if identity_at(&self.path("run")) == Some(made) && !self.proven("run", made) {
            self.prove("run", made);
        }
    }

    /// The start's tier: the barrier only over an earlier named boot's record, this boot named too, with every torn kind's directory on the root's device. There a cut that loses OPEN loses every later write on the device with it, and this boot's first incarnation flushes `inc` in full before the attach presents anything to a broker, which puts OPEN on the media. Over a record of this boot, in full: a visible OPEN may be a failed replacement of this boot's CLEAN, and a same-boot lease flushes nothing later, so a lost OPEN would leave CLEAN vouching for its in-flight entries, which the next boot would republish behind later posts a broker on another volume kept.
    fn open_tier(
        &self,
        last: Option<LastRun>,
        now: Option<[u8; 16]>,
        this_boot_in_full: bool,
    ) -> aterm_durable::Flush {
        use aterm_durable::Flush;
        let ordered = last.is_some_and(|run| {
            now.is_some()
                && run.boot.is_some()
                && (run.boot != now || !(this_boot_in_full || run.clean))
        });
        if !ordered {
            return Flush::Durable;
        }
        for kind in TornKind::ALL {
            match self.torn_kind_dir(kind) {
                Ok(None) => {}
                Ok(Some(dir)) if Flush::ordered_if_same_device(&dir, &self.root).is_ordered() => {}
                Ok(Some(_)) | Err(_) => return Flush::Durable,
            }
        }
        Flush::Ordered
    }

    /// A clean stop: every file of a [`TornKind`] this run must flush ([`StateDir::begin_run`]), the mirror's pin and the heads a hand-off kept ([`journal::HANDED`]), flushed where they are, every such directory flushed (an unlink made since is kept), and only then `run` says the run stopped cleanly.
    pub(crate) fn end_run_clean(&self) -> io::Result<Flushed> {
        self.end_run_clean_by(StopRules::ALL)
    }

    #[cfg(test)]
    pub(crate) fn end_run_clean_with(&self, rules: StopRules) -> io::Result<Flushed> {
        self.end_run_clean_by(rules)
    }

    fn end_run_clean_by(&self, rules: StopRules) -> io::Result<Flushed> {
        use aterm_durable::Flush;
        let _ = self.last_run()?;
        let dirs: Vec<PathBuf> = TornKind::ALL
            .into_iter()
            .filter_map(|kind| self.torn_kind_dir(kind).transpose())
            .collect::<io::Result<_>>()?;
        let paths = self
            .unflushed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let tier = |dir: &Path| Flush::ordered_if_same_device(dir, &self.root);
        let mut flushed = Flushed::default();
        if !rules.record_last {
            self.write_clean_record(&mut flushed)?;
        }
        let mut root_moved = false;
        for rel in &paths {
            let path = self.path(rel);
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
                Ok(meta) if !meta.is_file() => continue,
                Ok(_) => {}
            }
            let parent = path.parent().unwrap_or(&self.root);
            let flush = tier(parent);
            if rules.flush_files && self.flush_in_place(rel, flush)? {
                flushed.files += 1;
                flushed.note(Some(flush), 1);
            }
            root_moved |= parent == self.root;
        }
        if rules.flush_dirs {
            let root = self.root.clone();
            for dir in dirs.iter().chain(root_moved.then_some(&root)) {
                let flush = tier(dir);
                self.flush_dir_as(dir, flush)?;
                flushed.dirs += 1;
                flushed.note(Some(flush), 1);
            }
        }
        if rules.record_last {
            self.write_clean_record(&mut flushed)?;
        }
        self.unflushed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|rel| !paths.contains(rel));
        Ok(flushed)
    }

    fn write_clean_record(&self, flushed: &mut Flushed) -> io::Result<()> {
        use aterm_durable::Flush;
        let clean = LastRun {
            boot: self.kernel_boot(),
            clean: true,
        };
        let mut record = clean.render().into_bytes();
        record.push(b'\n');
        self.replace_bytes_as("run", &record, Some(Flush::Ordered), STATE_FILE_MODE)?;
        flushed.note(Some(Flush::Ordered), 1);
        self.flush_dir_as(&self.root, Flush::Durable)?;
        flushed.note(Some(Flush::Durable), 1);
        Ok(())
    }

    /// The highest fleet-halt offset this node has published an `ack` for, so a reconnect does not re-answer the halt row its `Last` read returns again, nor a row that arrives both on a `Last` page and on the fleet tail.
    #[must_use]
    pub fn halt_acked(&self) -> Option<u64> {
        self.read("halt-acked").and_then(|s| crate::parse_uint(&s))
    }

    /// Record the halt offset this node has answered.
    pub fn set_halt_acked(&self, off: u64) -> io::Result<()> {
        if self.halt_acked().is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write("halt-acked", &off.to_string())
    }

    /// The inbox-group commit a bridge of `node` owed when it exited with no broker connection to
    /// make it on (`Bridge::hand_over_inbox_commit`). Each node's is its own: a commit a bridge of
    /// another node left, one before this Mac's key rotated, is that node's group's, which the
    /// node service makes before its own-inbox move follows that group (§11 C3b,
    /// `federate::Routes`), and made on this node's group it would pass this node's unread mail
    /// there.
    #[must_use]
    pub fn inbox_owed(&self, node: &str) -> Option<u64> {
        owed_of(&self.owed_entries(), node)
    }

    /// [`Self::inbox_owed`] for a reader that must not take a file it could not read for one that
    /// is not there (the own-inbox move's gate, §11 C3b): `Err` where `inbox-owed` exists and
    /// cannot be read.
    pub(crate) fn inbox_owed_present(&self, node: &str) -> io::Result<Option<u64>> {
        let text = self.read_present("inbox-owed")?;
        Ok(owed_of(
            &text.as_deref().map(parse_owed).unwrap_or_default(),
            node,
        ))
    }

    /// Every node's handed-over inbox commit, one `<node> <offset>` line each, in node order;
    /// a line naming no node is no one's.
    fn owed_entries(&self) -> Vec<(String, u64)> {
        self.read("inbox-owed")
            .as_deref()
            .map(parse_owed)
            .unwrap_or_default()
    }

    /// Write `owed` as the handed-over inbox commits, or remove the file when none is left.
    fn set_owed_entries(&self, owed: &[(String, u64)]) -> io::Result<()> {
        if owed.is_empty() {
            return match self.remove_path(&self.path("inbox-owed")) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let text: String = owed
            .iter()
            .map(|(node, off)| format!("{node} {off}\n"))
            .collect();
        self.write("inbox-owed", &text)
    }

    /// Hand over `node`'s inbox-group commit through `off` ([`Self::inbox_owed`]), beside any
    /// other node's.
    pub fn set_inbox_owed(&self, node: &str, off: u64) -> io::Result<()> {
        let mut owed = self.owed_entries();
        match owed.iter_mut().find(|(of, _)| of == node) {
            Some((_, have)) if *have >= off => return Ok(()),
            Some((_, have)) => *have = off,
            None => owed.push((node.to_owned(), off)),
        }
        owed.sort_unstable();
        self.set_owed_entries(&owed)
    }

    /// The inbox commit `node`'s bridge handed over has been made; any other node's stays.
    pub fn clear_inbox_owed(&self, node: &str) -> io::Result<()> {
        let mut owed = self.owed_entries();
        let had = owed.len();
        owed.retain(|(of, _)| of != node);
        if owed.len() == had {
            return Ok(());
        }
        self.set_owed_entries(&owed)
    }

    /// Forget the halt watermark.
    pub fn clear_halt_acked(&self) -> io::Result<()> {
        match self.remove_path(&self.path("halt-acked")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Forget every offset this dir holds that the log this node reads now does not vouch for ([`LogWitness::replaced_by`]), and put each back where a reader of this log, whose head is `head`, resumes. `was` is the log those offsets were read on, where known.
    pub fn forget_log_offsets(
        &self,
        head: u64,
        was: Option<astream_broker::LogIdentity>,
        log: Option<astream_broker::LogIdentity>,
        keep_below: Option<u64>,
        lineage: Option<&astream_broker::Lineage>,
    ) -> io::Result<Vec<(u64, String)>> {
        let keep_below = keep_below.map(|f| f.min(head));
        let kept = |off: u64| keep_below.is_some_and(|f| off < f);
        let entries = |name: &str| -> io::Result<Vec<std::fs::DirEntry>> {
            let path = self.path(name);
            match std::fs::read_dir(&path) {
                Ok(dir) => dir.collect(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    match std::fs::symlink_metadata(&path) {
                        Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                        Err(other) => Err(other),
                        Ok(_) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        };
        let mut seen_removed = Vec::new();
        let mut seen_clamped = Vec::new();
        for entry in entries("seen")? {
            let name = entry.file_name();
            let sid = name.to_str().filter(|n| !n.contains('.'));
            let off = sid.and_then(|sid| self.seen_off(sid));
            match (sid, off, keep_below) {
                (Some(_), Some(off), Some(_)) if kept(off) => {}
                (Some(sid), Some(_), Some(f)) if f > 0 => {
                    seen_clamped.push((sid.to_string(), f - 1));
                }
                _ => seen_removed.push(entry.path()),
            }
        }
        let mut unhandled_removed = Vec::new();
        let mut unhandled_kept = Vec::new();
        for entry in entries("unhandled")? {
            let name = entry.file_name();
            let floors = match name.to_str().filter(|n| !n.contains('.')) {
                Some(sid) => {
                    let floors = self.topic_floors(sid)?;
                    keep_below.map(|_| (sid.to_string(), floors))
                }
                None => None,
            };
            match floors {
                Some((sid, floors)) => {
                    let below: BTreeMap<String, u64> =
                        floors.into_iter().filter(|(_, off)| kept(*off)).collect();
                    unhandled_kept.push((sid, below));
                }
                None => unhandled_removed.push(entry.path()),
            }
        }
        let mut topics = Vec::new();
        for entry in entries("topics")? {
            let name = entry.file_name();
            let Some(sid) = name.to_str().filter(|n| !n.contains('.')) else {
                continue;
            };
            let body = crate::enable::read_bounded_string(&entry.path(), 64 << 20)?;
            let rebased: BTreeMap<String, TopicCursor> = self
                .parse_topics(sid, &body)
                .into_iter()
                .map(|(topic, cursor)| {
                    let next = match keep_below {
                        Some(f) => cursor.next.min(f),
                        None => head,
                    };
                    (topic, TopicCursor { next })
                })
                .collect();
            topics.push((sid.to_string(), rebased));
        }
        #[cfg(test)]
        let skip_resets = keep_below.is_none()
            && self
                .skip_publication_resets
                .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let skip_resets = false;
        let OffsetResets {
            keys: key_resets,
            posts: post_resets,
        } = if skip_resets {
            OffsetResets {
                keys: Vec::new(),
                posts: Vec::new(),
            }
        } else {
            self.publication_offset_resets(keep_below, another_log(was, log, lineage))?
        };
        let in_flight = self.in_flight()?;
        let owed = self.owed_entries();
        for path in seen_removed.into_iter().chain(unhandled_removed) {
            match self.remove_path(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        for (sid, off) in seen_clamped {
            self.write(&format!("seen/{sid}"), &off.to_string())?;
        }
        for (sid, floors) in unhandled_kept {
            self.set_topic_floors(&sid, &floors)?;
        }
        for (sid, rebased) in topics {
            self.set_topics(&sid, &rebased)?;
        }
        for (name, text) in key_resets {
            self.write(&name, &text)?;
        }
        for (sid, id, pin) in post_resets {
            self.set_post_pin(&sid, id, &pin)?;
        }
        // The forged-self window starts again at this log's head: empty, its floor the head, so
        // no record below the head is accused, and none above it is vouched for until it lands
        // (`Bridge::remember_self_acks`).
        self.set_self_acked(&std::collections::BTreeSet::new(), head, 1)?;
        self.clear_halt_acked()?;
        let still: Vec<(String, u64)> =
            owed.iter().filter(|(_, off)| kept(*off)).cloned().collect();
        if still.len() != owed.len() {
            self.set_owed_entries(&still)?;
        }
        self.forget_asks_where(kept)?;
        let mut dropped = Vec::new();
        let mut posts = self.posts()?;
        let mut moved = false;
        for (seq, entry) in in_flight {
            let (Some(was), Some(now)) = (entry.log, log) else {
                continue;
            };
            let re = crate::body::Body::decode(&entry.body).0.re;
            if was == now || re.is_none() {
                continue;
            }
            let fork = match lineage {
                Some(lineage) => keep_below.and_then(|f| Some(f.min(now.continues(was, lineage)?))),
                None => keep_below,
            };
            moved = true;
            if re.is_some_and(|off| fork.is_some_and(|f| off < f)) {
                self.append_post_frame(
                    &mut posts,
                    journal::Frame::InFlight(InFlight { log, ..entry }),
                )?;
                continue;
            }
            self.append_post_frame(&mut posts, journal::Frame::Dropped(seq))?;
            dropped.push((seq, entry.subject));
        }
        if moved {
            self.sync_posts(&mut posts, aterm_durable::Flush::Durable)?;
        }
        Ok(dropped)
    }

    /// What this node knows of the bus log it reads ([`LogWitness`]), or `None` before its first attach recorded anything.
    #[must_use]
    pub fn log_witness(&self) -> Option<LogWitness> {
        self.read("bus-log").and_then(|s| LogWitness::parse(&s))
    }

    /// Record what this node knows of the bus log it reads.
    pub fn set_log_witness(&self, witness: &LogWitness) -> io::Result<()> {
        self.write("bus-log", &witness.render())
    }

    /// The known log identity this node saw rewind (reuse its offsets), which stays refused.
    pub(crate) fn rewound_log(&self) -> io::Result<Option<astream_broker::LogIdentity>> {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;

        let path = self.path("rewound-log");
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the rewound log fact is not a regular file",
                ));
            }
            Ok(_) => {}
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the rewound log fact is not a regular file",
            ));
        }
        let mut text = String::with_capacity(
            usize::try_from(meta.len().min(128).saturating_add(1)).unwrap_or(0),
        );
        file.take(128).read_to_string(&mut text)?;
        if text.len() < 128 {
            if let Some(log) = astream_broker::LogIdentity::parse(text.trim()) {
                return Ok(Some(log));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the rewound log fact has no valid identity",
        ))
    }

    /// Record a rewound identity durably, so its refusal outlives a bridge or host restart.
    pub(crate) fn set_rewound_log(&self, log: astream_broker::LogIdentity) -> io::Result<()> {
        self.write_durable("rewound-log", &log.to_string())
    }

    /// Release the refusal, once the caller has fenced the endpoint on a different known log.
    pub(crate) fn clear_rewound_log(&self, witness: &LogWitness) -> io::Result<()> {
        self.write_durable("bus-log", &witness.render())?;
        match self.remove_path(&self.path("rewound-log")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        self.flush_dir(&self.root)
    }

    /// The fleet halts of `fleet` this node holds in force.
    #[must_use]
    pub fn standing_halts(&self, fleet: &str) -> BTreeMap<String, StandingHalt> {
        let Some(body) = self.read("halts") else {
            return BTreeMap::new();
        };
        let mut out = BTreeMap::new();
        for line in body.lines() {
            match StandingHalt::parse(line) {
                Some((f, human, halt)) if f == fleet => {
                    out.insert(human, halt);
                }
                Some(_) => {}
                None => eprintln!(
                    "aterm-link: {}: `{}` is not a standing halt; it is not kept",
                    self.path("halts").display(),
                    safe_line(line)
                ),
            }
        }
        out
    }

    /// Every fleet halt this node held in force under the old subject, of any fleet, as
    /// `(fleet, human, halt)`: what the move to the mesh carries (§11 C4), since the mesh's one
    /// fleet is seldom the one the halts were set on.
    #[must_use]
    pub fn old_standing_halts(&self) -> Vec<(String, String, StandingHalt)> {
        let Some(body) = self.read("halts") else {
            return Vec::new();
        };
        body.lines().filter_map(StandingHalt::parse).collect()
    }

    /// Forget every fleet halt held under the old subject, once the move carried them.
    ///
    /// # Errors
    /// A write that fails.
    pub fn clear_old_standing_halts(&self) -> io::Result<()> {
        if self.read("halts").is_none() {
            return Ok(());
        }
        self.write_durable("halts", "")
    }

    /// The halts this node applied with the mesh on ([`crate::halt::Halts`], its own text), or
    /// `None` while it applied none.
    #[must_use]
    pub fn mesh_halts(&self) -> Option<String> {
        self.read(crate::halt::STATE_FILE)
    }

    /// Replace the halts this node applied with the mesh on, flushed before it answers: a halt
    /// is acknowledged only once it would stand across a crash.
    ///
    /// # Errors
    /// A write that fails.
    pub fn set_mesh_halts(&self, text: &str) -> io::Result<()> {
        self.write_durable(crate::halt::STATE_FILE, text)
    }

    /// Replace `fleet`'s standing halts ([`StateDir::standing_halts`]).
    pub fn set_standing_halts(
        &self,
        fleet: &str,
        halts: &BTreeMap<String, StandingHalt>,
    ) -> io::Result<()> {
        let lines: Vec<String> = halts
            .iter()
            .map(|(human, halt)| halt.render(fleet, human))
            .collect();
        self.write_durable("halts", &lines.join("\n"))
    }

    /// The tofu `sid → node` pins.
    #[must_use]
    pub fn pins(&self) -> BTreeMap<String, String> {
        self.pin_lines()
            .into_iter()
            .filter_map(|l| {
                l.split_once(' ')
                    .map(|(a, b)| (a.to_string(), b.to_string()))
            })
            .collect()
    }

    /// The tofu pins on the media before a route stands on one.
    pub(crate) fn settle_pins(&self) -> io::Result<()> {
        if self.on_the_media("pins") {
            return Ok(());
        }
        let text = match crate::enable::read_bounded_string(&self.path("pins"), 64 << 20) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let lines: Vec<&str> = text.lines().filter(|l| l.contains(' ')).collect();
        self.write_durable("pins", &lines.join("\n"))
    }

    fn pin_lines(&self) -> Vec<String> {
        self.read("pins")
            .map(|s| {
                s.lines()
                    .filter(|l| l.contains(' '))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Pin `sid` to `node` unless it is pinned already.
    pub fn pin(&self, sid: &str, node: &str) -> io::Result<String> {
        let mut lines = self.pin_lines();
        if let Some(have) = lines
            .iter()
            .filter_map(|l| l.split_once(' '))
            .find(|(s, _)| *s == sid)
            .map(|(_, n)| n.to_string())
        {
            self.settle_pins()?;
            return Ok(have);
        }
        lines.push(format!("{sid} {node}"));
        let over = lines.len().saturating_sub(PINS_KEEP);
        self.write_durable("pins", &lines[over..].join("\n"))?;
        Ok(node.to_string())
    }

    /// Drop the pins of `sids`, in one durable write when any has one: nothing of those
    /// departed sessions is held here any more (`Bridge::reclaim_subjects`).
    pub(crate) fn unpin(&self, sids: &BTreeSet<String>) -> io::Result<()> {
        if sids.is_empty() {
            return Ok(());
        }
        let lines = self.pin_lines();
        let kept: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .filter(|l| l.split_once(' ').is_none_or(|(s, _)| !sids.contains(s)))
            .collect();
        if kept.len() == lines.len() {
            return Ok(());
        }
        self.write_durable("pins", &kept.join("\n"))
    }

    /// One session's broadcast cursors.
    #[must_use]
    pub fn topics(&self, sid: &str) -> BTreeMap<String, TopicCursor> {
        self.read(&format!("topics/{sid}"))
            .map_or_else(BTreeMap::new, |body| self.parse_topics(sid, &body))
    }

    fn parse_topics(&self, sid: &str, body: &str) -> BTreeMap<String, TopicCursor> {
        let mut out = BTreeMap::new();
        for line in body.trim().lines() {
            match TopicCursor::parse(line) {
                Some((topic, cursor)) => {
                    out.insert(topic, cursor);
                }
                None => eprintln!(
                    "aterm-link: {}: `{}` is not a broadcast cursor; that topic \
                     resumes from the bus head; what it missed is lost",
                    self.path(&format!("topics/{sid}")).display(),
                    safe_line(line)
                ),
            }
        }
        out
    }

    /// Replace one session's broadcast cursors.
    pub fn set_topics(&self, sid: &str, cursors: &BTreeMap<String, TopicCursor>) -> io::Result<()> {
        let body: Vec<String> = cursors
            .iter()
            .map(|(t, c)| format!("{t} {}", c.next))
            .collect();
        self.write(&format!("topics/{sid}"), &body.join("\n"))
    }

    /// Forget the broadcast cursors of a session gone from the roster, and the refill floors that rest on them ([`Self::topic_floors`]).
    pub fn forget_topics(&self, sid: &str) {
        let _ = self.remove_path(&self.path(&format!("topics/{sid}")));
        let _ = self.remove_path(&self.path(&format!("unhandled/{sid}")));
    }

    /// One session's broadcast refill floors.
    pub fn topic_floors(&self, sid: &str) -> io::Result<BTreeMap<String, u64>> {
        let dir = self.path("unhandled");
        match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "refill floor parent is not a directory",
                ));
            }
            Ok(_) => {}
        }
        let path = dir.join(sid);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "refill floor is not a regular file",
                ));
            }
            Ok(_) => {}
        }
        parse_topic_floors(&path, &crate::enable::read_bounded_string(&path, 64 << 20)?)
    }

    /// Replace one session's broadcast refill floors ([`Self::topic_floors`]).
    pub fn set_topic_floors(&self, sid: &str, floors: &BTreeMap<String, u64>) -> io::Result<()> {
        let name = format!("unhandled/{sid}");
        if floors.is_empty() {
            return match self.remove_path(&self.path(&name)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let body: Vec<String> = floors.iter().map(|(t, off)| format!("{t} {off}")).collect();
        self.write(&name, &body.join("\n"))
    }
    /// The producer sequence this bridge reserved for one outbound post, and whether a `post key=` entry an earlier post reserved chose it ([`StateDir::key_seq`]).
    #[must_use]
    pub fn post_seq(&self, sid: &str, id: u64) -> Option<(u64, bool)> {
        self.post_pin(sid, id)
            .ok()
            .flatten()
            .map(|pin| (pin.seq, pin.via_key))
    }

    /// Reserve a sequence for one outbound post before it is published.
    pub fn set_post_seq(&self, sid: &str, id: u64, seq: u64, via_key: bool) -> io::Result<()> {
        self.set_post_pin(
            sid,
            id,
            &PublishPin {
                seq,
                via_key,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
                at: None,
            },
        )
    }

    /// Forget a retired post's sequence: one journal record, none where the journal holds no
    /// reservation for it. The open forgets it even where the record cannot be written, as an
    /// older build's unlink of its file did on a disk too full to write, and the journal is
    /// rewritten without it before its next record ([`StateDir::append_post_frame`]); a stop or
    /// crash before that leaves it in the file, for the next open.
    pub fn clear_post_seq(&self, sid: &str, id: u64) {
        let _timed = self.timed(crate::wait_tally::Row::ClearPostSeq);
        let Ok(mut posts) = self.posts() else {
            return;
        };
        if posts.index().pin(sid, id).is_some() {
            let _ = self.append_post_frame(
                &mut posts,
                journal::Frame::Retire {
                    sid: sid.to_string(),
                    id,
                },
            );
        }
    }

    /// SETTLE AN ACK PASS'S LANDED POSTS in the post journal (critic item E07): for each, in
    /// order, its in-flight entry's end ([`PostSettle::landed`], as
    /// [`StateDir::clear_in_flight`] writes it) and then its reservation's
    /// ([`PostSettle::retired`], as [`StateDir::clear_post_seq`] writes it), each only where the
    /// journal holds it, every record in one write ([`StateDir::append_post_frames`]). On a
    /// failure the entries stand and the reservations are forgotten all the same, as each call
    /// alone would leave them.
    ///
    /// # Errors
    /// The journal unreadable or damaged, or the write: no entry was ended.
    pub(crate) fn settle_posts(&self, settles: &[PostSettle<'_>]) -> io::Result<()> {
        let _timed = self.timed(crate::wait_tally::Row::ClearPostSeq);
        let mut posts = self.posts()?;
        let index = posts.index();
        let mut frames = Vec::new();
        for settle in settles {
            if let Some(seq) = settle
                .landed
                .filter(|seq| index.in_flight.contains_key(seq))
            {
                frames.push(journal::Frame::Landed(seq));
            }
            if let Some((sid, id)) = settle
                .retired
                .filter(|(sid, id)| index.pin(sid, *id).is_some())
            {
                frames.push(journal::Frame::Retire {
                    sid: sid.to_string(),
                    id,
                });
            }
        }
        self.append_post_frames(&mut posts, frames)
    }

    /// The producer sequence this bridge reserved for one owed receipt, if any.
    #[must_use]
    pub fn receipt_seq(&self, sid: &str, rid: u64) -> Option<u64> {
        self.receipt_pin(sid, rid).ok().flatten().map(|pin| pin.seq)
    }

    /// Reserve a sequence for one owed receipt before it is published ([`StateDir::receipt_seq`]).
    pub fn set_receipt_seq(&self, sid: &str, rid: u64, seq: u64) -> io::Result<()> {
        self.remove_head(&format!("{ACK_HEADS}/{sid}.{rid}"))?;
        self.write(&format!("acks/{sid}.{rid}"), &seq.to_string())
    }

    /// Forget a retired receipt's sequence, and its head ([`ACK_HEADS`]).
    pub fn clear_receipt_seq(&self, sid: &str, rid: u64) {
        let _ = self.remove_path(&self.path(&format!("acks/{sid}.{rid}")));
        if self.keeps_receipt_heads() {
            let _ = self.remove_head(&format!("{ACK_HEADS}/{sid}.{rid}"));
        }
    }

    /// The producer sequence a session's `post key=<key>` reserved, if any post under that key did.
    #[must_use]
    pub fn key_seq(&self, sid: &str, key: &str) -> Option<u64> {
        self.key_pin(sid, key).ok().flatten().map(|pin| pin.seq)
    }

    #[cfg(test)]
    fn key_lines(&self, sid: &str) -> Vec<String> {
        self.read(&format!("keys/{sid}"))
            .map(|s| {
                s.lines()
                    .filter(|l| l.contains(' '))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Reserve `seq` for `key` before the publish, unless the key holds one already (first wins).
    pub fn set_key_seq(&self, sid: &str, key: &str, seq: u64) -> io::Result<u64> {
        self.set_key_pin(
            sid,
            key,
            &PublishPin {
                seq,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
                at: None,
            },
        )
        .map(|pin| pin.seq)
    }

    /// Forget `key`'s reservation if it is `seq` (that pair only).
    pub fn forget_key_seq(&self, sid: &str, key: &str, seq: u64) -> io::Result<()> {
        if !self.key_pin(sid, key)?.is_some_and(|pin| pin.seq == seq) {
            return Ok(());
        }
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| l.split_whitespace().next() != Some(key))
            .collect();
        self.write(&format!("keys/{sid}"), &kept.join("\n"))
    }

    /// Every deadline this node still owes a verdict on, oldest offset first, as the disk holds
    /// them now: the asks journal, reconciled with the tables of a build before it
    /// ([`StateDir::read_asks`]). Read afresh at each call, so another open sees what the bridge
    /// wrote since.
    #[must_use]
    pub fn deadlines(&self) -> Vec<Deadline> {
        self.read_asks().index.deadline_rows()
    }

    /// Every ask this node remembers the recipient of, oldest offset first, as the disk holds
    /// them now ([`StateDir::deadlines`]).
    #[must_use]
    pub fn asked(&self) -> Vec<Asked> {
        self.read_asks().index.asked_rows()
    }

    /// Write the asked table as a build before the asks journal wrote it: its last
    /// [`ASKED_KEEP`] rows, no hand-off named.
    #[cfg(test)]
    pub(crate) fn set_asked_as_an_older_build(&self, list: &[Asked]) -> io::Result<()> {
        let over = list.len().saturating_sub(ASKED_KEEP);
        let lines: Vec<String> = list[over..].iter().map(Asked::render).collect();
        self.write(asks::ASKED, &lines.join("\n"))
    }

    /// Write the deadline table as a build before the asks journal wrote it: its last
    /// [`DEADLINES_KEEP`] rows, no hand-off named.
    #[cfg(test)]
    pub(crate) fn set_deadlines_as_an_older_build(&self, list: &[Deadline]) -> io::Result<()> {
        let over = list.len().saturating_sub(DEADLINES_KEEP);
        let lines: Vec<String> = list[over..].iter().map(Deadline::render).collect();
        self.write(asks::DEADLINES, &lines.join("\n"))
    }

    /// The directory itself, for what this type does not model.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
