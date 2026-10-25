// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Persistent bridge state directory (`StateDir`): atomic file replacement, quarantine, and session cursors.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
        for tok in line.split_whitespace() {
            match tok.split_once('=') {
                Some(("off", v)) => off = v.parse().ok(),
                Some(("sid", v)) => sid = Some(v.to_string()),
                Some(("at", v)) => at = v.parse().ok(),
                Some(("dl", v)) => dl = v.parse().ok(),
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
        let off = toks.next()?.parse().ok()?;
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

fn tolerate_dir_flush(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::Unsupported
                    | io::ErrorKind::InvalidInput
                    | io::ErrorKind::PermissionDenied
                    | io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        other => other,
    }
}

fn sync_dir_required_as(dir: &Path, flush: aterm_durable::Flush) -> io::Result<()> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    std::fs::File::open(dir).and_then(|d| {
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
    /// The boot that wrote it, the one boot whose page cache vouches for it.
    pub boot: Option<[u8; 16]>,
    /// The `inc` the top was flushed into, `None` where the platform names no inode.
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
        for tok in text.split_whitespace() {
            match tok.split_once('=')? {
                ("used", v) if used.is_none() => used = Some(v.parse().ok()?),
                ("top", v) if top.is_none() => top = Some(v.parse().ok()?),
                ("boot", "-") if boot.is_none() => boot = Some(None),
                ("boot", v) if boot.is_none() => {
                    boot = Some(Some(astream_sys::parse_boot_id(v.as_bytes())?));
                }
                ("file", "-") if file.is_none() => file = Some(None),
                ("file", v) if file.is_none() => {
                    let mut ids = v.split(':').map(|n| n.parse::<u64>().ok());
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

fn on_boot_volume(dir: &Path) -> bool {
    std::fs::File::open(dir).is_ok_and(|opened| astream_broker::on_boot_volume(&opened))
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
    /// This boot, as the walk stands it in.
    pub boot: Option<Option<[u8; 16]>>,
    /// The dir stands off the boot volume.
    pub off_volume: bool,
    /// The step the next take dies at.
    pub kill_at: Option<IncStep>,
    /// One defect, the walk's negative controls (the model's `Buggy`).
    pub mutant: u8,
    /// The lease a flush covers, short enough for the walk to run one out (the model's `Span`).
    pub span: Option<u64>,
}

/// The bridge's state directory.
pub struct StateDir {
    root: PathBuf,
    disk: crate::fx::Disk,
    unflushed: std::sync::Mutex<std::collections::BTreeSet<String>>,
    logpin_fence: std::sync::Mutex<Option<std::fs::File>>,
    run_fence: std::sync::Mutex<Option<std::fs::File>>,
    undurable: std::sync::Mutex<std::collections::BTreeSet<String>>,
    floor_admitted: std::sync::atomic::AtomicBool,
    quarantine_tried: std::sync::Mutex<BTreeMap<String, QuarantineCopy>>,
    #[cfg(any(test, debug_assertions))]
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
}

/// The filesystem operations one [`StateDir`] made, by kind (counted only in a test build,
/// `StateDir::ops`), so a test states what a post costs as a count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FileOps {
    /// Reservation and journal reads ([`StateDir::read_reservation`]), hits and misses alike.
    pub reads: u64,
    /// Atomic replaces: a fresh temp written, then renamed over the target.
    pub replaces: u64,
    /// Renames, the replaces' own included.
    pub renames: u64,
    /// `fsync`s (`F_FULLFSYNC` on macOS).
    pub fsyncs: u64,
    /// Barriers (`F_BARRIERFSYNC` on macOS, `aterm_durable::ordered`).
    pub barriers: u64,
    /// Unlinks of a journal entry or a reservation.
    pub unlinks: u64,
    /// Appends to an append-only file.
    pub appends: u64,
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
}

impl PublishPin {
    pub(crate) fn parse(text: &str) -> io::Result<Self> {
        let bad = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid publication reservation",
            )
        };
        let mut words = text.split_whitespace();
        let seq = words.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
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
                        .and_then(|s| s.parse::<u64>().ok())
                        .filter(|id| *id > 0)
                        .ok_or_else(bad)?,
                );
                parts = &parts[2..];
                continue;
            }
            if parts[0] == "original-off" && original_off.is_none() {
                original_off = Some(parts.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?);
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
        })
    }

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

/// One record in flight onto this node's own lanes ([`StateDir::in_flight`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InFlight {
    pub pin: PublishPin,
    pub subject: String,
    pub body: Vec<u8>,
    /// The log this body was prepared on, the only one where a `re=` in it points.
    pub log: Option<astream_broker::LogIdentity>,
}

impl InFlight {
    fn render(&self) -> String {
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
        })
    }
}

/// The acked self-lane log as read back ([`StateDir::self_acked_log`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AckedLog {
    /// Every offset the file holds whole.
    pub offsets: std::collections::BTreeSet<u64>,
    /// How many entries the file holds, repeats included.
    pub entries: usize,
    /// Whether the file ends at a terminator (or is absent), so an append may go on after its last byte.
    pub appendable: bool,
}

impl AckedLog {
    fn parse(text: &str) -> Self {
        let mut log = Self {
            appendable: text.is_empty() || text.ends_with(','),
            ..Self::default()
        };
        let mut pieces: Vec<&str> = text.split(',').collect();
        let tail = pieces.pop().unwrap_or_default();
        for piece in pieces {
            log.entries += 1;
            if let Ok(off) = piece.trim().parse() {
                log.offsets.insert(off);
            }
        }
        if tail.ends_with('\n') {
            if let Ok(off) = tail.trim().parse() {
                log.entries += 1;
                log.offsets.insert(off);
            }
        }
        log
    }

    fn render(offsets: impl Iterator<Item = u64>) -> String {
        let mut text = String::new();
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
    /// `inflight/<seq>` ([`StateDir::in_flight`]).
    InFlight,
    /// `sent/<sid>.<id>` ([`StateDir::post_pin`]).
    Sent,
    /// `acks/<sid>.<rid>` ([`StateDir::receipt_pin`]).
    Acks,
    /// `keys/<sid>` ([`StateDir::key_pin`]).
    Keys,
    /// `unhandled/<sid>` ([`StateDir::topic_floors`]).
    Unhandled,
}

impl TornKind {
    /// Every kind, in the order a sweep visits them.
    pub(crate) const ALL: [Self; 5] = [
        Self::InFlight,
        Self::Sent,
        Self::Acks,
        Self::Keys,
        Self::Unhandled,
    ];

    const fn place(self) -> &'static str {
        match self {
            Self::InFlight => "inflight",
            Self::Sent => "sent",
            Self::Acks => "acks",
            Self::Keys => "keys",
            Self::Unhandled => "unhandled",
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
        }
    }

    fn entry(name: &str, text: &str) -> Option<InFlight> {
        name.parse().ok().and_then(|seq| InFlight::parse(seq, text))
    }

    fn names(self, name: &str) -> bool {
        !name.is_empty()
            && !name.contains('/')
            && !name.starts_with('.')
            && (self != Self::InFlight || name.bytes().all(|b| b.is_ascii_digit()))
    }
}

/// What a run record's write flushed ([`StateDir::begin_run`], [`StateDir::end_run_clean`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Flushed {
    /// Files of a [`TornKind`], and the mirror's pin, flushed where they are.
    pub files: u64,
    /// Directories whose entries moved, flushed before the record (the root's full flush for the record not counted).
    pub dirs: u64,
    /// Barriers: a file's, a directory's, the record's bytes'.
    pub ordered: u64,
    /// Full flushes: a file's or a directory's on another device than the root, the record's.
    pub full: u64,
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
}

impl StartRules {
    /// Every rule on: what ships.
    pub(crate) const ALL: Self = Self {
        flush_record: true,
        flush_root: true,
    };
}

/// How the last bridge of this dir stopped, and in which boot (`run`: open ordered ahead of every later write, clean durable, [`StateDir::begin_run`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LastRun {
    /// The boot it ran in, as the kernel names it.
    pub boot: Option<[u8; 16]>,
    /// Whether it stopped cleanly, every file of a [`TornKind`] on the media.
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

/// The bus mirror's pin ([`StateDir::logpin`]).
pub(crate) const LOGPIN: &str = "logpin";
/// The bus mirror's alias key, its durable copy ([`StateDir::logkey`]).
pub(crate) const LOGKEY: &str = "logkey";
/// The floor of the bus mirror's lifetime total ([`StateDir::logfloor`]).
pub(crate) const LOGFLOOR: &str = "logfloor";

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

impl StateDir {
    /// Open (creating) a state directory.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        Self::open_on(root, crate::fx::Disk::Os)
    }

    /// [`StateDir::open`], writing through `disk` (the bridge's door, [`crate::fx::Fx::disk`]).
    pub(crate) fn open_on(root: impl Into<PathBuf>, disk: crate::fx::Disk) -> io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("seen"))?;
        std::fs::create_dir_all(root.join("sent"))?;
        Ok(Self::at_on(root, disk))
    }

    fn at(root: PathBuf) -> Self {
        Self::at_on(root, crate::fx::Disk::Os)
    }

    fn at_on(root: PathBuf, disk: crate::fx::Disk) -> Self {
        Self {
            root,
            disk,
            unflushed: std::sync::Mutex::default(),
            logpin_fence: std::sync::Mutex::default(),
            run_fence: std::sync::Mutex::default(),
            undurable: std::sync::Mutex::default(),
            floor_admitted: std::sync::atomic::AtomicBool::new(false),
            quarantine_tried: std::sync::Mutex::default(),
            #[cfg(any(test, debug_assertions))]
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
        }
    }

    #[cfg(test)]
    pub(crate) fn copy_on_every_try(&self, on: bool) {
        self.copy_on_every_try
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn ops(&self) -> FileOps {
        *self.ops.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn set_inc_seam(&self, seam: IncSeam) {
        *self.inc_seam.lock().unwrap_or_else(|e| e.into_inner()) = seam;
    }

    #[cfg(test)]
    pub(crate) fn refuse_writes_of(&self, name: Option<&str>) {
        *self.refused.lock().unwrap_or_else(|e| e.into_inner()) = name.map(str::to_string);
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
        #[cfg(any(test, debug_assertions))]
        self.times.turn_on(_on);
    }

    /// Copy this dir's rows into the wait tally ([`crate::wait_tally::Kept`]).
    pub(crate) fn copy_dir_rows(&self, _t: &mut crate::wait_tally::Tally) {
        #[cfg(any(test, debug_assertions))]
        self.times.copy_into(_t);
    }

    #[cfg(any(test, debug_assertions))]
    fn timed(&self, row: crate::wait_tally::Row) -> crate::wait_tally::Timer<'_> {
        self.times.timer(row)
    }

    #[cfg(not(any(test, debug_assertions)))]
    fn timed(&self, _row: crate::wait_tally::Row) -> std::marker::PhantomData<&'_ ()> {
        std::marker::PhantomData
    }

    fn count(&self, _note: impl FnOnce(&mut FileOps)) {
        #[cfg(test)]
        _note(&mut self.ops.lock().unwrap_or_else(|e| e.into_inner()));
    }

    fn unlink(&self, name: &str) -> io::Result<()> {
        self.count(|ops| ops.unlinks += 1);
        self.remove_path(&self.path(name))
    }

    fn remove_path(&self, path: &Path) -> io::Result<()> {
        let w = crate::fx::Write::Remove { path };
        let _timed = self.timed(crate::wait_tally::Row::DirUnlink);
        self.disk.before(w)?;
        std::fs::remove_file(path)?;
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
        std::fs::read_to_string(self.path(name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn read_reservation(&self, name: &str) -> io::Result<Option<String>> {
        self.count(|ops| ops.reads += 1);
        match std::fs::read_to_string(self.path(name)) {
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

    /// Establish the current pin's v=2 fence before a copy, take or send.
    pub(crate) fn ensure_logpin_durable(&self, text: impl FnOnce() -> String) -> io::Result<()> {
        let mut known = self.logpin_fence.lock().unwrap_or_else(|e| e.into_inner());
        if self.logpin_fence_known(&mut known) {
            return Ok(());
        }
        self.replace_logpin(&text(), &mut known, true)
    }

    fn logpin_fence_known(&self, known: &mut Option<std::fs::File>) -> bool {
        use std::os::unix::fs::{FileExt as _, MetadataExt as _};
        let matches = known.as_ref().is_some_and(|held| {
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
        });
        if matches {
            return true;
        }
        *known = None;
        false
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
        self.replace_bytes(LOGPIN, text.as_bytes(), durable, 0o600)?;
        if crate::mirror::Pin::by_this_build(text.as_bytes()) {
            *known = self.retained_logpin();
        }
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
        let path = self.path(LOGPIN);
        if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
            return None;
        }
        let file = std::fs::File::open(&path).ok()?;
        let mut head = Vec::new();
        file.take(crate::mirror::PIN_HEADER.len() as u64 + 1)
            .read_to_end(&mut head)
            .ok()?;
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

    /// Raise `logfloor` to `top`, durably.
    pub(crate) fn raise_logfloor(&self, top: u64) -> io::Result<u64> {
        let admitted = self.logfloor_admitted();
        match self.logfloor()? {
            Some(floor) if floor >= top && admitted => Ok(floor),
            floor => {
                let top = floor.map_or(top, |floor| floor.max(top));
                let bytes = format!("{top}\n");
                self.replace_bytes_as_with(
                    LOGFLOOR,
                    bytes.as_bytes(),
                    Some(aterm_durable::Flush::Durable),
                    STATE_FILE_MODE,
                    std::convert::identity,
                )?;
                self.floor_admitted
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(top)
            }
        }
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

    /// Whether the floor that reads is on the media as far as this open can say.
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
        let bytes = std::fs::read(&path)?;
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

    pub(crate) fn post_pin(&self, sid: &str, id: u64) -> io::Result<Option<PublishPin>> {
        self.read_reservation(&format!("sent/{sid}.{id}"))?
            .map(|s| PublishPin::parse(&s))
            .transpose()
    }

    pub(crate) fn set_post_pin(&self, sid: &str, id: u64, pin: &PublishPin) -> io::Result<()> {
        let _timed = self.timed(crate::wait_tally::Row::SetPostPin);
        self.write(&format!("sent/{sid}.{id}"), &pin.render())
    }

    pub(crate) fn receipt_pin(&self, sid: &str, rid: u64) -> io::Result<Option<PublishPin>> {
        self.read_reservation(&format!("acks/{sid}.{rid}"))?
            .map(|s| PublishPin::parse(&s))
            .transpose()
    }

    pub(crate) fn set_receipt_pin(&self, sid: &str, rid: u64, pin: &PublishPin) -> io::Result<()> {
        self.write(&format!("acks/{sid}.{rid}"), &pin.render())
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

    fn publication_offset_resets(
        &self,
        keep_below: Option<u64>,
    ) -> io::Result<Vec<(String, String)>> {
        let reset = |pin: &mut PublishPin| -> bool {
            let stale = pin
                .original_off
                .is_some_and(|off| keep_below.is_none_or(|f| off >= f));
            stale && pin.original_off.take().is_some()
        };
        let mut rewrites = Vec::new();
        for directory in ["keys", "sent"] {
            let path = self.path(directory);
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(e)
                    if e.kind() == io::ErrorKind::NotFound
                        && std::fs::symlink_metadata(&path)
                            .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
                {
                    continue;
                }
                Err(e) => return Err(e),
            };
            for entry in entries {
                let entry = entry?;
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
                let text = std::fs::read_to_string(entry.path())?;
                let next = if directory == "keys" {
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
                    changed.then(|| lines.join("\n"))
                } else {
                    let mut pin = PublishPin::parse(&text)?;
                    reset(&mut pin).then(|| pin.render())
                };
                if let Some(next) = next {
                    rewrites.push((format!("{directory}/{name}"), next));
                }
            }
        }
        Ok(rewrites)
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
        let staged = (|| {
            f.write_all(bytes)?;
            if let Some(flush) = flush.filter(|_| flushes_media()) {
                flush.file(&f)?;
            }
            drop(f);
            std::fs::rename(&tmp, &target)
        })();
        if staged.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return staged;
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
        if TornKind::of(name).is_some() || name == LOGPIN {
            let mut unflushed = self.unflushed.lock().unwrap_or_else(|e| e.into_inner());
            if durable && finished.is_ok() {
                unflushed.remove(name);
            } else {
                unflushed.insert(name.to_string());
            }
        }
        finished
    }

    /// Take this state dir for one bridge's life.
    pub fn lock_bridge(&self) -> io::Result<BridgeLock> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.path("lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the bridge lock {} is not a regular file", path.display()),
            ));
        }
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
        self.read("pid").and_then(|s| s.parse().ok())
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

    /// The node id this dir records, if it records one.
    #[must_use]
    pub fn node(&self) -> Option<String> {
        self.read("node")
    }

    /// The incarnation this node last used, as far as the local disk knows.
    #[must_use]
    pub fn incarnation(&self) -> u64 {
        let used = self.lease().map_or(0, |lease| lease.used);
        used.max(incarnation_of_seq(self.sequence()))
    }

    fn incarnation_top(&self) -> u64 {
        self.read("inc").and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// What `inc-lease` holds, if it holds a whole one ([`IncLease::parse`]).
    pub(crate) fn lease(&self) -> Option<IncLease> {
        self.read("inc-lease").and_then(|s| IncLease::parse(&s))
    }

    /// Which `inc` this dir holds now ([`IncFile`]).
    #[cfg(unix)]
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

    /// No inode to name: never a trusted lease (nor a boot to trust it in: [`on_boot_volume`]).
    #[cfg(not(unix))]
    pub(crate) fn inc_file(&self) -> Option<IncFile> {
        None
    }

    /// The mark this state dir's bridges put on every presence row of a session they write
    /// ([`PRESENCE_MARK`]).
    #[cfg(unix)]
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

    /// No inode to name: no mark, and no row is ever the dir's by one.
    #[cfg(not(unix))]
    #[must_use]
    pub(crate) fn mark(&self, _node: &str) -> Option<String> {
        None
    }

    /// This boot, when the page cache can vouch for this state dir.
    pub(crate) fn trusted_boot(&self) -> Option<[u8; 16]> {
        #[cfg(test)]
        {
            let seam = self.inc_seam();
            if let Some(boot) = seam.boot {
                return boot.filter(|_| !seam.off_volume);
            }
        }
        crate::durability::this_boot().filter(|_| on_boot_volume(&self.root))
    }

    fn kernel_boot(&self) -> Option<[u8; 16]> {
        #[cfg(test)]
        if let Some(boot) = self.inc_seam().boot {
            return boot;
        }
        crate::durability::this_boot()
    }

    /// Take the incarnation an attach runs under, before its will is registered.
    pub fn take_incarnation(&self, bus: u64) -> io::Result<u64> {
        let (top, file) = (self.incarnation_top(), self.inc_file());
        let boot = self.trusted_boot();
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
        let leased = |file| IncLease {
            used: plan.next,
            top: plan.top,
            boot,
            file,
        };
        #[cfg(test)]
        if plan.flush && self.inc_seam().mutant == 3 {
            self.write("inc-lease", &leased(file).render())?;
        }
        let file = if plan.flush {
            self.write_durable("inc", &plan.top.to_string())?;
            self.kill_point(IncStep::Flushed)?;
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
        self.read("seq").and_then(|s| s.parse().ok()).unwrap_or(0)
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
            .and_then(|s| s.parse().ok())
    }

    /// Record a session's refill watermark ([`Self::seen_off`]).
    pub fn set_seen_off(&self, sid: &str, off: u64) -> io::Result<()> {
        if self.seen_off(sid).is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write(&format!("seen/{sid}"), &off.to_string())
    }

    /// Every offset this bridge holds a `PublishAck` for on its own lanes.
    #[must_use]
    pub fn self_acked(&self) -> std::collections::BTreeSet<u64> {
        self.self_acked_log().offsets
    }

    /// The `acked` file as it stands.
    pub(crate) fn self_acked_log(&self) -> AckedLog {
        match std::fs::read_to_string(self.path("acked")) {
            Ok(text) => AckedLog::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => AckedLog {
                appendable: true,
                ..AckedLog::default()
            },
            Err(_) => AckedLog::default(),
        }
    }

    /// Replace the acked self-lane set with the newest `keep` of `set`.
    pub fn set_self_acked(
        &self,
        set: &std::collections::BTreeSet<u64>,
        keep: usize,
    ) -> io::Result<()> {
        let over = set.len().saturating_sub(keep);
        self.replace_bytes(
            "acked",
            AckedLog::render(set.iter().skip(over).copied()).as_bytes(),
            false,
            STATE_FILE_MODE,
        )
    }

    /// Append `off` to the acked self-lane log, ended by its comma, in one `write` to a file opened `O_APPEND`.
    pub(crate) fn append_self_acked(&self, off: u64) -> io::Result<()> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let _call = self.timed(crate::wait_tally::Row::AppendSelfAcked);
        self.count(|ops| ops.appends += 1);
        let path = self.path("acked");
        let line = AckedLog::render(std::iter::once(off));
        let w = crate::fx::Write::Append {
            path: &path,
            bytes: line.as_bytes(),
        };
        let _timed = self.timed(crate::wait_tally::Row::DirAppend);
        self.disk.before(w)?;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(STATE_FILE_MODE)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != aterm_uds::peer::our_uid()
            || metadata.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the acked log is not an owned regular single-link file",
            ));
        }
        file.write_all(line.as_bytes())?;
        self.disk.after(w)?;
        Ok(())
    }

    /// Every record in flight onto this node's own lanes, by sequence.
    pub(crate) fn in_flight(&self) -> io::Result<Vec<(u64, InFlight)>> {
        let dir = self.path("inflight");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(&dir)
                        .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
            {
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let bad = || {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: malformed in-flight entry", entry.path().display()),
                )
            };
            let seq = name.parse().map_err(|_| bad())?;
            let text = self
                .read_reservation(&format!("inflight/{name}"))?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "{}: in-flight entry disappeared during recovery",
                            entry.path().display()
                        ),
                    )
                })?;
            let record = InFlight::parse(seq, &text).ok_or_else(bad)?;
            out.push((seq, record));
        }
        out.sort_by_key(|(seq, _)| *seq);
        Ok(out)
    }

    /// Record one own-lane record as in flight, before it is published.
    pub(crate) fn set_in_flight(&self, entry: &InFlight) -> io::Result<()> {
        let _timed = self.timed(crate::wait_tally::Row::SetInFlight);
        self.replace(
            &format!("inflight/{}", entry.pin.seq),
            &entry.render(),
            false,
            0o600,
        )
    }

    /// Forget the in-flight record at `seq`.
    pub(crate) fn clear_in_flight(&self, seq: u64) -> io::Result<()> {
        let _timed = self.timed(crate::wait_tally::Row::ClearInFlight);
        match self.unlink(&format!("inflight/{seq}")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Drop the in-flight record at `seq` for good.
    pub(crate) fn drop_in_flight(&self, seq: u64) -> io::Result<()> {
        match self.unlink(&format!("inflight/{seq}")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        self.flush_dir(&self.path("inflight"))
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
        if !file.metadata()?.is_file() {
            return Err(bad());
        }
        let mut text = String::new();
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

    fn attached_in_this_boot(&self) -> bool {
        self.trusted_boot()
            .is_some_and(|now| self.lease().and_then(|lease| lease.boot) == Some(now))
    }

    fn torn_kind_dir(&self, kind: TornKind) -> io::Result<Option<PathBuf>> {
        let path = self.path(kind.place());
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
        let Some(dir) = self.torn_kind_dir(kind)? else {
            return Ok(Vec::new());
        };
        let entries = std::fs::read_dir(dir)?;
        let mut names = Vec::new();
        for entry in entries {
            if let Some(name) = entry?.file_name().to_str().filter(|n| kind.names(n)) {
                names.push(name.to_string());
            }
        }
        names.sort_unstable();
        let mut out = Vec::new();
        for name in names {
            let rel = format!("{}/{name}", kind.place());
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
        self.sweep_torn(TornKind::whole)
    }

    #[cfg(test)]
    pub(crate) fn set_aside_torn_by_the_reader_alone(&self) -> io::Result<Vec<SetAside>> {
        self.sweep_torn(TornKind::reads)
    }

    fn sweep_torn(
        &self,
        whole: fn(TornKind, &Path, &str, &str) -> bool,
    ) -> io::Result<Vec<SetAside>> {
        if !self.torn_files_possible()? {
            return Ok(Vec::new());
        }
        let mut torn: Vec<(String, Vec<u8>)> = Vec::new();
        for (kind, name, rel) in self.torn_kind_files()? {
            let path = self.path(&rel);
            let bytes = match std::fs::read(&path) {
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
        self.move_into_quarantine(&torn, "")
    }

    /// After an unclean stop in an earlier boot ([`StateDir::torn_files_possible`]), every whole in-flight entry is retired.
    pub(crate) fn retire_in_flight_of_an_earlier_boot(&self) -> io::Result<Vec<SetAside>> {
        if !self.torn_files_possible()? || self.attached_in_this_boot() {
            return Ok(Vec::new());
        }
        let mut whole: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, rel) in self.kind_files(TornKind::InFlight)? {
            let path = self.path(&rel);
            let bytes = match std::fs::read(&path) {
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
        self.move_into_quarantine(&whole, RETIRED_PREFIX)
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
            let tried = self
                .quarantine_tried
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(from)
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
                        match self.publish_quarantine_file(from, &to, bytes) {
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
                .remove(from);
            dirs.insert(path.parent().unwrap_or(&self.root).to_path_buf());
        }
        for dir in dirs {
            self.flush_dir(&dir)?;
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

    fn publish_quarantine_file(&self, from: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
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
            if flushes_media() {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "quarantine evidence is flushed before its source may be unlinked"
                )]
                file.sync_all()?;
            }
            std::fs::hard_link(&tmp, &target)
        })();
        if staged.is_ok() {
            self.quarantine_tried
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    from.to_string(),
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

    /// A bridge's run begins, after its sweep ([`StateDir::set_aside_torn`]) and before it writes any file of a [`TornKind`].
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
            let pin = std::fs::symlink_metadata(self.path(LOGPIN))
                .is_ok_and(|meta| meta.is_file())
                .then(|| LOGPIN.to_string());
            self.unflushed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(here.into_iter().map(|(_, _, rel)| rel).chain(pin));
        }
        let tier = self.open_tier(last, now);
        let mut flushed = Flushed::default();
        let mut bytes = open.render().into_bytes();
        bytes.push(b'\n');
        let flush = match (rules.flush_record, rules.flush_root) {
            (false, _) => None,
            (true, false) => Some(Flush::Ordered),
            (true, true) => Some(tier),
        };
        self.replace_bytes_as_with(
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
        Ok(flushed)
    }

    fn open_tier(&self, last: Option<LastRun>, now: Option<[u8; 16]>) -> aterm_durable::Flush {
        use aterm_durable::Flush;
        let ordered =
            last.is_some_and(|run| now.is_some() && run.boot.is_some() && run.boot != now);
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

    /// A clean stop: every file of a [`TornKind`] this run must flush ([`StateDir::begin_run`]), and the mirror's pin, flushed where it is, every such directory flushed (an unlink made since is kept), and only then `run` says the run stopped cleanly.
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
        self.read("halt-acked").and_then(|s| s.parse().ok())
    }

    /// Record the halt offset this node has answered.
    pub fn set_halt_acked(&self, off: u64) -> io::Result<()> {
        if self.halt_acked().is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write("halt-acked", &off.to_string())
    }

    /// The inbox-group commit a bridge owed when it exited with no broker connection to make it on (`Bridge::hand_over_inbox_commit`).
    #[must_use]
    pub fn inbox_owed(&self) -> Option<u64> {
        self.read("inbox-owed").and_then(|s| s.parse().ok())
    }

    /// Hand over the inbox-group commit through `off` ([`Self::inbox_owed`]).
    pub fn set_inbox_owed(&self, off: u64) -> io::Result<()> {
        if self.inbox_owed().is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write("inbox-owed", &off.to_string())
    }

    /// The inbox commit handed over has been made.
    pub fn clear_inbox_owed(&self) -> io::Result<()> {
        match self.remove_path(&self.path("inbox-owed")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Forget the halt watermark.
    pub fn clear_halt_acked(&self) -> io::Result<()> {
        match self.remove_path(&self.path("halt-acked")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Forget every offset this dir holds that the log this node reads now does not vouch for ([`LogWitness::replaced_by`]), and put each back where a reader of this log, whose head is `head`, resumes.
    pub fn forget_log_offsets(
        &self,
        head: u64,
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
            let body = std::fs::read_to_string(entry.path())?;
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
        let publication_resets = self.publication_offset_resets(keep_below)?;
        let in_flight = self.in_flight()?;
        let owed = self.inbox_owed();
        let deadlines: Vec<Deadline> = self
            .deadlines()
            .into_iter()
            .filter(|d| kept(d.off))
            .collect();
        let asked: Vec<Asked> = self.asked().into_iter().filter(|a| kept(a.off)).collect();
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
        for (name, text) in publication_resets {
            self.write(&name, &text)?;
        }
        self.set_self_acked(&head.checked_sub(1).into_iter().collect(), 1)?;
        self.clear_halt_acked()?;
        if !owed.is_some_and(kept) {
            self.clear_inbox_owed()?;
        }
        self.set_deadlines(&deadlines)?;
        self.set_asked(&asked)?;
        let mut dropped = Vec::new();
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
            if re.is_some_and(|off| fork.is_some_and(|f| off < f)) {
                self.replace(
                    &format!("inflight/{seq}"),
                    &InFlight { log, ..entry }.render(),
                    true,
                    0o600,
                )?;
                continue;
            }
            match self.remove_path(&self.path(&format!("inflight/{seq}"))) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {
                    moved = true;
                    dropped.push((seq, entry.subject));
                }
            }
        }
        if moved {
            self.flush_dir(&self.path("inflight"))?;
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
        let mut text = String::new();
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
        let text = match std::fs::read_to_string(self.path("pins")) {
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
        parse_topic_floors(&path, &std::fs::read_to_string(&path)?)
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
        let line = if via_key {
            format!("{seq} key")
        } else {
            seq.to_string()
        };
        self.write(&format!("sent/{sid}.{id}"), &line)
    }

    /// Forget a retired post's sequence.
    pub fn clear_post_seq(&self, sid: &str, id: u64) {
        let _timed = self.timed(crate::wait_tally::Row::ClearPostSeq);
        let _ = self.unlink(&format!("sent/{sid}.{id}"));
    }

    /// The producer sequence this bridge reserved for one owed receipt, if any.
    #[must_use]
    pub fn receipt_seq(&self, sid: &str, rid: u64) -> Option<u64> {
        self.receipt_pin(sid, rid).ok().flatten().map(|pin| pin.seq)
    }

    /// Reserve a sequence for one owed receipt before it is published ([`StateDir::receipt_seq`]).
    pub fn set_receipt_seq(&self, sid: &str, rid: u64, seq: u64) -> io::Result<()> {
        self.write(&format!("acks/{sid}.{rid}"), &seq.to_string())
    }

    /// Forget a retired receipt's sequence.
    pub fn clear_receipt_seq(&self, sid: &str, rid: u64) {
        let _ = self.remove_path(&self.path(&format!("acks/{sid}.{rid}")));
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

    /// Every deadline this node still owes a verdict on, oldest offset first.
    #[must_use]
    pub fn deadlines(&self) -> Vec<Deadline> {
        self.read("deadlines")
            .map(|s| s.lines().filter_map(Deadline::parse).collect())
            .unwrap_or_default()
    }

    /// Every ask this node remembers the recipient of, oldest offset first.
    #[must_use]
    pub fn asked(&self) -> Vec<Asked> {
        self.read("asked")
            .map(|s| s.lines().filter_map(Asked::parse).collect())
            .unwrap_or_default()
    }

    /// Replace the asked table, keeping the newest [`ASKED_KEEP`] by position.
    pub fn set_asked(&self, list: &[Asked]) -> io::Result<()> {
        let over = list.len().saturating_sub(ASKED_KEEP);
        let lines: Vec<String> = list[over..].iter().map(Asked::render).collect();
        self.write("asked", &lines.join("\n"))
    }

    /// Replace the deadline table, keeping the newest [`DEADLINES_KEEP`] by position.
    pub fn set_deadlines(&self, list: &[Deadline]) -> io::Result<()> {
        let over = list.len().saturating_sub(DEADLINES_KEEP);
        let lines: Vec<String> = list[over..].iter().map(Deadline::render).collect();
        self.write("deadlines", &lines.join("\n"))
    }

    /// The directory itself, for what this type does not model.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("atlink-state-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn base(inc: u64) -> u64 {
        astream_broker::incarnation_base(inc).expect("in the layout")
    }

    fn will(inc: u64) -> u64 {
        astream_broker::will_seq(inc).expect("in the layout")
    }

    #[test]
    fn every_reserved_sequence_sits_in_astreams_incarnation_layout() {
        let dir = scratch("layout");
        let st = StateDir::open(&dir).expect("open");
        for inc in [0, 1, 7, INC_MAX] {
            let block = st.reserve_sequence_block(inc, 3).expect("a block");
            let base = base(inc);
            let will = will(inc);
            assert!(
                base < *block.start() && *block.end() < will,
                "inc {inc}: {block:?} strictly inside ({base}, {will})"
            );
            assert_eq!(astream_broker::incarnation_of(*block.end()), Some(inc));
            assert_eq!(
                st.incarnation(),
                inc,
                "the dir answers for the block's incarnation"
            );
        }
        assert!(
            st.reserve_sequence_block(INC_MAX + 1, 1).is_err(),
            "past INC_MAX the layout has no space"
        );
        let corrupt = scratch("layout-ack-half");
        let st = StateDir::open(&corrupt).expect("open");
        std::fs::write(
            corrupt.join("seq"),
            (astream_broker::ACK_SEQ_BASE | 5).to_string(),
        )
        .expect("seed a seq in the ack half");
        assert!(
            st.take_incarnation(0).is_err(),
            "a seq no incarnation owns refuses the take; it is never incarnation 0"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&corrupt);
    }

    #[test]
    fn a_sequence_block_is_on_disk_before_use_and_never_reaches_the_wills_number() {
        let dir = scratch("seqblock");
        let st = StateDir::open(&dir).expect("open");
        let block = st.reserve_sequence_block(3, 64).expect("a block");
        assert_eq!(block, base(3) + 1..=base(3) + 64);
        assert_eq!(
            st.sequence(),
            *block.end(),
            "the END is what the file holds"
        );
        let reopened = StateDir::open(&dir).expect("reopen");
        assert_eq!(
            reopened.reserve_next_sequence(3).expect("one more"),
            block.end() + 1,
            "a reopen starts above the whole block, however little of it was used"
        );
        assert_eq!(
            reopened.reserve_sequence_block(4, 2).expect("inc 4"),
            base(4) + 1..=base(4) + 2
        );
        std::fs::write(dir.join("seq"), (will(5) - 3).to_string()).expect("seed near the top");
        let short = reopened
            .reserve_sequence_block(5, 64)
            .expect("a short block");
        assert_eq!(short, will(5) - 2..=will(5) - 1);
        assert!(
            reopened.reserve_sequence_block(5, 64).is_err(),
            "nothing below the will's number is left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_flush_tolerates_a_gone_directory_and_refuses_a_broken_path() {
        use std::os::unix::fs::PermissionsExt as _;
        struct RestoreMode(PathBuf, std::fs::Permissions);
        impl Drop for RestoreMode {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, self.1.clone());
            }
        }
        let owned = aterm_tempfile::Builder::new()
            .prefix("syncdir")
            .tempdir_in("/tmp")
            .expect("scratch");
        let dir = owned.path();
        let locked = dir.join("locked");
        std::fs::create_dir_all(&locked).expect("scratch");
        let flush = aterm_durable::Flush::Ordered;
        assert!(sync_dir(dir).is_ok());
        assert!(sync_dir_required_as(dir, flush).is_ok());
        assert!(sync_dir(&dir.join("gone")).is_ok());
        assert_eq!(
            sync_dir_required_as(&dir.join("gone"), flush).map_err(|e| e.kind()),
            Err(io::ErrorKind::NotFound)
        );
        let known = locked.join("known");
        std::fs::write(&known, b"still reachable").expect("a known file");
        let restore = RestoreMode(
            locked.clone(),
            std::fs::metadata(&locked).expect("its mode").permissions(),
        );
        let mode = |m| std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(m));
        mode(0o300).expect("write but not read");
        let file = std::fs::read(&known);
        let direct = std::fs::File::open(&locked)
            .map(|_| ())
            .map_err(|e| e.kind());
        let required = sync_dir_required_as(&locked, flush).map_err(|e| e.kind());
        let opened = sync_dir(&locked);
        drop(restore);
        assert_eq!(
            file.expect("known file remains readable"),
            b"still reachable"
        );
        assert_eq!(
            required, direct,
            "required flush preserves the directory-open result, even when only known files are reachable"
        );
        assert!(opened.is_ok());
        std::fs::write(dir.join("file"), b"").expect("a file");
        assert_eq!(
            sync_dir(&dir.join("file").join("dir")).map_err(|e| e.kind()),
            Err(io::ErrorKind::NotADirectory)
        );
    }

    #[test]
    fn the_node_id_is_minted_once_and_survives_reopen() {
        let dir = scratch("node");
        let first = StateDir::open(&dir)
            .expect("open")
            .node_id(|| "n-0123456789abcdef".to_string())
            .expect("mint");
        let second = StateDir::open(&dir)
            .expect("reopen")
            .node_id(|| panic!("must not re-mint"))
            .expect("read back");
        assert_eq!(first, second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_node_file_that_holds_no_node_id_is_kept_aside_and_an_id_minted() {
        let dir = scratch("node-aside");
        let st = StateDir::open(&dir).expect("open");
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
        std::fs::write(dir.join("node"), "h-x\n").expect("a person's id where the node's goes");
        assert_eq!(
            st.node_id(|| "n-0123456789abcdef".to_string())
                .expect("mint"),
            "n-0123456789abcdef"
        );
        assert_eq!(read("node").as_deref(), Some("n-0123456789abcdef\n"));
        assert_eq!(
            read("node.aside").as_deref(),
            Some("h-x\n"),
            "kept, not overwritten"
        );
        std::fs::write(dir.join("node"), [0xff, b'\n']).expect("bytes that are not text");
        assert_eq!(st.node_id(|| "n-m7".to_string()).expect("mint"), "n-m7");
        assert_eq!(
            std::fs::read(dir.join("node.aside.1")).ok(),
            Some(vec![0xff, b'\n'])
        );
        assert_eq!(
            read("node.aside").as_deref(),
            Some("h-x\n"),
            "the first kept as it was"
        );
        assert_eq!(
            st.node_id(|| panic!("must not re-mint")).expect("kept"),
            "n-m7"
        );
        assert!(read("node.aside.2").is_none(), "a node id is not set aside");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_node_file_with_bytes_that_the_new_id_replaced_stays_aside() {
        let dir = scratch("node-take-back");
        let st = StateDir::open(&dir).expect("open");
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
        std::fs::write(dir.join("node"), "").expect("an empty node file");
        assert_eq!(st.node_id(|| "n-e".to_string()).expect("mint"), "n-e");
        assert!(
            read("node.aside").is_none(),
            "an empty node file held nothing to keep"
        );

        std::fs::write(dir.join("node"), "h-x\n").expect("a person's id where the node's goes");
        st.refuse_writes_of(Some("node"));
        for _ in 0..2 {
            assert!(
                st.node_id(|| "n-a".to_string()).is_err(),
                "the write is refused"
            );
            assert_eq!(read("node").as_deref(), Some("h-x\n"), "left as it was");
            assert!(
                read("node.aside").is_none(),
                "a failed write takes its link back"
            );
        }
        st.refuse_writes_of(None);
        st.set_inc_seam(IncSeam {
            kill_at: Some(IncStep::Renamed),
            ..Default::default()
        });
        assert!(
            st.node_id(|| "n-b".to_string()).is_err(),
            "the flush after the rename fails"
        );
        st.set_inc_seam(IncSeam::default());
        assert_eq!(read("node").as_deref(), Some("n-b\n"));
        assert_eq!(
            read("node.aside").as_deref(),
            Some("h-x\n"),
            "once the write replaced the file, its link is the only copy and stays"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_topic_cursor_line_parses_or_is_dropped_beside_its_neighbours() {
        let c = |next| TopicCursor { next };
        assert_eq!(
            TopicCursor::parse("build.failed 42"),
            Some(("build.failed".to_string(), c(42)))
        );
        assert_eq!(
            TopicCursor::parse("old.topic 3 7"),
            Some(("old.topic".to_string(), c(3)))
        );
        for bad in [
            "",
            "onlytopic",
            "t x",
            "t 1 y",
            "t 1 2 3",
            "t +5",
            "T 5",
            "a/b 5",
            "a* 5",
            "> 5",
        ] {
            assert_eq!(TopicCursor::parse(bad), None, "{bad:?}");
        }
        let dir = scratch("topics");
        let st = StateDir::open(&dir).expect("open");
        let mut good = BTreeMap::new();
        good.insert("a".to_string(), c(10));
        good.insert("b".to_string(), c(20));
        st.set_topics("s-x", &good).expect("write");
        assert_eq!(st.topics("s-x"), good);
        std::fs::write(
            st.path("topics/s-x"),
            "a 10\nthis is not a cursor \x1b[2J\nb 20\n",
        )
        .expect("corrupt one line");
        assert_eq!(st.topics("s-x"), good, "the neighbours survive");
        st.forget_topics("s-x");
        assert!(st.topics("s-x").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// EVERY FILE THE STATE DIRECTORY HOLDS IS THE OWNER'S ALONE ([`STATE_FILE_MODE`]): a post's
    /// `key=` and a session's topic names are caller text, kept no wider than the 0600 journal
    /// keeps the key, whatever the umask lets a plain create make.
    #[test]
    fn every_file_the_state_directory_holds_is_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("modes");
        let st = StateDir::open(&dir).expect("open");
        st.set_key_seq("s-x", "QZP6KEY", 7).expect("a key");
        let mut topics = BTreeMap::new();
        topics.insert("build.done".to_string(), TopicCursor { next: 3 });
        st.set_topics("s-x", &topics).expect("topics");
        let mut floors = BTreeMap::new();
        floors.insert("build.done".to_string(), 3);
        st.set_topic_floors("s-x", &floors).expect("floors");
        st.set_seen_off("s-x", 9).expect("seen");
        st.set_halt_acked(4).expect("halt-acked");
        st.set_inbox_owed(5).expect("inbox-owed");
        st.set_self_acked(&std::collections::BTreeSet::from([1, 2]), 8)
            .expect("acked");
        st.append_self_acked(3).expect("acked, appended");
        st.raise_logfloor(11).expect("logfloor");
        let mut seen = Vec::new();
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).expect("a dir").flatten() {
                let meta = e.metadata().expect("meta");
                if meta.is_dir() {
                    stack.push(e.path());
                } else {
                    let rel = e.path().strip_prefix(&dir).expect("under").to_path_buf();
                    seen.push((rel, meta.permissions().mode() & 0o777));
                }
            }
        }
        seen.sort();
        for want in [
            "keys/s-x",
            "topics/s-x",
            "unhandled/s-x",
            "acked",
            "logfloor",
        ] {
            assert!(
                seen.iter().any(|(rel, _)| rel == Path::new(want)),
                "{want} written: {seen:?}"
            );
        }
        let wide: Vec<_> = seen.iter().filter(|(_, m)| m & 0o077 != 0).collect();
        assert!(wide.is_empty(), "files others may read: {wide:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broadcast_refill_floor_round_trips_and_goes_with_its_cursors_and_its_log() {
        let dir = scratch("unhandled");
        let st = StateDir::open(&dir).expect("open");
        let mut floors = BTreeMap::new();
        floors.insert("a".to_string(), 10);
        floors.insert("b.c".to_string(), 7);
        st.set_topic_floors("s-x", &floors).expect("write");
        assert_eq!(st.topic_floors("s-x").unwrap(), floors);
        std::fs::write(
            st.path("unhandled/s-x"),
            "a 10\nb.c +7\nB 3\nb.c 7\nd 1 2\n",
        )
        .expect("damage some lines");
        assert!(
            st.topic_floors("s-x").is_err(),
            "damaged state is not an empty floor"
        );
        st.set_topic_floors("s-x", &floors).expect("repair");
        assert_eq!(st.topic_floors("s-x").unwrap(), floors);

        st.set_topic_floors("s-x", &BTreeMap::new())
            .expect("an empty set");
        assert!(
            !st.path("unhandled/s-x").exists(),
            "an empty set is no file"
        );
        assert!(st.set_topic_floors("s-x", &BTreeMap::new()).is_ok());

        st.set_topic_floors("s-x", &floors).expect("write");
        st.forget_topics("s-x");
        assert!(
            st.topic_floors("s-x").unwrap().is_empty(),
            "a departed session's floors go"
        );

        st.set_topic_floors("s-y", &floors).expect("write");
        st.forget_log_offsets(100, None, None, None)
            .expect("re-base");
        assert!(
            st.topic_floors("s-y").unwrap().is_empty(),
            "a floor names a record of the old log"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_seen_watermark_never_goes_backwards() {
        let dir = scratch("seen");
        let st = StateDir::open(&dir).expect("open");
        st.set_seen_off("s-a", 42).expect("write");
        st.set_seen_off("s-a", 7).expect("write");
        assert_eq!(st.seen_off("s-a"), Some(42));
        st.set_seen_off("s-a", 43).expect("write");
        assert_eq!(st.seen_off("s-a"), Some(43));
        assert_eq!(st.seen_off("s-unknown"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_self_acked_set_is_a_set_and_is_bounded() {
        let dir = scratch("acked");
        let st = StateDir::open(&dir).expect("open");
        let mut set = std::collections::BTreeSet::new();
        for off in [10u64, 30, 20] {
            set.insert(off);
            st.set_self_acked(&set, 3).expect("write");
        }
        assert_eq!(st.self_acked(), [10, 20, 30].into_iter().collect());
        assert_eq!(st.halt_acked(), None);
        st.set_halt_acked(91).expect("write");
        st.set_halt_acked(7).expect("write");
        assert_eq!(st.halt_acked(), Some(91));
        st.set_halt_acked(92).expect("write");
        assert_eq!(st.halt_acked(), Some(92));
        set.insert(40);
        st.set_self_acked(&set, 3).expect("write");
        assert_eq!(
            st.self_acked(),
            [20, 30, 40].into_iter().collect(),
            "the oldest is dropped, and 10 is now unknown rather than proven ours"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pin_is_first_wins_and_reports_the_pinned_node() {
        let dir = scratch("pins");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.pin("s-a", "n-one").expect("pin"), "n-one");
        assert_eq!(
            st.pin("s-a", "n-two").expect("pin"),
            "n-one",
            "a second node claiming a pinned sid is a conflict, not a re-pin"
        );
        assert_eq!(
            StateDir::open(&dir).expect("reopen").pins().get("s-a"),
            Some(&"n-one".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_posts_sequence_is_pinned_to_the_post_and_survives_a_reopen() {
        let dir = scratch("postseq");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.post_seq("s-a", 1), None);
        st.set_post_seq("s-a", 1, 0x0000_0002_0000_0007, false)
            .expect("reserve");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").post_seq("s-a", 1),
            Some((0x0000_0002_0000_0007, false))
        );
        assert_eq!(st.post_seq("s-a", 2), None);
        st.clear_post_seq("s-a", 1);
        assert_eq!(st.post_seq("s-a", 1), None);
        st.set_post_seq("s-a", 3, 0x0000_0002_0000_0009, true)
            .expect("reserve via key");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").post_seq("s-a", 3),
            Some((0x0000_0002_0000_0009, true))
        );
        std::fs::write(dir.join("sent/s-a.4"), "17\n").expect("write");
        assert_eq!(st.post_seq("s-a", 4), Some((17, false)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_receipt_pins_its_sequence_until_it_is_retired() {
        let dir = scratch("acks");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.receipt_seq("s-a", 1), None);
        st.set_receipt_seq("s-a", 1, 77).expect("reserve");
        st.set_receipt_seq("s-a", 2, 78).expect("reserve");
        st.set_receipt_seq("s-b", 1, 79).expect("reserve");
        let again = StateDir::open(&dir).expect("reopen");
        assert_eq!(again.receipt_seq("s-a", 1), Some(77), "survives a reopen");
        assert_eq!(again.receipt_seq("s-a", 2), Some(78));
        assert_eq!(again.receipt_seq("s-b", 1), Some(79), "per session");
        again.clear_receipt_seq("s-a", 1);
        assert_eq!(again.receipt_seq("s-a", 1), None, "retired");
        assert_eq!(again.receipt_seq("s-a", 2), Some(78), "the others stand");
        assert!(
            std::fs::read_to_string(dir.join("acks/s-a.2"))
                .expect("the pin file")
                .trim()
                .parse::<u64>()
                .is_ok(),
            "a pin holds the number and nothing else"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn publication_identity_survives_pins_and_malformed_reservations_are_not_absent() {
        let dir = scratch("identities");
        let st = StateDir::open(&dir).unwrap();
        let pin = PublishPin {
            seq: 17,
            via_key: true,
            identity: Some(PublishIdentity {
                subject: "/f/f/in/n-a/s-b/n-c/ask".into(),
                digest: "a".repeat(64),
            }),
            original: Some(PublishIdentity {
                subject: "/f/f/in/n-d/s-e/n-c/note".into(),
                digest: "b".repeat(64),
            }),
            original_off: Some(0),
            owner: Some(1),
        };
        st.set_post_pin("s-a", 1, &pin).unwrap();
        assert_eq!(
            StateDir::open(&dir).unwrap().post_pin("s-a", 1).unwrap(),
            Some(pin.clone())
        );
        for damaged in [
            "",
            "17 identity /f/f/x nope",
            "17 identity /f/f/x",
            "17 garbage",
            "17 owner 1 owner 2",
            "17 original-off nope",
        ] {
            st.write("sent/s-a.1", damaged).unwrap();
            assert_eq!(
                st.post_pin("s-a", 1).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        for damaged in [
            "key-only",
            "key-only garbage",
            "key-only 17 identity /f/f/x bad",
        ] {
            st.write("keys/s-a", damaged).unwrap();
            assert_eq!(
                st.key_pin("s-a", "key-only").unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        st.write("keys/s-a", "key-only\t17").unwrap();
        assert_eq!(st.key_pin("s-a", "key-only").unwrap().unwrap().seq, 17);
        st.forget_key_seq("s-a", "key-only", 17).unwrap();
        assert!(st.key_pin("s-a", "key-only").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_flight_entries_round_trip_in_order_and_a_damaged_one_is_not_absent() {
        let dir = scratch("inflight");
        let st = StateDir::open(&dir).unwrap();
        assert!(
            st.in_flight().unwrap().is_empty(),
            "no dir is nothing in flight"
        );
        let entry = |seq: u64, body: &[u8]| InFlight {
            pin: PublishPin {
                seq,
                via_key: false,
                identity: Some(PublishIdentity {
                    subject: "/f/f/in/n-a/s-b/n-a/note".into(),
                    digest: "c".repeat(64),
                }),
                original: None,
                original_off: None,
                owner: Some(3),
            },
            subject: "/f/f/in/n-a/s-b/n-a/note".into(),
            body: body.to_vec(),
            log: None,
        };
        let late = InFlight {
            log: Some(astream_broker::LogIdentity {
                id: [0xaa; 16],
                generation: 2,
            }),
            ..entry(0x0000_0002_0000_0009, b"v=1 t=7 text=two%0Alines\n\x00\xff")
        };
        let early = entry(0x0000_0002_0000_0002, b"");
        st.set_in_flight(&late).unwrap();
        st.set_in_flight(&early).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(dir.join(format!("inflight/{}", late.pin.seq)))
                .unwrap()
                .permissions(),
        );
        assert_eq!(
            mode & 0o077,
            0,
            "a record's bytes are the owner's: {mode:o}"
        );
        std::fs::write(dir.join("inflight/.9.tmp.1.abcd"), "junk").unwrap();
        assert_eq!(
            StateDir::open(&dir).unwrap().in_flight().unwrap(),
            vec![(early.pin.seq, early.clone()), (late.pin.seq, late.clone())]
        );
        for damaged in [
            "",
            "8589934601",
            "8589934601\n/f/f/x",
            "8589934601\n/f/f/x\nnot-hex",
            "8589934601\n/f/f/*\n00",
            "8589934601\n/f/f/x\n00\nextra",
            "8589934601\n/f/f/x\n00\nlog=",
            "8589934601\n/f/f/x\n00\nlog=AA.0",
            "8589934601\n/f/f/x\n00\nlog=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.0\nextra",
            "8589934600\n/f/f/x\n00",
        ] {
            st.write(&format!("inflight/{}", late.pin.seq), damaged)
                .unwrap();
            assert_eq!(
                st.in_flight().unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "{damaged:?} must block recovery"
            );
            assert!(
                dir.join(format!("inflight/{}", late.pin.seq)).exists(),
                "uncertain publication fate must not discard its journal"
            );
        }
        st.clear_in_flight(late.pin.seq).unwrap();
        st.clear_in_flight(early.pin.seq).unwrap();
        assert!(st.in_flight().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_acked_log_is_appended_reads_an_older_file_and_never_takes_a_torn_tail() {
        let dir = scratch("acked-log");
        let st = StateDir::open(&dir).unwrap();
        let set = |offs: &[u64]| {
            offs.iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            st.self_acked_log(),
            AckedLog {
                appendable: true,
                ..AckedLog::default()
            },
            "no file is the empty log, appendable"
        );
        for off in [5, 7, 9, 7] {
            st.append_self_acked(off).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("acked")).unwrap(),
            "5,7,9,7,"
        );
        assert_eq!(
            st.self_acked_log(),
            AckedLog {
                offsets: set(&[5, 7, 9]),
                entries: 4,
                appendable: true,
            }
        );
        let older = |text: &str| {
            std::fs::write(dir.join("acked"), text).unwrap();
            st.self_acked_log()
        };
        assert_eq!(
            older("1,2,3\n"),
            AckedLog {
                offsets: set(&[1, 2, 3]),
                entries: 3,
                appendable: false,
            },
            "an older bridge's line"
        );
        assert_eq!(
            older("5,7,45"),
            AckedLog {
                offsets: set(&[5, 7]),
                entries: 2,
                appendable: false,
            },
            "45 may be the torn head of 456: it is no offset of ours"
        );
        assert_eq!(older("").offsets, set(&[]));
        st.set_self_acked(&set(&[20, 30, 40]), 2).unwrap();
        let text = std::fs::read_to_string(dir.join("acked")).unwrap();
        assert_eq!(text, "30,40,", "the newest two, each ended by its comma");
        let older_reader: std::collections::BTreeSet<u64> = text
            .trim()
            .split(',')
            .filter_map(|n| n.parse().ok())
            .collect();
        assert_eq!(
            older_reader,
            set(&[30, 40]),
            "an older bridge reads the same set"
        );
        assert!(st.self_acked_log().appendable);
        std::fs::remove_file(dir.join("acked")).unwrap();
        let elsewhere = dir.join("elsewhere");
        std::fs::write(&elsewhere, "").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("acked")).unwrap();
        assert!(
            st.append_self_acked(1).is_err(),
            "a planted link is refused"
        );
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_acked_append_preserves_a_file_with_another_name() {
        let scratch = aterm_tempfile::TempDir::new().expect("exclusive state scratch");
        let root = std::fs::canonicalize(scratch.path()).expect("canonical state scratch");
        let ordinary = StateDir::open(root.join("ordinary")).expect("ordinary state");
        ordinary
            .append_self_acked(5)
            .expect("first ordinary append");
        ordinary
            .append_self_acked(7)
            .expect("second ordinary append");
        assert_eq!(
            std::fs::read(ordinary.path("acked")).expect("ordinary log"),
            b"5,7,",
            "ordinary append controls precede the occupied-file case"
        );

        let linked = StateDir::open(root.join("linked")).expect("linked state");
        let other = root.join("other-owned-file");
        const SENTINEL: &[u8] = b"the other file owns these bytes\n";
        std::fs::write(&other, SENTINEL).expect("owned sentinel");
        std::fs::hard_link(&other, linked.path("acked")).expect("owned hardlink");
        let result = linked.append_self_acked(42);
        let other_bytes = std::fs::read(&other).expect("other file remains readable");
        let state_bytes = std::fs::read(linked.path("acked")).expect("state name remains readable");
        assert!(
            result.is_err() && other_bytes == SENTINEL && state_bytes == SENTINEL,
            "ACKED_APPEND_REQUIRES_OWNED_SINGLE_LINK_FILE: result={result:?}, other={other_bytes:?}, state={state_bytes:?}"
        );
    }

    #[test]
    fn a_re_base_drops_proven_old_references_and_keeps_uncertain_or_unrelated_mail() {
        use astream_broker::LogIdentity;
        let (a, b) = (
            LogIdentity {
                id: [0xaa; 16],
                generation: 0,
            },
            LogIdentity {
                id: [0xbb; 16],
                generation: 0,
            },
        );
        let dir = scratch("inflight-rebase");
        let st = StateDir::open(&dir).unwrap();
        let face = "/f/f/in/n-a/s-b/n-a/expired";
        let entry = |seq: u64, log: Option<LogIdentity>| InFlight {
            pin: PublishPin {
                seq,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
            },
            subject: face.into(),
            body: format!("v=1 t=1 re={seq}").into_bytes(),
            log,
        };
        st.set_in_flight(&entry(1, Some(a))).unwrap();
        st.set_in_flight(&entry(2, None)).unwrap();
        st.set_in_flight(&entry(3, Some(b))).unwrap();
        let note = InFlight {
            body: b"v=1 t=1 text=still-owed".to_vec(),
            ..entry(4, Some(a))
        };
        st.set_in_flight(&note).unwrap();
        assert_eq!(
            st.forget_log_offsets(100, Some(b), None, None).unwrap(),
            vec![(1, face.to_string())]
        );
        let kept = vec![(2, entry(2, None)), (3, entry(3, Some(b))), (4, note)];
        assert_eq!(st.in_flight().unwrap(), kept);
        assert!(
            st.forget_log_offsets(100, Some(b), None, None)
                .unwrap()
                .is_empty(),
            "idempotent"
        );
        assert!(
            st.forget_log_offsets(7, None, None, None)
                .unwrap()
                .is_empty(),
            "no identity proves no mismatch"
        );
        assert_eq!(st.in_flight().unwrap(), kept, "uncertainty is retained");
        st.set_seen_off("s-b", 44).unwrap();
        st.write("inflight/5", "damaged").unwrap();
        assert_eq!(
            st.forget_log_offsets(7, Some(b), None, None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(dir.join("inflight/5").exists(), "damage is kept for repair");
        assert_eq!(st.seen_off("s-b"), Some(44), "preflight changed nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_moved_generation_clamps_the_offsets_it_continues_and_forgets_the_rest() {
        use astream_broker::LogIdentity;
        let old = LogIdentity {
            id: [0xaa; 16],
            generation: 0,
        };
        let new = LogIdentity {
            generation: 1,
            ..old
        };
        let pin = |original_off: Option<u64>| PublishPin {
            seq: 1,
            via_key: true,
            identity: original_off.map(|_| PublishIdentity {
                subject: "/f/f/in/n-a/s-b/n-c/ask".into(),
                digest: "a".repeat(64),
            }),
            original: None,
            original_off,
            owner: None,
        };
        let in_flight = |seq: u64, re: u64| InFlight {
            pin: PublishPin { seq, ..pin(None) },
            subject: "/f/f/in/n-a/s-b/n-a/answer".into(),
            body: format!("v=1 t=1 re={re}").into_bytes(),
            log: Some(old),
        };
        let deadline = |off| Deadline {
            off,
            sid: "s-x".into(),
            at: 9_000,
            dl: 60,
            to: None,
        };
        let asked = |off| Asked {
            off,
            to: "n-b".into(),
        };
        let cursors = |pairs: &[(&str, u64)]| -> BTreeMap<String, TopicCursor> {
            pairs
                .iter()
                .map(|(t, next)| ((*t).to_string(), TopicCursor { next: *next }))
                .collect()
        };
        let floors = |pairs: &[(&str, u64)]| -> BTreeMap<String, u64> {
            pairs
                .iter()
                .map(|(t, off)| ((*t).to_string(), *off))
                .collect()
        };
        let fill = |st: &StateDir| {
            st.set_seen_off("s-low", 3).unwrap();
            st.set_seen_off("s-high", 9).unwrap();
            st.set_topics("s-x", &cursors(&[("a", 2), ("b", 8)]))
                .unwrap();
            st.set_topic_floors("s-x", &floors(&[("a", 1), ("b", 7)]))
                .unwrap();
            st.set_topic_floors("s-y", &floors(&[("c", 6)])).unwrap();
            st.set_halt_acked(2).unwrap();
            st.set_inbox_owed(4).unwrap();
            st.set_deadlines(&[deadline(3), deadline(6)]).unwrap();
            st.set_asked(&[asked(3), asked(6)]).unwrap();
            st.set_key_pin("s-x", "low", &pin(Some(1))).unwrap();
            st.set_key_pin("s-x", "high", &pin(Some(6))).unwrap();
            st.set_in_flight(&in_flight(1, 2)).unwrap();
            st.set_in_flight(&in_flight(2, 7)).unwrap();
            st.set_self_acked(&[1, 6].into_iter().collect(), 8).unwrap();
        };
        let answer = |seq: u64| (seq, "/f/f/in/n-a/s-b/n-a/answer".to_string());

        let dir = scratch("clamp");
        let st = StateDir::open(&dir).unwrap();
        fill(&st);
        assert_eq!(
            st.forget_log_offsets(12, Some(new), Some(5), None).unwrap(),
            vec![answer(2)],
            "only the in-flight reply to a record the loss took goes"
        );
        let clamped = |st: &StateDir| {
            assert_eq!(st.seen_off("s-low"), Some(3), "below the fork: kept");
            assert_eq!(st.seen_off("s-high"), Some(4), "at the fork");
            assert_eq!(st.topics("s-x"), cursors(&[("a", 2), ("b", 5)]));
            assert_eq!(st.topic_floors("s-x").unwrap(), floors(&[("a", 1)]));
            assert!(st.topic_floors("s-y").unwrap().is_empty());
            assert_eq!(st.halt_acked(), None, "cleared, clamped or not");
            assert_eq!(
                st.inbox_owed(),
                Some(4),
                "the commit owed is below the fork"
            );
            assert_eq!(st.deadlines(), [deadline(3)]);
            assert_eq!(st.asked(), [asked(3)]);
            assert_eq!(
                st.key_pin("s-x", "low").unwrap().unwrap().original_off,
                Some(1)
            );
            assert_eq!(
                st.key_pin("s-x", "high").unwrap().unwrap().original_off,
                None
            );
            assert_eq!(
                st.in_flight().unwrap(),
                vec![(
                    1,
                    InFlight {
                        log: Some(new),
                        ..in_flight(1, 2)
                    }
                )],
                "the reply below the fork is kept, RE-WITNESSED onto the new generation: the \
                 attach replays a reference only on the log its witness names"
            );
            assert_eq!(
                st.self_acked_log().offsets,
                [11].into_iter().collect(),
                "the window re-bases at the head"
            );
        };
        clamped(&st);
        assert!(
            st.forget_log_offsets(12, Some(new), Some(5), None)
                .unwrap()
                .is_empty(),
            "idempotent"
        );
        clamped(&st);
        st.set_topics("s-z", &cursors(&[("d", 30)])).unwrap();
        st.forget_log_offsets(12, Some(new), Some(40), None)
            .unwrap();
        assert_eq!(st.topics("s-z"), cursors(&[("d", 12)]));

        let twin = scratch("clamp-ctl");
        let st = StateDir::open(&twin).unwrap();
        fill(&st);
        assert_eq!(
            st.forget_log_offsets(12, Some(new), None, None).unwrap(),
            vec![answer(1), answer(2)]
        );
        assert_eq!((st.seen_off("s-low"), st.seen_off("s-high")), (None, None));
        assert_eq!(st.topics("s-x"), cursors(&[("a", 12), ("b", 12)]));
        assert!(st.topic_floors("s-x").unwrap().is_empty());
        assert_eq!(st.halt_acked(), None);
        assert_eq!(st.inbox_owed(), None);
        assert!(st.deadlines().is_empty() && st.asked().is_empty());
        assert_eq!(
            st.key_pin("s-x", "low").unwrap().unwrap().original_off,
            None
        );
        assert!(st.in_flight().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&twin);
    }

    #[test]
    fn an_in_flight_reply_is_kept_only_below_the_fork_of_the_log_it_was_witnessed_on() {
        let log = |generation| astream_broker::LogIdentity {
            id: [0x3c; 16],
            generation,
        };
        let lineage = astream_broker::Lineage::from_steps(&[(1, 41), (2, 138)])
            .expect("a lineage astream reads");
        let entry = |seq: u64, re: u64, generation: u64| InFlight {
            pin: PublishPin {
                seq,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
            },
            subject: "/f/f/in/n-a/s-b/n-a/ack".into(),
            body: format!("v=1 t=1 re={re}").into_bytes(),
            log: Some(log(generation)),
        };
        let fill = |st: &StateDir| {
            st.set_in_flight(&entry(1, 40, 0)).unwrap();
            st.set_in_flight(&entry(2, 41, 0)).unwrap();
            st.set_in_flight(&entry(3, 137, 1)).unwrap();
            st.set_in_flight(&entry(4, 138, 1)).unwrap();
        };
        let ack = |seq: u64| (seq, "/f/f/in/n-a/s-b/n-a/ack".to_string());
        let dir = scratch("own-fork");
        let st = StateDir::open(&dir).unwrap();
        fill(&st);
        let keep_below = LogWitness {
            high: 150,
            id: Some(log(1)),
        }
        .continues(Some(log(2)), Some(&lineage));
        assert_eq!(keep_below, Some(138), "the newest move's fork");
        assert_eq!(
            st.forget_log_offsets(150, Some(log(2)), keep_below, Some(&lineage))
                .unwrap(),
            vec![ack(2), ack(4)],
            "generation 0's reply at 41 and generation 1's at 138 go"
        );
        let rewitnessed = |seq, re, generation| {
            (
                seq,
                InFlight {
                    log: Some(log(2)),
                    ..entry(seq, re, generation)
                },
            )
        };
        assert_eq!(
            st.in_flight().unwrap(),
            vec![rewitnessed(1, 40, 0), rewitnessed(3, 137, 1)],
            "each kept below its own fork, re-witnessed onto generation 2"
        );
        let twin = scratch("own-fork-none");
        let st = StateDir::open(&twin).unwrap();
        fill(&st);
        assert_eq!(
            st.forget_log_offsets(150, Some(log(2)), keep_below, None)
                .unwrap(),
            vec![ack(4)]
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&twin);
    }

    #[test]
    fn a_key_pins_its_sequence_first_wins_and_the_4097th_evicts_the_oldest() {
        let dir = scratch("keys");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.key_seq("s-a", "k-1"), None);
        assert_eq!(st.set_key_seq("s-a", "k-1", 41).expect("reserve"), 41);
        assert_eq!(
            StateDir::open(&dir).expect("reopen").key_seq("s-a", "k-1"),
            Some(41),
            "the reservation survives a reopen"
        );
        assert_eq!(st.set_key_seq("s-a", "k-1", 99).expect("re-reserve"), 41);
        assert_eq!(st.key_seq("s-a", "k-1"), Some(41));
        assert_eq!(st.key_seq("s-b", "k-1"), None);
        assert_eq!(st.set_key_seq("s-b", "k-1", 7).expect("reserve"), 7);
        assert_eq!(st.key_seq("s-a", "k-1"), Some(41));

        let full: Vec<String> = (0..KEYS_KEEP).map(|n| format!("k-{n} {n}")).collect();
        st.write("keys/s-c", &full.join("\n"))
            .expect("seed a full table");
        assert_eq!(st.key_seq("s-c", "k-0"), Some(0));
        assert_eq!(
            st.key_seq("s-c", &format!("k-{}", KEYS_KEEP - 1)),
            Some(KEYS_KEEP as u64 - 1)
        );
        assert_eq!(
            st.set_key_seq("s-c", "k-newest", 1_000_000)
                .expect("the 4097th"),
            1_000_000
        );
        assert_eq!(st.key_seq("s-c", "k-0"), None, "the oldest key is evicted");
        assert_eq!(st.key_seq("s-c", "k-1"), Some(1), "the next-oldest stands");
        assert_eq!(st.key_seq("s-c", "k-newest"), Some(1_000_000));
        assert_eq!(st.key_lines("s-c").len(), KEYS_KEEP, "the table is capped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deadlines_round_trip_are_bounded_and_a_damaged_line_reads_as_nothing() {
        let dir = scratch("deadlines");
        let st = StateDir::open(&dir).expect("open");
        assert!(st.deadlines().is_empty());
        let one = Deadline {
            off: 90_312,
            sid: "s-abcdef0123456789".to_string(),
            at: 1_700_000_240_123,
            dl: 240_000,
            to: Some("n-0123456789abcdef".to_string()),
        };
        st.set_deadlines(std::slice::from_ref(&one)).expect("write");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").deadlines(),
            vec![one.clone()]
        );
        st.set_deadlines(&[]).expect("clear");
        assert!(st.deadlines().is_empty());
        for damaged in [
            "",
            "off=1 sid=s-a at=2",          // no dl
            "off=1 sid=s-a dl=3",          // no at
            "sid=s-a at=2 dl=3",           // no offset
            "off=1 at=2 dl=3",             // no sid
            "off=1 sid= at=2 dl=3",        // an empty sid
            "off=x sid=s-a at=2 dl=3",     // an offset that is not one
            "off=1 sid=s-a at=2 dl=3 to=", // an empty recipient
        ] {
            assert_eq!(Deadline::parse(damaged), None, "{damaged:?}");
        }
        assert_eq!(Deadline::parse(&one.render()), Some(one.clone()));
        assert_eq!(
            Deadline::parse("off=90312 sid=s-abcdef0123456789 at=1700000240123 dl=240000"),
            Some(Deadline { to: None, ..one })
        );
        let many: Vec<Deadline> = (0..DEADLINES_KEEP as u64 + 3)
            .map(|n| Deadline {
                off: n,
                sid: "s-a".to_string(),
                at: n,
                dl: 1,
                to: None,
            })
            .collect();
        st.set_deadlines(&many).expect("write many");
        let kept = st.deadlines();
        assert_eq!(kept.len(), DEADLINES_KEEP);
        assert_eq!(kept[0].off, 3, "the three oldest are gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_asked_table_round_trips_and_is_bounded() {
        let dir = scratch("asked");
        let st = StateDir::open(&dir).expect("open");
        assert!(st.asked().is_empty());
        let two = vec![
            Asked {
                off: 7,
                to: "n-0123456789abcdef".to_string(),
            },
            Asked {
                off: 9,
                to: "h-andrew".to_string(),
            },
        ];
        st.set_asked(&two).expect("write");
        assert_eq!(StateDir::open(&dir).expect("reopen").asked(), two);
        for damaged in ["", "7", "x n-a", "7 n-a extra"] {
            assert_eq!(Asked::parse(damaged), None, "{damaged:?}");
        }
        let many: Vec<Asked> = (0..ASKED_KEEP as u64 + 2)
            .map(|off| Asked {
                off,
                to: "n-a".to_string(),
            })
            .collect();
        st.set_asked(&many).expect("write many");
        let kept = st.asked();
        assert_eq!(kept.len(), ASKED_KEEP);
        assert_eq!(kept[0].off, 2, "the two oldest are gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pin_table_is_bounded_and_evicts_the_oldest_first_sight() {
        let dir = scratch("pins-bound");
        let st = StateDir::open(&dir).expect("open");
        let full: Vec<String> = (0..PINS_KEEP)
            .map(|n| format!("s-{n:016x} n-{:016x}", n % 3))
            .collect();
        st.write("pins", &full.join("\n"))
            .expect("seed a full table");
        assert_eq!(st.pins().len(), PINS_KEEP);
        for n in PINS_KEEP..PINS_KEEP + 16 {
            st.pin(&format!("s-{n:016x}"), &format!("n-{:016x}", n % 3))
                .expect("pin");
        }
        let pins = st.pins();
        assert_eq!(pins.len(), PINS_KEEP, "the table is capped");
        for n in 0..16 {
            assert!(!pins.contains_key(&format!("s-{n:016x}")), "{n} evicted");
        }
        for n in 16..PINS_KEEP + 16 {
            assert_eq!(
                pins.get(&format!("s-{n:016x}")),
                Some(&format!("n-{:016x}", n % 3)),
                "{n} kept, with its node"
            );
        }
        let survivor = format!("s-{:016x}", PINS_KEEP);
        assert_eq!(
            st.pin(&survivor, "n-rogue").expect("re-pin"),
            format!("n-{:016x}", PINS_KEEP % 3)
        );
        assert_eq!(st.pins().len(), PINS_KEEP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_bridge_lock_has_one_holder_until_it_drops() {
        let dir = scratch("lock");
        let st = StateDir::open(&dir).expect("open");
        let held = st.lock_bridge().expect("the first take");
        std::fs::write(dir.join("pid"), "4242\n").expect("pid");
        let refused = st.lock_bridge().err().expect("a second take is refused");
        assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
        assert!(refused.to_string().contains("(pid 4242)"), "{refused}");
        drop(held);
        let _again = st.lock_bridge().expect("free once the holder let go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_log_switch_refuses_a_dangling_offset_directory() {
        for blocked in ["seen", "topics"] {
            let dir = scratch("dangling-offsets");
            let st = StateDir::open(&dir).unwrap();
            let area = dir.join(blocked);
            if area.is_dir() {
                std::fs::remove_dir(&area).unwrap();
            }
            std::os::unix::fs::symlink(dir.join("missing"), &area).unwrap();
            assert!(
                st.forget_log_offsets(20, None, None, None).is_err(),
                "unreadable {blocked} was empty"
            );
            assert!(area.is_symlink(), "the unreadable directory entry stays");
            std::fs::remove_file(&area).unwrap();
            assert!(
                st.forget_log_offsets(20, None, None, None).is_ok(),
                "an absent directory is empty"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn the_log_witness_round_trips_and_names_another_log_by_identity_or_head() {
        use astream_broker::LogIdentity;
        let a = LogIdentity {
            id: [0xaa; 16],
            generation: 0,
        };
        let a_rewritten = LogIdentity { generation: 1, ..a };
        let b = LogIdentity {
            id: [0xbb; 16],
            generation: 0,
        };
        let dir = scratch("witness");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.log_witness(), None, "nothing seen yet");
        st.set_log_witness(&LogWitness {
            high: 500,
            id: None,
        })
        .expect("write");
        let w = st.log_witness().expect("read back");
        assert_eq!(
            w,
            LogWitness {
                high: 500,
                id: None
            }
        );
        assert!(w.replaced_by(499, None) && w.replaced_by(499, Some(a)));
        assert!(!w.replaced_by(500, None) && !w.replaced_by(10_000, Some(a)));
        let with_id = LogWitness {
            high: 500,
            id: Some(a),
        };
        st.set_log_witness(&with_id).expect("write");
        assert_eq!(st.log_witness(), Some(with_id));
        assert!(with_id.replaced_by(10_000, Some(b)));
        assert!(with_id.replaced_by(10_000, Some(a_rewritten)));
        assert!(!with_id.replaced_by(10_000, Some(a)));
        assert!(
            !with_id.replaced_by(10_000, None),
            "no identity says nothing"
        );
        assert!(
            with_id.replaced_by(499, Some(a)),
            "a head below is evidence too"
        );
        assert_eq!(
            LogWitness::parse("id=00ff high=7"),
            Some(LogWitness { high: 7, id: None })
        );
        assert_eq!(LogWitness::parse("high=+7"), None);
        assert_eq!(LogWitness::parse("id=00ff"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn standing_halts_round_trip_per_fleet_and_a_damaged_line_is_nothing() {
        let dir = scratch("halts");
        let st = StateDir::open(&dir).expect("open");
        assert!(st.standing_halts("f").is_empty());
        let halts = BTreeMap::from([
            (
                "h-a".to_string(),
                StandingHalt {
                    reason: "main%20broken".to_string(),
                    kept: false,
                },
            ),
            (
                "h-b".to_string(),
                StandingHalt {
                    reason: "fleet-halt".to_string(),
                    kept: true,
                },
            ),
        ]);
        st.set_standing_halts("f", &halts).expect("write");
        assert_eq!(st.standing_halts("f"), halts);
        assert!(st.standing_halts("g").is_empty(), "another fleet's halts");
        std::fs::write(
            dir.join("halts"),
            "f h-a r witnessed\nf h-b r maybe\nf n-node r kept\nf h-c\n",
        )
        .expect("write");
        assert_eq!(
            st.standing_halts("f").into_keys().collect::<Vec<_>>(),
            ["h-a"]
        );
        st.set_standing_halts("f", &BTreeMap::new()).expect("empty");
        assert!(st.standing_halts("f").is_empty());

        st.set_halt_acked(9).expect("ack");
        st.clear_halt_acked().expect("clear");
        assert_eq!(st.halt_acked(), None);
        st.clear_halt_acked().expect("clearing nothing is no error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_reads_as_unknown() {
        let dir = scratch("corrupt");
        let st = StateDir::open(&dir).expect("open");
        std::fs::write(dir.join("seq"), "   \n").expect("write");
        assert_eq!(st.sequence(), 0);
        std::fs::write(dir.join("inc"), "not-a-number").expect("write");
        assert_eq!(st.incarnation(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn boot_id(b: u8) -> [u8; 16] {
        [b; 16]
    }

    fn in_boot(b: u8) -> IncSeam {
        IncSeam {
            boot: Some(Some(boot_id(b))),
            ..IncSeam::default()
        }
    }

    fn inc_file_id(n: u64) -> Option<IncFile> {
        Some(IncFile {
            dev: 1,
            dir: 2,
            ino: n,
        })
    }

    #[test]
    fn an_incarnation_lease_round_trips_and_anything_torn_is_no_lease() {
        for lease in [
            IncLease {
                used: 3,
                top: 66,
                boot: Some(boot_id(7)),
                file: Some(IncFile {
                    dev: u64::MAX,
                    dir: 0,
                    ino: 12_345,
                }),
            },
            IncLease {
                used: 5,
                top: 5,
                boot: None,
                file: None,
            },
        ] {
            assert_eq!(IncLease::parse(&lease.render()), Some(lease), "{lease:?}");
        }
        let id = aterm_codec::hex::encode_lower(&boot_id(7));
        for torn in [
            String::new(),
            "used=3 top=66".to_string(),
            format!("used=3 boot={id} file=1:2:3"),
            format!("used=3 top=66 boot={id}"),
            format!("used=3 top=66 boot={} file=1:2:3", &id[..31]),
            format!("used=3 top=66 boot={} file=1:2:3", "0".repeat(32)),
            format!("used=67 top=66 boot={id} file=1:2:3"),
            format!("used=3 used=4 top=66 boot={id} file=1:2:3"),
            format!("used=3 top=66 boot={id} file=1:2:3 extra=1"),
            format!("used=x top=66 boot={id} file=1:2:3"),
            format!("used=3 top=66 boot={id} file=1:2"),
            format!("used=3 top=66 boot={id} file=1:2:3:4"),
            format!("used=3 top=66 boot={id} file=1::3"),
            format!("used=3 top=66 boot={id} file=1:2:3 file=-"),
        ] {
            assert_eq!(IncLease::parse(&torn), None, "{torn:?}");
        }
    }

    #[test]
    fn the_incarnation_plan_trusts_only_this_boots_lease_and_counts_seq_and_the_bus() {
        let (b1, b2) = (Some(boot_id(1)), Some(boot_id(2)));
        let f = inc_file_id(1);
        let lease = |used, top, boot| {
            Some(IncLease {
                used,
                top,
                boot,
                file: f,
            })
        };
        let plan = |next, top, flush| Some(IncPlan { next, top, flush });
        let span = INC_LEASE;
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 3, b1, 0, span),
            plan(4, 66, false)
        );
        assert_eq!(
            plan_incarnation(66, f, lease(66, 66, b1), 66, b1, 0, span),
            plan(67, 66 + span, true)
        );
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 3, b2, 0, span),
            plan(67, 66 + span, true)
        );
        assert_eq!(
            plan_incarnation(130, f, lease(3, 66, b1), 3, b1, 0, span),
            plan(131, 130 + span, true)
        );
        for (file, named) in [(inc_file_id(2), f), (None, f), (f, None), (None, None)] {
            let other = Some(IncLease {
                used: 3,
                top: 66,
                boot: b1,
                file: named,
            });
            assert_eq!(
                plan_incarnation(66, file, other, 3, b1, 0, span),
                plan(67, 66 + span, true),
                "inc {file:?}, lease naming {named:?}"
            );
        }
        assert_eq!(
            plan_incarnation(9, f, None, 9, b1, 0, span),
            plan(10, 9 + span, true)
        );
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 66, b1, 0, span),
            plan(67, 66 + span, true)
        );
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 3, b1, 90, span),
            plan(91, 90 + span, true)
        );
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, None), 3, None, 0, span),
            plan(67, 67, true)
        );
        assert_eq!(
            plan_incarnation(INC_MAX - 1, f, None, 0, b1, 0, span),
            plan(INC_MAX, INC_MAX, true)
        );
        assert_eq!(plan_incarnation(INC_MAX, f, None, 0, b1, 0, span), None);
        assert_eq!(plan_incarnation(0, None, None, 0, b1, INC_MAX, span), None);
        assert_eq!(
            plan_incarnation(0, None, None, u64::MAX >> 32, b1, 0, span),
            None
        );
    }

    #[test]
    fn a_lease_of_attaches_costs_one_flush_and_a_new_boot_flushes_once() {
        let dir = scratch("inclease-cost");
        let st = StateDir::open(&dir).expect("open");
        st.set_inc_seam(in_boot(1));
        let take = |n: u64| -> Vec<u64> {
            (0..n)
                .map(|_| st.take_incarnation(0).expect("take"))
                .collect()
        };
        let before = st.ops();
        let mut taken = take(3);
        assert_eq!(
            (st.incarnation(), st.incarnation_top()),
            (3, INC_LEASE),
            "the doctor's answer is what was taken, not the top"
        );
        taken.extend(take(INC_LEASE - 3));
        let lease = st.ops() - before;
        assert_eq!(
            taken,
            (1..=INC_LEASE).collect::<Vec<_>>(),
            "one apart, from 1"
        );
        assert_eq!(
            (lease.fsyncs, lease.replaces),
            (2, INC_LEASE + 1),
            "one flushed `inc`, then one `inc-lease` replace per attach: {lease:?}"
        );
        let before = st.ops();
        assert_eq!(st.take_incarnation(0).expect("take"), INC_LEASE + 1);
        assert_eq!(
            (st.ops() - before).fsyncs,
            2,
            "the lease ran out: flushed again"
        );

        st.set_inc_seam(in_boot(2));
        let before = st.ops();
        assert_eq!(
            st.take_incarnation(0).expect("take"),
            2 * INC_LEASE + 1,
            "above the whole lease"
        );
        assert_eq!((st.ops() - before).fsyncs, 2);
        let before = st.ops();
        assert_eq!(st.take_incarnation(0).expect("take"), 2 * INC_LEASE + 2);
        assert_eq!((st.ops() - before).fsyncs, 0, "and then trusted again");

        st.set_inc_seam(IncSeam {
            boot: Some(None),
            ..IncSeam::default()
        });
        for _ in 0..3 {
            let before = st.ops();
            let inc = st.take_incarnation(0).expect("take");
            assert_eq!((st.ops() - before).fsyncs, 2);
            assert_eq!(st.lease().map(|l| (l.used, l.top)), Some((inc, inc)));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_doctor_never_answers_a_lost_leases_unused_top() {
        let dir = scratch("inclease-doctor");
        let st = StateDir::open(&dir).expect("open");
        st.set_inc_seam(in_boot(1));
        for want in 1..=3 {
            let inc = st.take_incarnation(0).expect("take");
            assert_eq!(inc, want);
            st.reserve_sequence_block(inc, 16).expect("a block");
        }
        assert_eq!(
            (st.incarnation(), st.incarnation_top()),
            (3, INC_LEASE),
            "the doctor answers the incarnation taken, not the lease's top"
        );

        std::fs::remove_file(dir.join("inc-lease")).expect("lose the lease");
        assert_eq!(
            st.incarnation(),
            3,
            "seq's incarnation, never the lost lease's unused top ({INC_LEASE})"
        );
        std::fs::write(dir.join("inc-lease"), "used=3 top=").expect("tear the lease");
        assert_eq!(st.incarnation(), 3, "a torn lease");
        std::fs::write(dir.join("seq"), (base(1) + 16).to_string()).expect("seq");
        assert_eq!(st.incarnation(), 1, "a rolled-back seq");
        std::fs::remove_file(dir.join("seq")).expect("lose seq");
        assert_eq!(st.incarnation(), 0, "nothing known");
        let seq = st
            .reserve_next_sequence(st.incarnation())
            .expect("a sequence");
        assert!(
            seq < will(1),
            "the doctor's sequence {seq:#x} is below every will this node registered"
        );

        std::fs::remove_file(dir.join("inc-lease")).expect("no lease");
        std::fs::write(dir.join("inc"), "9").expect("inc");
        std::fs::write(dir.join("seq"), (base(9) + 5).to_string()).expect("seq");
        assert_eq!(st.incarnation(), 9, "an older build's dir");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_copied_state_dir_flushes_its_own_inc_before_trusting_a_lease() {
        let (a, c) = (scratch("inclease-copy-a"), scratch("inclease-copy-c"));
        let original = StateDir::open(&a).expect("open the original");
        original.set_inc_seam(in_boot(1));
        for want in 1..=5 {
            assert_eq!(original.take_incarnation(0).expect("take"), want);
        }
        let copy = StateDir::open(&c).expect("open the copy");
        for file in ["inc", "inc-lease", "seq"] {
            if a.join(file).exists() {
                std::fs::copy(a.join(file), c.join(file)).expect("copy");
            }
        }
        copy.set_inc_seam(in_boot(1));
        let before = copy.ops();
        assert_eq!(
            copy.take_incarnation(0).expect("take"),
            INC_LEASE + 1,
            "above the copied lease's whole top"
        );
        assert_eq!(
            (copy.ops() - before).fsyncs,
            2,
            "the copy's own `inc` is flushed before its first incarnation is presented"
        );
        let before = copy.ops();
        assert_eq!(copy.take_incarnation(0).expect("take"), INC_LEASE + 2);
        assert_eq!(
            (copy.ops() - before).fsyncs,
            0,
            "and then its own lease is trusted"
        );
        let before = original.ops();
        assert_eq!(original.take_incarnation(0).expect("take"), 6);
        assert_eq!(
            (original.ops() - before).fsyncs,
            0,
            "the original keeps its lease"
        );
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&c);
    }

    #[cfg(unix)]
    #[test]
    fn a_state_dirs_mark_is_its_own_and_a_copy_has_another() {
        const NODE: &str = "n-0123456789abcdef";
        let boot = crate::durability::test_boot(1);
        let open = |path: &Path| {
            let state = StateDir::open(path).expect("open the state dir");
            state.set_inc_seam(IncSeam {
                boot: Some(Some(boot)),
                ..IncSeam::default()
            });
            state
        };
        let (a, c) = (scratch("mark-a"), scratch("mark-c"));
        let original = open(&a);
        std::fs::write(a.join("node"), format!("{NODE}\n")).expect("the node");
        let mark = original
            .mark(NODE)
            .expect("a mark where the platform names an inode");
        assert_eq!(mark.len(), 16, "{mark}");
        assert!(mark.bytes().all(|b| b.is_ascii_hexdigit()), "{mark}");
        assert_eq!(
            open(&a).mark(NODE).as_deref(),
            Some(mark.as_str()),
            "the same dir, opened again, is the same dir"
        );
        let copy = open(&c);
        std::fs::copy(a.join("node"), c.join("node")).expect("copy the node");
        let copied = copy.mark(NODE).expect("the copy's mark");
        assert_ne!(copied, mark, "a copy of the dir is another dir");
        assert_ne!(
            original.mark("n-fedcba9876543210").as_deref(),
            Some(mark.as_str()),
            "another node's"
        );
        original.set_inc_seam(IncSeam {
            boot: Some(Some(crate::durability::test_boot(2))),
            ..IncSeam::default()
        });
        assert_ne!(original.mark(NODE).as_deref(), Some(mark.as_str()));
        original.set_inc_seam(IncSeam {
            boot: Some(None),
            ..IncSeam::default()
        });
        assert_eq!(
            original.mark(NODE),
            None,
            "no boot identity grants no adoption"
        );
        original.set_inc_seam(IncSeam {
            boot: Some(Some(boot)),
            ..IncSeam::default()
        });
        assert_eq!(original.mark(NODE).as_deref(), Some(mark.as_str()));
        let moved = scratch("mark-moved");
        std::fs::rename(&a, &moved).expect("rename the original");
        assert_eq!(
            open(&moved).mark(NODE).as_deref(),
            Some(mark.as_str()),
            "a rename keeps the dir"
        );
        let of_bytes = |dir: &Path| {
            let node = std::fs::read(dir.join("node")).expect("the node");
            format!("{:016x}", aterm_hash::fnv1a_64(&node))
        };
        assert_eq!(
            of_bytes(&moved),
            of_bytes(&c),
            "the copy holds the same bytes"
        );
        let _ = std::fs::remove_dir_all(&moved);
        let _ = std::fs::remove_dir_all(&c);
    }

    #[test]
    fn the_shipped_trust_decision_leases_a_dir_on_the_boot_volume() {
        let dir = scratch("inclease-shipped");
        let st = StateDir::open(&dir).expect("open");
        let boot = crate::durability::this_boot();
        let on_boot = on_boot_volume(&dir);
        if cfg!(target_vendor = "apple") {
            assert!(boot.is_some(), "macOS names its boot session");
            assert!(on_boot, "{dir:?} is on the boot volume");
        }
        let before = st.ops();
        let taken: Vec<u64> = (0..5)
            .map(|_| st.take_incarnation(0).expect("take"))
            .collect();
        assert_eq!(taken, [1, 2, 3, 4, 5]);
        let fsyncs = (st.ops() - before).fsyncs;
        eprintln!(
            "MEASURED five shipped takes: {fsyncs} fsyncs (boot {boot:?}, on boot volume {on_boot})"
        );
        let want = if boot.is_some() && on_boot { 2 } else { 10 };
        assert_eq!(fsyncs, want, "five takes on the shipped trust decision");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn lay_down(st: &StateDir, s: &aterm_spec::interp::State) {
        let v = |name: &str| u64::try_from(s[name]).expect("a model value is not negative");
        let put = |file: &str, held: Option<String>| match held {
            Some(text) => std::fs::write(st.path(file), text).expect(file),
            None => match std::fs::remove_file(st.path(file)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => panic!("{file}: {e}"),
                _ => {}
            },
        };
        put("inc", (v("c_inc") > 0).then(|| v("c_inc").to_string()));
        put(
            "inc-lease",
            (v("c_lb") > 0).then(|| {
                IncLease {
                    used: v("c_used"),
                    top: v("c_top"),
                    boot: Some(boot_id(u8::try_from(v("c_lb")).expect("a small boot"))),
                    file: st.inc_file(),
                }
                .render()
            }),
        );
        put(
            "seq",
            (v("c_seq") > 0).then(|| (base(v("c_seq")) + 64).to_string()),
        );
    }

    fn read_back(st: &StateDir) -> [i64; 5] {
        let int = |n: u64| i64::try_from(n).expect("a small value");
        let (used, top, lb) = st.lease().map_or((0, 0, 0), |l| {
            let boot = l.boot.expect("the walk always names its boot");
            assert!(boot.iter().all(|b| *b == boot[0]), "a stand-in boot id");
            (l.used, l.top, u64::from(boot[0]))
        });
        [
            int(st.incarnation_top()),
            int(used),
            int(top),
            int(lb),
            int(st.sequence() >> 32),
        ]
    }

    fn diverging_takes(
        model: &aterm_spec::derive::Model,
        states: &[aterm_spec::interp::State],
        mutant: u8,
        first: bool,
    ) -> (usize, Vec<String>) {
        let span = model
            .consts
            .iter()
            .find(|(name, _)| *name == "Span")
            .map(|(_, v)| u64::try_from(*v).expect("a positive span"))
            .expect("the model declares Span");
        let walk = if first { "control" } else { "walk" };
        let dir = scratch(&format!("inclease-{walk}-{mutant}"));
        let st = StateDir::open(&dir).expect("open");
        let mut seen = std::collections::BTreeSet::new();
        let (mut compared, mut diverged) = (0, Vec::new());
        for s in states {
            let key = ["boot", "c_inc", "c_used", "c_top", "c_lb", "c_seq", "pc"].map(|k| s[k]);
            if s["pc"] != 1 || !seen.insert(key) {
                continue;
            }
            let completed = ["Take", "TakeFlushed"]
                .iter()
                .find_map(|a| model.successors(a, s).into_iter().next());
            for (kill, action) in [
                (None, None),
                (Some(IncStep::Renamed), Some("KillRenamed")),
                (Some(IncStep::Flushed), Some("KillFlushed")),
            ] {
                let killed = action.and_then(|a| model.successors(a, s).into_iter().next());
                let (expect, dies) = match (killed, &completed) {
                    (Some(k), _) => (k, true),
                    (None, Some(c)) => (c.clone(), false),
                    (None, None) => continue, // the model's bound, not a step
                };
                if kill.is_some() && !dies && s["tr"] == 0 {
                    // Distinct branch.
                    continue;
                }
                lay_down(&st, s);
                st.set_inc_seam(IncSeam {
                    boot: Some(Some(boot_id(u8::try_from(s["boot"]).expect("small")))),
                    kill_at: kill,
                    mutant,
                    span: Some(span),
                    off_volume: false,
                });
                let got = st.take_incarnation(0);
                let files = read_back(&st);
                let want = ["c_inc", "c_used", "c_top", "c_lb", "c_seq"].map(|k| expect[k]);
                let answer_ok = match (&got, dies) {
                    (Err(_), true) => true,
                    (Ok(inc), false) => i64::try_from(*inc).ok() == Some(s["pn"]),
                    _ => false,
                };
                compared += 1;
                if !answer_ok || files != want {
                    diverged.push(format!(
                        "from {s:?} killed at {kill:?}: took {got:?} (model pn {}), files \
                         {files:?}, model {want:?}",
                        s["pn"]
                    ));
                    if first {
                        let _ = std::fs::remove_dir_all(&dir);
                        return (compared, diverged);
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        (compared, diverged)
    }

    #[test]
    fn the_incarnation_lease_refines_its_model() {
        use aterm_spec::interp::{reachable_states, with_consts};
        let committed = aterm_models_session::incarnation_lease_model();
        let walk = |buggy: u8| {
            let model = match buggy {
                0 => committed.clone(),
                _ => with_consts(&committed, &[("Buggy", i64::from(buggy)), ("MaxBoot", 2)]),
            };
            (
                buggy,
                diverging_takes(&model, &reachable_states(&model), buggy, false),
            )
        };
        let roomier = with_consts(&committed, &[("MaxTop", 7), ("MaxBoot", 2)]);
        let roomier_states = reachable_states(&roomier);
        let (walks, controls) = std::thread::scope(|scope| {
            let committed_walk = scope.spawn(|| walk(0));
            let defective: Vec<_> = (1..=3u8).map(walk).collect();
            let controls: Vec<_> = (1..=3u8)
                .map(|mutant| {
                    (
                        mutant,
                        diverging_takes(&roomier, &roomier_states, mutant, true),
                    )
                })
                .collect();
            let committed_walk = committed_walk
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            (
                std::iter::once(committed_walk)
                    .chain(defective)
                    .collect::<Vec<_>>(),
                controls,
            )
        });
        for (buggy, (compared, diverged)) in walks {
            eprintln!("IncarnationLease Buggy = {buggy}: {compared} takes held to the model");
            assert!(
                compared > 100,
                "Buggy = {buggy}: only {compared} takes compared"
            );
            assert!(
                diverged.is_empty(),
                "Buggy = {buggy}: the take is not the model's in {} of {compared}:\n{}",
                diverged.len(),
                diverged.join("\n")
            );
        }
        for (mutant, (compared, diverged)) in controls {
            assert!(
                !diverged.is_empty(),
                "the take's defect {mutant} matched the committed model in all {compared} takes"
            );
        }
    }

    fn resume_through(
        st: &StateDir,
        s: &std::collections::BTreeMap<&'static str, i64>,
        top: i64,
        rebase_at_head: bool,
    ) -> i64 {
        let at = |name: &str| u64::try_from(s[name]).expect("a model offset");
        let (g, rg, h, c) = (at("g"), at("rg"), at("h"), at("c"));
        let mut steps = Vec::new();
        if g >= 2 && s["f2"] != top {
            steps.push((g - 1, at("f2")));
        }
        if g >= 1 && s["f1"] != top {
            steps.push((g, at("f1")));
        }
        let lineage = astream_broker::Lineage::from_steps(&steps).expect("a lineage astream reads");
        let log = |generation| astream_broker::LogIdentity {
            id: [0x5a; 16],
            generation,
        };
        let witness = LogWitness {
            high: c,
            id: Some(log(rg)),
        };
        let keep_below = if rebase_at_head {
            None
        } else {
            witness.continues(Some(log(g)), Some(&lineage))
        };
        let mut cursors = BTreeMap::new();
        cursors.insert("t".to_string(), TopicCursor { next: c });
        st.set_topics("s-r", &cursors).expect("write the cursor");
        st.forget_log_offsets(h, Some(log(g)), keep_below, Some(&lineage))
            .expect("forget or clamp");
        i64::try_from(st.topics("s-r")["t"].next).expect("an offset")
    }

    #[test]
    fn the_reader_resume_refines_its_model() {
        use aterm_spec::interp;
        let committed = aterm_models_session::fabric_reader_resume_model();
        let top = committed
            .consts
            .iter()
            .find(|(name, _)| *name == "Top")
            .map(|(_, v)| *v)
            .expect("the model's Top");
        let dir = scratch("resume-tier1");
        let st = StateDir::open(&dir).expect("open");
        let mut real: BTreeMap<[i64; 6], i64> = BTreeMap::new();
        let mut run = |s: &std::collections::BTreeMap<&'static str, i64>| {
            let key = ["g", "rg", "h", "c", "f1", "f2"].map(|v| s[v]);
            *real
                .entry(key)
                .or_insert_with(|| resume_through(&st, s, top, false))
        };
        let mut walk = |model: &aterm_spec::derive::Model, committed: bool| {
            let (mut compared, mut diverged, mut rebase_refused) = (0, Vec::new(), false);
            for state in interp::reachable_states(model) {
                let deciding = state["pc"] == 0 && state["down"] == 0 && state["rg"] < state["g"];
                if !deciding {
                    continue;
                }
                let cursor = run(&state);
                let decided = |r| {
                    let mut next = state.clone();
                    next.insert("r", r);
                    next.insert("pc", 1);
                    next
                };
                compared += 1;
                let taken = decided(cursor);
                if interp::admits(model, &state, &taken) == Some("Decide") {
                    let mut resumed = taken.clone();
                    assert!(model.fire("Resume", &mut resumed), "Resume from {taken:?}");
                    for law in ["NoSkipInWindow", "AnotherLogOutsideWindow"]
                        .into_iter()
                        .filter(|_| committed)
                    {
                        assert!(
                            model.check_invariant(law, &resumed),
                            "{}: the real resume to {cursor} breaks {law} from {state:?}",
                            model.name
                        );
                    }
                } else {
                    diverged.push(format!("{state:?}: real {cursor}"));
                    if !committed {
                        break;
                    }
                }
                if committed && !rebase_refused {
                    let rebase = resume_through(&st, &state, top, true);
                    rebase_refused =
                        interp::admits(model, &state, &decided(rebase)) != Some("Decide");
                }
            }
            (compared, diverged, rebase_refused)
        };

        let (compared, diverged, rebase_refused) = walk(&committed, true);
        eprintln!("FabricReaderResume: {compared} resumes held to the model");
        assert!(compared > 100, "only {compared} resumes compared");
        assert!(
            diverged.is_empty(),
            "the real reader is not the model's in {} of {compared}:\n{}",
            diverged.len(),
            diverged.join("\n")
        );
        assert!(
            rebase_refused,
            "NEGATIVE CONTROL: the reader that re-bases at the head matched the model in all \
             {compared} resumes"
        );
        for buggy in 1..=3 {
            let (compared, diverged, _) = walk(&interp::with_buggy(&committed, buggy), false);
            assert!(
                !diverged.is_empty(),
                "the real reader matched the defective model Buggy = {buggy} in all {compared} \
                 resumes"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_generation_clamp_preserves_uncertain_refill_evidence() {
        let dir = aterm_tempfile::TempDir::new().expect("owned state");
        let st = StateDir::open(dir.path()).expect("open");
        let sid = "s-kept";
        let topics = BTreeMap::from([("topic".to_string(), TopicCursor { next: 9 })]);
        st.set_topics(sid, &topics).unwrap();
        st.set_seen_off(sid, 8).unwrap();
        st.set_topic_floors(
            sid,
            &BTreeMap::from([("topic".to_string(), 1), ("gone".to_string(), 7)]),
        )
        .unwrap();
        st.forget_log_offsets(9, None, Some(6), None).unwrap();
        assert_eq!(
            st.topic_floors(sid).unwrap(),
            BTreeMap::from([("topic".into(), 1)])
        );
        assert_eq!(st.topics(sid)["topic"].next, 6);
        assert_eq!(st.seen_off(sid), Some(5));
        st.forget_log_offsets(9, None, None, None).unwrap();
        assert!(st.topic_floors(sid).unwrap().is_empty());

        let mut preserved = Vec::new();
        for keep in [Some(6), None] {
            for bytes in [b"not a floor".as_slice(), b"", b"topic 1\ntopic 2", b"\xff"] {
                st.set_topics(sid, &topics).unwrap();
                st.set_seen_off(sid, 8).unwrap();
                let floor = dir.path().join("unhandled").join(sid);
                std::fs::write(&floor, bytes).unwrap();
                let before_topics = std::fs::read(dir.path().join("topics").join(sid)).unwrap();
                let before_seen = std::fs::read(dir.path().join("seen").join(sid)).unwrap();
                let result = st.forget_log_offsets(9, None, keep, None);
                preserved.push(
                    result.is_err()
                        && std::fs::read(&floor).is_ok_and(|got| got == bytes)
                        && std::fs::read(dir.path().join("topics").join(sid))
                            .is_ok_and(|got| got == before_topics)
                        && std::fs::read(dir.path().join("seen").join(sid))
                            .is_ok_and(|got| got == before_seen),
                );
            }
        }
        assert!(
            preserved.into_iter().all(|kept| kept),
            "CLAMP_DROPPED_UNCERTAIN_REFILL_EVIDENCE"
        );
    }

    fn in_flight_entry(seq: u64) -> InFlight {
        InFlight {
            pin: PublishPin {
                seq,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
            },
            subject: "/f/sim/in/n-1/s-a/n-1/note".to_string(),
            body: b"v=1 t=5 text=hello".to_vec(),
            log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
        }
    }

    #[test]
    fn a_torn_file_after_an_unclean_stop_is_set_aside_and_within_a_boot_refused() {
        for unclean in [true, false] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("torn")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).expect("open");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("the last bridge's run, in boot 1");
            st.take_incarnation(0).expect("its attach");
            let whole = in_flight_entry(7);
            st.set_in_flight(&whole).expect("a whole entry");
            let pin = PublishPin {
                identity: Some(PublishIdentity {
                    subject: whole.subject.clone(),
                    digest: "ab".repeat(32),
                }),
                ..whole.pin.clone()
            };
            st.set_post_pin("s-a", 1, &pin)
                .expect("a whole reservation");
            let entry = in_flight_entry(8).render();
            let cut_entry = &entry.as_bytes()[..entry.find("/s-a/").expect("the face") + 3];
            std::fs::write(root.join("inflight/8"), cut_entry).expect("cut entry");
            let reservation = PublishPin {
                seq: 9,
                ..pin.clone()
            }
            .render();
            let cut_pin = &reservation.as_bytes()[..reservation.len() - 20];
            std::fs::write(root.join("sent/s-a.2"), cut_pin).expect("cut reservation");
            for (place, name) in [
                ("acks", "s-a.4"),
                ("keys", "s-a"),
                ("unhandled", "s-a"),
                ("", "logpin"),
            ] {
                std::fs::create_dir_all(root.join(place)).expect("its dir");
                std::fs::write(root.join(place).join(name), b"").expect("torn to nothing");
            }
            std::fs::write(root.join("keys/s-a"), b"k1 12 identity /f/x").expect("cut key");

            if unclean {
                st.set_inc_seam(in_boot(2));
                assert!(
                    st.torn_files_possible().expect("run record reads"),
                    "boot 2, after boot 1's bridge stopped uncleanly"
                );
                let moved = st.set_aside_torn().expect("the sweep");
                let from: Vec<&str> = moved.iter().map(|m| m.from.as_str()).collect();
                assert_eq!(
                    from,
                    [
                        "inflight/8",
                        "sent/s-a.2",
                        "acks/s-a.4",
                        "keys/s-a",
                        "unhandled/s-a",
                    ],
                    "every torn file, kind by kind"
                );
                for m in &moved {
                    assert_eq!(m.to, format!("{QUARANTINE}/{}", m.from.replace('/', ".")));
                    assert!(
                        !root.join(&m.from).exists(),
                        "{} is moved, not copied",
                        m.from
                    );
                }
                assert_eq!(
                    std::fs::read(root.join("quarantine/inflight.8")).expect("kept"),
                    cut_entry,
                    "its exact bytes, never deleted"
                );
                assert_eq!(
                    std::fs::read(root.join("quarantine/sent.s-a.2")).expect("kept"),
                    cut_pin
                );
                assert_eq!(
                    st.in_flight().expect("the journal reads"),
                    vec![(7, whole.clone())]
                );
                assert_eq!(st.post_pin("s-a", 2).expect("reads"), None);
                assert_eq!(st.post_pin("s-a", 1).expect("reads"), Some(pin.clone()));
                assert_eq!(st.receipt_pin("s-a", 4).expect("reads"), None);
                assert!(st.topic_floors("s-a").expect("reads").is_empty());
                assert_eq!(
                    st.logpin().expect("reads").as_deref(),
                    Some(""),
                    "the mirror's pin stays, for the mirror to refuse"
                );
                st.publication_offset_resets(None)
                    .expect("a log move's reset reads");
                assert!(st.set_aside_torn().expect("again").is_empty(), "idempotent");
                std::fs::write(root.join("inflight/8"), b"").expect("torn again");
                assert_eq!(
                    st.publish_quarantine_file(
                        "inflight/8",
                        "quarantine/inflight.8",
                        b"replacement",
                    )
                    .expect_err("the actual publication cannot replace an occupant")
                    .kind(),
                    io::ErrorKind::AlreadyExists
                );
                assert_eq!(
                    std::fs::read(root.join("inflight/8")).expect("source remains"),
                    b""
                );
                assert_eq!(
                    std::fs::read(root.join("quarantine/inflight.8")).expect("sentinel remains"),
                    cut_entry
                );
                let again = st.set_aside_torn().expect("the next boot's sweep");
                assert_eq!(again.len(), 1);
                assert_eq!(again[0].to, format!("{QUARANTINE}/inflight.8.1"));
                assert_eq!(
                    std::fs::read(root.join("quarantine/inflight.8")).expect("kept"),
                    cut_entry,
                    "the first one is not overwritten"
                );
            } else {
                assert!(
                    !st.torn_files_possible().expect("run record reads"),
                    "the same boot: nothing a kill does tears a file"
                );
                assert!(st.set_aside_torn().expect("no sweep").is_empty());
                assert!(!root.join(QUARANTINE).exists(), "nothing moved");
                assert_eq!(
                    st.in_flight().expect_err("refused").kind(),
                    io::ErrorKind::InvalidData
                );
                assert_eq!(
                    st.post_pin("s-a", 2).expect_err("refused").kind(),
                    io::ErrorKind::InvalidData
                );
                assert!(st.publication_offset_resets(None).is_err());
                assert!(st.topic_floors("s-a").is_err());
                assert!(st
                    .logpin()
                    .expect("read")
                    .is_some_and(|t| crate::mirror::Pin::parse(&t).is_err()));
            }
        }
    }

    #[test]
    fn a_planted_quarantine_link_is_refused_and_nothing_moves() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("planted")
            .tempdir_in("/tmp")
            .expect("scratch");
        let elsewhere = aterm_tempfile::Builder::new()
            .prefix("elsewhere")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.begin_run()
            .expect("a run in boot 1, never stopped cleanly");
        st.set_inc_seam(in_boot(2));
        std::fs::write(root.join("sent/s-a.1"), b"").expect("torn to nothing");
        std::os::unix::fs::symlink(elsewhere.path(), root.join(QUARANTINE)).expect("plant");
        let refused = st.set_aside_torn().expect_err("a link is no quarantine");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
        assert!(root.join("sent/s-a.1").exists(), "nothing moved");
        assert_eq!(
            std::fs::read_dir(elsewhere.path()).expect("list").count(),
            0,
            "nothing written through the link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_sweep_tried_again_after_its_unlinks_failed_moves_each_file_once() {
        use std::os::unix::fs::PermissionsExt as _;
        assert_ne!(
            aterm_uds::peer::our_uid(),
            0,
            "chmod refusal needs an unprivileged uid"
        );
        struct RestoreMode(PathBuf, std::fs::Permissions);
        impl Drop for RestoreMode {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, self.1.clone());
            }
        }
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Unsupported] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("sweep-again-parent")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).unwrap();
            st.set_inc_seam(in_boot(1));
            st.begin_run().unwrap();
            std::fs::create_dir_all(root.join("inflight")).unwrap();
            std::fs::write(root.join("inflight/7"), b"torn 7").unwrap();
            st.set_inc_seam(in_boot(2));
            st.refuse_directory_flush(Some((root, aterm_durable::Flush::Durable, kind)));
            let before = st.ops();
            for _ in 0..2 {
                assert_eq!(st.set_aside_torn().unwrap_err().kind(), kind);
                assert!(root.join(QUARANTINE).is_dir());
                assert_eq!(std::fs::read_dir(root.join(QUARANTINE)).unwrap().count(), 0);
                assert_eq!(std::fs::read(root.join("inflight/7")).unwrap(), b"torn 7");
                assert_eq!(st.ops().replaces, before.replaces, "no copy published");
            }
            assert!(
                st.ops().fsyncs > before.fsyncs,
                "the root fence was attempted"
            );
            st.refuse_directory_flush(None);
            let moved = st.set_aside_torn().unwrap();
            assert_eq!(moved.len(), 1);
            assert_eq!(moved[0].to, "quarantine/inflight.7");
            assert_eq!(std::fs::read(root.join(&moved[0].to)).unwrap(), b"torn 7");
            assert!(!root.join("inflight/7").exists());
        }
        let dir = aterm_tempfile::Builder::new()
            .prefix("sweep-again")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.begin_run()
            .expect("a run in boot 1, never stopped cleanly");
        std::fs::create_dir_all(root.join("inflight")).expect("its dir");
        for seq in [7, 8, 9] {
            std::fs::write(root.join(format!("inflight/{seq}")), format!("torn {seq}"))
                .expect("torn");
        }
        std::fs::create_dir_all(root.join(QUARANTINE)).expect("the quarantine");
        std::fs::write(root.join("quarantine/inflight.7"), b"torn 7")
            .expect("a person's same-byte occupant");
        st.set_inc_seam(in_boot(2));
        st.refuse_writes_of(Some("quarantine/inflight.7"));
        assert!(
            st.set_aside_torn().is_err(),
            "the first publish failed before its link"
        );
        st.refuse_writes_of(None);
        assert_eq!(std::fs::read(root.join("inflight/7")).unwrap(), b"torn 7");
        st.refuse_directory_flush(Some((
            root.join(QUARANTINE).as_path(),
            aterm_durable::Flush::Durable,
            io::ErrorKind::Other,
        )));
        assert!(
            st.set_aside_torn().is_err(),
            "exclusive link made, its fence failed"
        );
        st.refuse_directory_flush(None);
        assert_eq!(
            std::fs::read(root.join("quarantine/inflight.7.1")).unwrap(),
            b"torn 7"
        );
        assert!(
            root.join("inflight/7").exists(),
            "no source unlinked after the failed fence"
        );
        std::fs::write(root.join("quarantine/inflight.9"), b"a person's own file")
            .expect("a file of other bytes at the name");
        st.set_inc_seam(in_boot(2));
        let mode = |m: u32| {
            std::fs::set_permissions(root.join("inflight"), std::fs::Permissions::from_mode(m))
                .expect("chmod");
        };
        let _restore = RestoreMode(
            root.join("inflight"),
            std::fs::metadata(root.join("inflight"))
                .unwrap()
                .permissions(),
        );
        mode(0o555);
        for attempt in 0..3 {
            let refused = st.set_aside_torn().expect_err("the unlinks fail");
            assert_eq!(
                refused.kind(),
                io::ErrorKind::PermissionDenied,
                "try {attempt}"
            );
        }
        mode(0o755);
        let moved = st.set_aside_torn().expect("the sweep, its dir writable");
        let to: Vec<&str> = moved.iter().map(|m| m.to.as_str()).collect();
        assert_eq!(
            to,
            [
                "quarantine/inflight.7.1",
                "quarantine/inflight.8",
                "quarantine/inflight.9.1"
            ]
        );
        let mut held: Vec<String> = std::fs::read_dir(root.join(QUARANTINE))
            .expect("list")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        held.sort();
        assert_eq!(
            held,
            [
                "inflight.7",
                "inflight.7.1",
                "inflight.8",
                "inflight.9",
                "inflight.9.1"
            ],
            "one copy of each torn file, and the person's file kept"
        );
        assert_eq!(
            std::fs::read(root.join("quarantine/inflight.9")).expect("kept"),
            b"a person's own file"
        );
        assert_eq!(
            std::fs::read(root.join("quarantine/inflight.7")).unwrap(),
            b"torn 7",
            "same-byte occupant kept, not adopted"
        );
        assert_eq!(st.quarantined_torn().expect("count"), Some(5));
        for seq in [7, 8, 9] {
            assert!(
                !root.join(format!("inflight/{seq}")).exists(),
                "{seq} moved"
            );
        }
        st.begin_run()
            .expect("a run in boot 2, never stopped cleanly");
        std::fs::write(root.join("inflight/7"), "torn 7").expect("torn again");
        st.set_inc_seam(in_boot(3));
        let again = st.set_aside_torn().expect("boot 3's sweep");
        let to: Vec<&str> = again.iter().map(|m| m.to.as_str()).collect();
        assert_eq!(
            to,
            ["quarantine/inflight.7.2"],
            "another file, another copy"
        );
        assert_eq!(st.quarantined_torn().expect("count"), Some(6));
        for replaced in [false, true] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("sweep-again-identity")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).unwrap();
            st.set_inc_seam(in_boot(1));
            st.begin_run().unwrap();
            std::fs::create_dir_all(root.join("inflight")).unwrap();
            std::fs::write(root.join("inflight/7"), b"torn 7").unwrap();
            st.set_inc_seam(in_boot(2));
            let quarantine = root.join(QUARANTINE);
            st.refuse_directory_flush(Some((
                quarantine.as_path(),
                aterm_durable::Flush::Durable,
                io::ErrorKind::Other,
            )));
            assert!(st.set_aside_torn().is_err());
            st.refuse_directory_flush(None);
            let copy = quarantine.join("inflight.7");
            std::fs::remove_file(&copy).unwrap();
            if replaced {
                std::fs::write(&copy, b"torn 7").unwrap();
            }
            let moved = st.set_aside_torn().unwrap();
            assert_eq!(moved.len(), 1);
            assert_eq!(
                moved[0].to,
                if replaced {
                    "quarantine/inflight.7.1"
                } else {
                    "quarantine/inflight.7"
                }
            );
            assert_eq!(std::fs::read(&copy).unwrap(), b"torn 7");
            assert!(!root.join("inflight/7").exists());
        }
    }

    #[test]
    fn a_planted_source_parent_is_refused_before_sweep_or_clean_close() {
        for kind in TornKind::ALL {
            for linked in [true, false] {
                let dir = aterm_tempfile::Builder::new()
                    .prefix("source-parent")
                    .tempdir_in("/tmp")
                    .expect("scratch");
                let elsewhere = aterm_tempfile::Builder::new()
                    .prefix("source-target")
                    .tempdir_in("/tmp")
                    .expect("scratch");
                let st = StateDir::open(dir.path()).expect("open");
                st.set_inc_seam(in_boot(1));
                st.begin_run().expect("open run");
                let name = if kind == TornKind::InFlight {
                    "8"
                } else {
                    "s-a.1"
                };
                let rel = format!("{}/{name}", kind.place());
                st.write(&rel, "12").expect("a pending write");
                std::fs::remove_file(st.path(&rel)).expect("remove owned fixture");
                std::fs::remove_dir(st.path(kind.place())).expect("replace owned parent");
                let sentinel = elsewhere.path().join(name);
                std::fs::write(&sentinel, b"external evidence").expect("outside sentinel");
                if linked {
                    std::os::unix::fs::symlink(elsewhere.path(), st.path(kind.place()))
                        .expect("plant parent link");
                } else {
                    std::fs::write(st.path(kind.place()), b"parent sentinel")
                        .expect("plant non-directory");
                }
                let run = std::fs::read(st.path("run")).expect("original run");
                st.set_inc_seam(in_boot(2));
                assert_eq!(
                    st.set_aside_torn()
                        .expect_err("refuse source parent")
                        .kind(),
                    io::ErrorKind::InvalidData
                );
                assert_eq!(
                    st.end_run_clean().expect_err("refuse clean close").kind(),
                    io::ErrorKind::InvalidData
                );
                assert_eq!(
                    std::fs::read(&sentinel).expect("outside remains"),
                    b"external evidence"
                );
                assert_eq!(std::fs::read(st.path("run")).expect("run remains"), run);
                assert!(st.unflushed.lock().expect("pending").contains(&rel));
                assert!(
                    !st.path(QUARANTINE).exists(),
                    "no evidence publication before admission"
                );
                if linked {
                    assert!(std::fs::symlink_metadata(st.path(kind.place()))
                        .expect("link remains")
                        .is_symlink());
                } else {
                    assert_eq!(
                        std::fs::read(st.path(kind.place())).expect("parent remains"),
                        b"parent sentinel"
                    );
                }
            }
        }
    }

    #[test]
    fn an_existing_run_marker_never_authorizes_legacy_fallback() {
        use std::os::unix::fs::PermissionsExt as _;
        for occupant in ["invalid", "directory", "dangling", "unreadable"] {
            // Distinct branch.
            if occupant == "unreadable" && aterm_uds::peer::our_uid() == 0 {
                // Distinct branch.
                continue;
            }
            let dir = aterm_tempfile::Builder::new()
                .prefix("run-marker")
                .tempdir_in("/tmp")
                .expect("scratch");
            let st = StateDir::open(dir.path()).expect("open");
            st.set_inc_seam(in_boot(1));
            st.take_incarnation(0).expect("legacy lease");
            st.set_inc_seam(in_boot(2));
            assert!(st
                .torn_files_possible()
                .expect("genuine absence uses legacy lease"));
            let run = st.path("run");
            let clean = LastRun {
                boot: Some(boot_id(1)),
                clean: true,
            }
            .render();
            match occupant {
                "invalid" => std::fs::write(&run, b"not a run\n").expect("invalid marker"),
                "directory" => std::fs::create_dir(&run).expect("nonregular marker"),
                "dangling" => std::os::unix::fs::symlink(dir.path().join("missing"), &run)
                    .expect("dangling marker"),
                _ => {
                    std::fs::write(&run, &clean).expect("valid clean marker");
                    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o000))
                        .expect("make unreadable");
                }
            }
            std::fs::write(st.path("sent/s-a.1"), b"torn evidence").expect("candidate");
            assert!(st.last_run().is_err(), "the occupant is not absence");
            assert!(st.torn_files_possible().is_err(), "no legacy verdict");
            assert!(st.set_aside_torn().is_err(), "no sweep");
            assert!(st.begin_run().is_err(), "no replacement on start");
            assert!(st.end_run_clean().is_err(), "no replacement on stop");
            assert_eq!(
                std::fs::read(st.path("sent/s-a.1")).expect("source remains"),
                b"torn evidence"
            );
            assert!(!st.path(QUARANTINE).exists());
            match occupant {
                "invalid" => assert_eq!(
                    std::fs::read(&run).expect("invalid evidence remains"),
                    b"not a run\n"
                ),
                "directory" => assert!(std::fs::symlink_metadata(&run)
                    .expect("directory remains")
                    .is_dir()),
                "dangling" => assert_eq!(
                    std::fs::read_link(&run).expect("link remains"),
                    dir.path().join("missing")
                ),
                _ => {
                    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o600))
                        .expect("restore owned marker");
                    assert_eq!(
                        std::fs::read_to_string(&run).expect("clean evidence remains"),
                        clean
                    );
                }
            }
        }
    }

    #[test]
    fn only_an_unclean_stop_in_an_earlier_boot_can_have_torn_a_file() {
        let unnamed = IncSeam {
            boot: Some(None),
            ..IncSeam::default()
        };
        for off_volume in [false, true] {
            let seam = |b| IncSeam {
                off_volume,
                ..in_boot(b)
            };
            let dir = aterm_tempfile::Builder::new()
                .prefix("unclean")
                .tempdir_in("/tmp")
                .expect("scratch");
            let st = StateDir::open(dir.path()).expect("open");
            st.set_inc_seam(seam(1));
            assert!(
                !st.torn_files_possible().expect("missing run"),
                "a new dir: nothing ran"
            );
            st.begin_run().expect("a run in boot 1");
            assert_eq!(
                st.last_run().expect("run record reads"),
                Some(LastRun {
                    boot: Some(boot_id(1)),
                    clean: false
                })
            );
            st.take_incarnation(0).expect("its attach");
            assert!(
                !st.torn_files_possible().expect("run reads"),
                "the same boot, killed"
            );
            st.set_inc_seam(seam(2));
            assert!(
                st.torn_files_possible().expect("run reads"),
                "boot 2, the run never closed"
            );
            st.set_inc_seam(unnamed);
            assert!(
                !st.torn_files_possible().expect("run reads"),
                "a boot nobody can name"
            );
            st.set_inc_seam(seam(1));
            st.end_run_clean().expect("a clean stop in boot 1");
            assert_eq!(
                st.last_run().expect("run record reads"),
                Some(LastRun {
                    boot: Some(boot_id(1)),
                    clean: true
                })
            );
            st.set_inc_seam(seam(2));
            assert!(
                !st.torn_files_possible().expect("run reads"),
                "boot 2, after a clean stop"
            );
            st.begin_run().expect("boot 2's run");
            st.set_inc_seam(seam(3));
            assert!(
                st.torn_files_possible().expect("run reads"),
                "boot 3, boot 2's run never closed"
            );
            std::fs::remove_file(dir.path().join("run")).expect("no record");
            st.set_inc_seam(seam(1));
            st.take_incarnation(0)
                .expect("an older build's attach, boot 1");
            st.set_inc_seam(seam(2));
            assert_eq!(
                st.torn_files_possible().expect("legacy missing run"),
                !off_volume,
                "the lease's boot, where it names one"
            );
        }
    }

    #[test]
    fn the_quarantine_counts_the_torn_files_the_owner_has_not_taken_away() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("held")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        assert_eq!(st.quarantined_torn().expect("no quarantine reads"), None);
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("boot 1's run");
        st.set_in_flight(&in_flight_entry(7))
            .expect("a whole entry");
        let entry = in_flight_entry(8).render();
        std::fs::write(
            root.join("inflight/8"),
            &entry.as_bytes()[..entry.find("/s-a/").expect("the face") + 3],
        )
        .expect("a cut entry");
        st.set_inc_seam(in_boot(2));
        assert_eq!(st.set_aside_torn().expect("the sweep").len(), 1);
        assert_eq!(
            st.retire_in_flight_of_an_earlier_boot()
                .expect("the retirement")
                .len(),
            1
        );
        let quarantine = root.join(QUARANTINE);
        std::fs::write(quarantine.join(".inflight.9.tmp.1"), b"staged").expect("a temp");
        std::os::unix::fs::symlink("inflight.8", quarantine.join("a-link")).expect("a link");
        assert_eq!(
            st.quarantined_torn().expect("the count"),
            Some(1),
            "the torn entry alone"
        );
        std::fs::remove_file(quarantine.join("inflight.8")).expect("the owner removes it");
        assert_eq!(
            st.quarantined_torn().expect("the count"),
            Some(0),
            "none torn left, the retired entry kept"
        );
        assert!(quarantine
            .join(format!("{RETIRED_PREFIX}inflight.7"))
            .exists());
        std::fs::remove_dir_all(&quarantine).expect("the owner empties it");
        std::fs::write(&quarantine, b"planted").expect("a file at the name");
        assert_eq!(
            st.quarantined_torn().expect_err("not a directory").kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_start_is_ordered_by_the_barrier_save_over_a_clean_stop_of_this_boot() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("start-tier")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let start = |boot: IncSeam| {
            let st = StateDir::open(root).expect("open");
            st.set_inc_seam(boot);
            let before = st.ops();
            let flushed = st.begin_run().expect("a start");
            let ops = st.ops() - before;
            assert_eq!(
                (ops.barriers, ops.fsyncs),
                (flushed.ordered, flushed.full),
                "said as made"
            );
            (st, (flushed.ordered, flushed.full))
        };
        let (_, cost) = start(in_boot(1));
        assert_eq!(cost, (0, 2), "over no record");
        let (st, cost) = start(in_boot(1));
        assert_eq!(cost, (0, 2), "over an open of this boot");
        st.end_run_clean().expect("a clean stop in boot 1");
        let (_, cost) = start(in_boot(1));
        assert_eq!(cost, (0, 2), "over a clean stop of this boot");
        let (st, cost) = start(in_boot(2));
        assert_eq!(cost, (2, 0), "over an earlier boot's open");
        st.end_run_clean().expect("a clean stop in boot 2");
        let (_, cost) = start(in_boot(3));
        assert_eq!(cost, (2, 0), "over an earlier boot's clean stop");
        let (_, cost) = start(IncSeam {
            boot: Some(None),
            ..IncSeam::default()
        });
        assert_eq!(cost, (0, 2), "in a boot nobody can name");
        let (st, cost) = start(in_boot(4));
        assert_eq!(cost, (0, 2), "over a record whose boot nobody named");
        st.end_run_clean().expect("a clean stop in boot 4");
        std::fs::remove_dir_all(root.join("sent")).expect("the reservations' directory");
        let elsewhere = aterm_tempfile::Builder::new()
            .prefix("start-tier-link")
            .tempdir_in("/tmp")
            .expect("scratch");
        std::os::unix::fs::symlink(elsewhere.path(), root.join("sent"))
            .expect("a link where a directory goes");
        let (_, cost) = start(in_boot(5));
        assert_eq!(cost, (0, 2), "a torn kind's directory that does not read");
    }

    #[test]
    fn after_a_clean_stop_damage_is_refused_across_a_reboot() {
        use aterm_durable::Flush;
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Unsupported] {
            for (reopen, next_boot) in [(false, 1), (true, 1), (false, 2), (true, 2)] {
                let dir = aterm_tempfile::Builder::new()
                    .prefix("clean-open-error")
                    .tempdir_in("/tmp")
                    .expect("scratch");
                let st = StateDir::open(dir.path()).expect("open");
                st.set_inc_seam(in_boot(1));
                st.begin_run().expect("initial OPEN");
                st.take_incarnation(0).expect("a lease in this boot");
                st.end_run_clean().expect("prior clean stop");
                st.set_inc_seam(in_boot(next_boot));
                let tier = if next_boot == 1 {
                    Flush::Durable
                } else {
                    Flush::Ordered
                };
                st.refuse_directory_flush(Some((dir.path(), tier, kind)));
                assert_eq!(
                    st.begin_run().expect_err("OPEN publication refused").kind(),
                    kind
                );
                assert!(
                    !st.last_run().unwrap().unwrap().clean,
                    "rename already visible"
                );
                let st = if reopen {
                    drop(st);
                    let st = StateDir::open(dir.path()).expect("reopen after failed publication");
                    st.set_inc_seam(in_boot(next_boot));
                    st
                } else {
                    st
                };
                st.refuse_directory_flush(Some((dir.path(), Flush::Durable, kind)));
                assert_eq!(
                    st.begin_run()
                        .expect_err("visible OPEN is not admission")
                        .kind(),
                    kind
                );
                st.refuse_directory_flush(None);
                let retried = st.begin_run().expect("successful OPEN retry");
                assert_eq!((retried.ordered, retried.full), (0, 2));
                let before = st.ops();
                st.take_incarnation(0)
                    .expect("the existing same-boot lease");
                assert_eq!(
                    (st.ops() - before).fsyncs,
                    if next_boot == 1 { 0 } else { 2 },
                    "only a new boot's incarnation supplies a later full flush"
                );
                let before = st.ops();
                assert_eq!(
                    st.begin_run().expect("same open reconnect"),
                    Flushed::default()
                );
                assert_eq!(st.ops() - before, FileOps::default());
                let replacement = st.path("run-replacement");
                std::fs::write(&replacement, std::fs::read(st.path("run")).unwrap()).unwrap();
                std::fs::rename(replacement, st.path("run")).unwrap();
                assert_eq!(
                    st.begin_run().expect("replacement needs a new fence").full,
                    2
                );
            }
        }
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Unsupported] {
            for final_root in [false, true] {
                let dir = aterm_tempfile::Builder::new()
                    .prefix("clean-dir-error")
                    .tempdir_in("/tmp")
                    .expect("scratch");
                let st = StateDir::open(dir.path()).expect("open");
                st.set_inc_seam(in_boot(1));
                st.begin_run().expect("an open run");
                let entry = in_flight_entry(7);
                st.set_in_flight(&entry).expect("an entry");
                st.set_post_pin("s-a", 1, &entry.pin)
                    .expect("a reservation");
                st.set_receipt_pin("s-a", 4, &entry.pin)
                    .expect("a receipt pin");
                let pending = st.unflushed.lock().unwrap().clone();
                assert_eq!(pending.len(), 3);
                let open = std::fs::read(st.path("run")).expect("the open marker");
                let (path, tier) = if final_root {
                    (st.root.clone(), Flush::Durable)
                } else {
                    (st.path("sent"), Flush::Ordered)
                };
                *st.dir_flush_refusal.lock().unwrap() = Some((path, tier, kind));
                assert_eq!(
                    st.end_run_clean()
                        .expect_err("a required directory barrier")
                        .kind(),
                    kind
                );
                assert_eq!(
                    *st.unflushed.lock().unwrap(),
                    pending,
                    "a failed stop retains every pending file for retry"
                );
                if !final_root {
                    assert_eq!(std::fs::read(st.path("run")).unwrap(), open);
                    assert!(!st.last_run().unwrap().unwrap().clean);
                }
                *st.dir_flush_refusal.lock().unwrap() = None;
                let flushed = st.end_run_clean().expect("a complete retry");
                assert_eq!(flushed.files, pending.len() as u64);
                assert_eq!(flushed.dirs, 3);
                assert_eq!(flushed.full, 1);
                assert!(st.unflushed.lock().unwrap().is_empty());
                assert!(st.last_run().unwrap().unwrap().clean);
            }
        }
        for clean in [true, false] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("clean")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let killed = StateDir::open(root).expect("open");
            killed.set_inc_seam(in_boot(1));
            killed.begin_run().expect("the first run");
            let entry = in_flight_entry(7);
            killed.set_in_flight(&entry).expect("an entry");
            killed
                .set_post_pin("s-a", 1, &entry.pin)
                .expect("a reservation");
            drop(killed);
            let st = StateDir::open(root).expect("its successor, in the same boot");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("the second run");
            st.set_receipt_pin("s-a", 4, &entry.pin)
                .expect("a receipt pin");
            if clean {
                st.refuse_writes_of(Some("sent/s-a.1"));
                assert!(
                    st.end_run_clean().is_err(),
                    "a controlled failed flush attempt"
                );
                assert_eq!(
                    st.last_run().expect("run reads").map(|run| run.clean),
                    Some(false)
                );
                st.refuse_writes_of(None);
                let before = st.ops();
                let flushed = st.end_run_clean().expect("a clean stop");
                let ops = st.ops() - before;
                assert_eq!(
                    (ops.replaces, ops.renames),
                    (1, 1),
                    "the three files of the two runs flushed where they are: only `run` written"
                );
                assert_eq!((ops.barriers, ops.fsyncs), (3 + 3 + 1, 1));
                assert_eq!(
                    flushed,
                    Flushed {
                        files: 3,
                        dirs: 3,
                        ordered: 7,
                        full: 1,
                    }
                );
                assert_eq!(
                    st.last_run().expect("run reads").map(|run| run.clean),
                    Some(true)
                );
            }
            std::fs::write(root.join("sent/s-a.1"), b"7 key identity\n").expect("a bug's damage");
            st.set_inc_seam(in_boot(2));
            let moved = st.set_aside_torn().expect("the sweep");
            if clean {
                assert!(moved.is_empty(), "nothing a clean stop left can be torn");
                assert!(st.post_pin("s-a", 1).is_err(), "refused, fail-closed");
            } else {
                assert_eq!(
                    moved.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
                    ["sent/s-a.1"],
                    "CONTROL: a run that never closed is judged unclean"
                );
            }
        }
    }

    #[test]
    fn a_flush_in_place_passes_over_what_is_gone_and_refuses_a_link() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("in-place")
            .tempdir_in("/tmp")
            .expect("scratch");
        let elsewhere = aterm_tempfile::Builder::new()
            .prefix("in-place-target")
            .tempdir_in("/tmp")
            .expect("scratch");
        let st = StateDir::open(dir.path()).expect("open");
        st.set_inc_seam(in_boot(1));
        st.write("keys/s-a", "k\n").expect("a key file");
        let flush = aterm_durable::Flush::Ordered;
        let before = st.ops();
        assert!(st
            .flush_in_place("keys/s-a", flush)
            .expect("a regular file"));
        assert_eq!((st.ops() - before).barriers, 1, "flushed where it is");
        let before = st.ops();
        assert!(!st.flush_in_place("keys/s-b", flush).expect("gone"));
        std::fs::create_dir(st.path("keys/s-c")).expect("a planted directory");
        assert!(!st.flush_in_place("keys/s-c", flush).expect("a directory"));
        let target = elsewhere.path().join("outside");
        std::fs::write(&target, b"outside").expect("what a link would name");
        std::os::unix::fs::symlink(&target, st.path("keys/s-d")).expect("a planted link");
        assert!(
            st.flush_in_place("keys/s-d", flush).is_err(),
            "a link is the open's error, never passed over as if gone"
        );
        assert_eq!(
            (st.ops() - before).barriers,
            0,
            "nothing passed over or refused is flushed"
        );
    }

    #[test]
    fn a_whole_in_flight_entry_of_an_earlier_boot_is_retired_and_within_a_boot_kept() {
        for (case, boot, clean) in [
            ("an unclean stop, then a reboot", 2, false),
            ("a kill in the same boot", 1, false),
            ("a clean stop, then a reboot", 2, true),
        ] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("retire")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).expect("open");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("boot 1's run");
            let (seven, nine) = (in_flight_entry(7), in_flight_entry(9));
            st.set_in_flight(&seven).expect("a whole entry");
            st.set_in_flight(&nine).expect("another");
            let on_disk = std::fs::read(root.join("inflight/7")).expect("its bytes");
            if clean {
                st.end_run_clean().expect("a clean stop");
            }
            let entry = in_flight_entry(8).render();
            let cut = &entry.as_bytes()[..entry.find("/s-a/").expect("the face") + 3];
            std::fs::write(root.join("inflight/8"), cut).expect("a cut entry");
            st.set_inc_seam(in_boot(boot));
            let retired = st
                .retire_in_flight_of_an_earlier_boot()
                .expect("the retirement");
            if boot == 1 || clean {
                assert!(retired.is_empty(), "{case}: nothing is retired");
                assert!(!root.join(QUARANTINE).exists(), "{case}: nothing moved");
                for seq in [7, 9] {
                    assert!(
                        root.join(format!("inflight/{seq}")).exists(),
                        "{case}: inflight/{seq} stays, for the attach to publish again"
                    );
                }
                continue;
            }
            assert_eq!(
                retired,
                [7, 9]
                    .map(|seq| SetAside {
                        from: format!("inflight/{seq}"),
                        to: format!("{QUARANTINE}/{RETIRED_PREFIX}inflight.{seq}"),
                    })
                    .to_vec(),
                "every whole entry, and only those"
            );
            assert_eq!(
                std::fs::read(root.join("quarantine/retired.inflight.7")).expect("kept"),
                on_disk,
                "its exact bytes, never deleted"
            );
            assert_eq!(
                st.in_flight()
                    .expect_err("the cut one, the torn sweep's")
                    .kind(),
                io::ErrorKind::InvalidData
            );
            let torn = st.set_aside_torn().expect("the torn sweep");
            assert_eq!(
                torn.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
                ["inflight/8"]
            );
            assert!(
                st.in_flight().expect("the journal reads").is_empty(),
                "nothing left for the attach to publish again"
            );
            assert!(
                st.retire_in_flight_of_an_earlier_boot()
                    .expect("again")
                    .is_empty(),
                "idempotent"
            );
            st.set_in_flight(&seven)
                .expect("an entry at a retired name");
            let again = st.retire_in_flight_of_an_earlier_boot().expect("again");
            assert_eq!(
                again.iter().map(|m| m.to.as_str()).collect::<Vec<_>>(),
                [format!("{QUARANTINE}/{RETIRED_PREFIX}inflight.7.1")]
            );
            assert_eq!(
                std::fs::read(root.join("quarantine/retired.inflight.7")).expect("kept"),
                on_disk,
                "the first one is not overwritten"
            );
        }
    }

    #[test]
    fn an_attach_of_this_boot_on_record_stops_the_retirement() {
        for (case, off_volume, attached_in_boot_2) in [
            ("a lease of boot 2", false, true),
            ("a lease of boot 1", false, false),
            ("off the boot volume", true, true),
        ] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("retire-lease")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).expect("open");
            let seam = |b| IncSeam {
                off_volume,
                ..in_boot(b)
            };
            st.set_inc_seam(seam(1));
            st.begin_run().expect("boot 1's run");
            st.take_incarnation(0).expect("boot 1's attach");
            st.set_in_flight(&in_flight_entry(7))
                .expect("a whole entry");
            st.set_inc_seam(seam(2));
            st.refuse_writes_of(Some("run"));
            assert_eq!(
                st.begin_run().expect_err("the disk refuses `run`").kind(),
                io::ErrorKind::StorageFull,
                "{case}"
            );
            assert_eq!(
                st.last_run().expect("run reads"),
                Some(LastRun {
                    boot: Some(boot_id(1)),
                    clean: false
                }),
                "{case}: `run` still names boot 1"
            );
            if attached_in_boot_2 {
                st.take_incarnation(0).expect("boot 2's attach");
            }
            let retired = st
                .retire_in_flight_of_an_earlier_boot()
                .expect("the retirement");
            if attached_in_boot_2 && !off_volume {
                assert!(retired.is_empty(), "{case}: nothing is retired");
                assert!(root.join("inflight/7").exists(), "{case}: the entry stays");
            } else {
                assert_eq!(
                    retired.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
                    ["inflight/7"],
                    "{case}: NEGATIVE CONTROL"
                );
            }
        }
    }

    #[test]
    fn a_cut_in_flight_entry_that_still_reads_is_set_aside_after_an_unclean_reboot() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("cut")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.take_incarnation(0).expect("the last attach, in boot 1");
        let reply = |seq| InFlight {
            body: b"v=1 t=5 re=42 text=a reply".to_vec(),
            log: astream_broker::LogIdentity::parse(&format!("{}.12", "5a".repeat(16))),
            ..in_flight_entry(seq)
        };
        let whole = reply(7);
        st.set_in_flight(&whole).expect("a whole entry");
        for seq in [8, 9, 10] {
            st.set_in_flight(&reply(seq))
                .expect("an entry the cut will take");
        }
        let at = |seq: u64, text: &str| match seq {
            8 => text.find("log=").expect("its witness"),
            9 => text.match_indices('\n').nth(1).expect("its face line").0 + 1 + 12,
            _ => text.len() - 2,
        };
        for seq in [8, 9, 10] {
            let path = root.join(format!("inflight/{seq}"));
            let text = std::fs::read_to_string(&path).expect("as written");
            let cut = &text[..at(seq, &text)];
            std::fs::write(&path, cut).expect("the cut");
            let read = InFlight::parse(seq, cut).expect("PRECONDITION: the cut still reads");
            assert_eq!(read.log.is_some(), seq == 10, "{cut:?}");
        }
        st.set_inc_seam(in_boot(2));
        let moved = st.set_aside_torn().expect("the sweep");
        assert_eq!(
            moved.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
            ["inflight/10", "inflight/8", "inflight/9"],
            "every cut entry, and not the whole one"
        );
        assert_eq!(st.in_flight().expect("reads"), vec![(7, whole)]);
    }

    #[test]
    fn damage_in_a_boot_nobody_can_name_is_refused_not_set_aside() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("unnamed")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(IncSeam {
            boot: Some(None),
            ..IncSeam::default()
        });
        st.take_incarnation(0).expect("an attach");
        std::fs::create_dir_all(root.join("sent")).expect("sent/");
        std::fs::write(root.join("sent/s-a.1"), b"12 key ident").expect("a damaged reservation");
        assert!(st.set_aside_torn().expect("no sweep").is_empty());
        assert!(st.post_pin("s-a", 1).is_err(), "refused, fail-closed");
    }

    #[test]
    fn a_torn_logpin_is_left_for_the_mirror_to_refuse() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("logpin")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.take_incarnation(0).expect("the last attach, in boot 1");
        std::fs::write(root.join("logpin"), b"").expect("torn to nothing");
        st.set_inc_seam(in_boot(2));
        assert!(st.set_aside_torn().expect("the sweep").is_empty());
        assert_eq!(
            st.reconcile_logpin().expect("the reconcile"),
            Reconciled::Kept,
            "no floor on the media: the pin holds the only copies"
        );
        assert_eq!(st.logpin().expect("read").as_deref(), Some(""));
    }

    fn mirror_pin(key: [u8; 16], total: u64) -> crate::mirror::Pin {
        crate::mirror::Pin {
            inc: 1,
            total,
            key: Some(key),
            win: Vec::new(),
            slots: vec![crate::mirror::PinSlot {
                pseq: 9,
                sid: "s-a".to_string(),
                at: aterm_slog::BusCursor::new(4, 2, 1),
                body: "v=2 t=1 ep=4 seq=2 ms=1 op=open node=t1 k=turn src=screen conf=strong"
                    .to_string(),
            }],
        }
    }

    #[test]
    fn a_torn_logpin_with_its_copies_is_set_aside_after_an_unclean_stop_and_kept_otherwise() {
        let key = [0x5a; 16];
        for (world, key_copy, floor_copy, reboot, clean, older) in [
            (
                "an unclean stop, then a reboot",
                true,
                true,
                true,
                false,
                false,
            ),
            ("a kill, in the same boot", true, true, false, false, false),
            ("no floor on the media", true, false, true, false, false),
            ("no key in logkey", false, true, true, false, false),
            ("a clean stop, then a reboot", true, true, true, true, false),
            (
                "an older build's pin over the copies",
                true,
                true,
                true,
                false,
                true,
            ),
        ] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("logpin")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).expect("open");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("the last bridge's run, in boot 1");
            st.take_incarnation(0).expect("its attach");
            if key_copy {
                st.set_logkey(&key).expect("the key's copy");
            }
            if floor_copy {
                assert_eq!(st.raise_logfloor(5000).expect("the floor"), 5000);
            }
            st.set_logpin(&mirror_pin(key, 3000).render())
                .expect("the first pin");
            let pin = mirror_pin(key, 4000);
            st.set_logpin(&pin.render()).expect("the pin");
            if clean {
                let before = st.ops();
                st.end_run_clean().expect("a clean stop");
                let ops = st.ops() - before;
                assert_eq!(
                    (ops.replaces, ops.barriers, ops.fsyncs),
                    (1, 4, 1),
                    "{world}: the pin flushed where it is, `sent/` (which every state dir has) \
                     and the root ordered after it, then `run`'s bytes ordered and the root \
                     flushed in full"
                );
            }
            let mut text = pin.render();
            if older {
                text = text.replacen("logpin v=2", "logpin v=1", 1);
            }
            let cut = &text.as_bytes()[..text.len() - 7];
            std::fs::write(root.join("logpin"), cut).expect("cut short");
            if reboot {
                st.set_inc_seam(in_boot(2));
            }
            let done = st.reconcile_logpin().expect("the reconcile");
            if world == "an unclean stop, then a reboot" {
                assert_eq!(
                    done,
                    Reconciled::SetAside(SetAside {
                        from: "logpin".to_string(),
                        to: format!("{QUARANTINE}/logpin"),
                    }),
                    "{world}"
                );
                assert_eq!(
                    std::fs::read(root.join("quarantine/logpin")).expect("kept"),
                    cut,
                    "its exact bytes, never deleted"
                );
                let placed = st.logpin().expect("reads").expect("a pin in its place");
                assert!(
                    crate::mirror::Pin::by_this_build(placed.as_bytes()),
                    "of the version an older build refuses: {placed:?}"
                );
                let from_the_copies = crate::mirror::Pin {
                    key: Some(key),
                    total: 5000,
                    ..crate::mirror::Pin::default()
                };
                assert_eq!(
                    crate::mirror::Pin::parse(&placed),
                    Ok(from_the_copies.clone()),
                    "no batch, logkey's key, logfloor's total"
                );
                assert!(
                    !st.unflushed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains(LOGPIN),
                    "written durably: no cut brings back the absence"
                );
                assert_eq!(
                    st.logkey().expect("reads"),
                    Some(key),
                    "the key's copy stays"
                );
                assert_eq!(
                    st.logfloor().expect("reads"),
                    Some(5000),
                    "so does the floor"
                );
                assert_eq!(st.reconcile_logpin().expect("again"), Reconciled::Kept);
                std::fs::remove_file(root.join("logpin")).expect("gone");
                assert_eq!(
                    st.reconcile_logpin().expect("the reconcile"),
                    Reconciled::Restored
                );
                assert_eq!(
                    st.logpin()
                        .expect("reads")
                        .map(|t| crate::mirror::Pin::parse(&t)),
                    Some(Ok(from_the_copies)),
                    "written again"
                );
                st.set_logpin(&mirror_pin(key, 100).render())
                    .expect("an older pin");
                assert_eq!(
                    st.reconcile_logpin().expect("the reconcile"),
                    Reconciled::Raised {
                        from: 100,
                        to: 5000
                    }
                );
                let raised =
                    crate::mirror::Pin::parse(&st.logpin().expect("reads").expect("a pin"))
                        .expect("whole");
                assert_eq!(raised, mirror_pin(key, 5000), "its batch and key kept");
                assert_eq!(st.reconcile_logpin().expect("again"), Reconciled::Kept);
            } else {
                assert_eq!(done, Reconciled::Kept, "{world}");
                assert_eq!(
                    st.logpin()
                        .expect("reads")
                        .map(String::into_bytes)
                        .as_deref(),
                    Some(cut),
                    "{world}: kept as it is, for the mirror to refuse"
                );
                assert!(!root.join(QUARANTINE).exists(), "{world}: nothing moved");
            }
        }
    }

    #[test]
    fn the_first_pin_of_this_build_is_durable_and_the_rest_atomic() {
        let older = mirror_pin([7; 16], 10)
            .render()
            .replacen("logpin v=2", "logpin v=1", 1);
        for (world, before) in [
            ("no pin", None),
            ("an older build's pin", Some(older.as_bytes())),
            ("damage that names no version", Some(&b"logpin v="[..])),
            ("this build's pin, torn", Some(&b"logpin v=2 inc=1 n"[..])),
        ] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("logpin")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).expect("open");
            if let Some(bytes) = before {
                std::fs::write(root.join("logpin"), bytes).expect("what is there");
            }
            let ops = st.ops();
            st.set_logpin(&mirror_pin([7; 16], 20).render())
                .expect("the pin");
            assert_eq!((st.ops() - ops).fsyncs, 2, "{world}");
            let ops = st.ops();
            st.set_logpin(&mirror_pin([7; 16], 30).render())
                .expect("the next");
            assert_eq!((st.ops() - ops).fsyncs, 0, "{world}: the next is atomic");
        }

        let dir = aterm_tempfile::Builder::new()
            .prefix("logpin")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let pin = mirror_pin([7; 16], 40).render();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(IncSeam {
            kill_at: Some(IncStep::Renamed),
            ..Default::default()
        });
        assert!(
            st.set_logpin(&pin).is_err(),
            "the first flush failed after rename"
        );
        assert!(crate::mirror::Pin::by_this_build(
            &std::fs::read(root.join(LOGPIN)).unwrap()
        ));
        st.set_inc_seam(IncSeam::default());
        let ops = st.ops();
        st.set_logpin(&pin).expect("retried");
        assert_eq!((st.ops() - ops).fsyncs, 2, "the retry is still durable");
        let ops = st.ops();
        st.ensure_logpin_durable(|| panic!("a known fence needs no render or rewrite"))
            .expect("already admitted");
        assert_eq!((st.ops() - ops).replaces, 0);

        let st = StateDir::open(root).expect("reopen");
        let ops = st.ops();
        st.ensure_logpin_durable(|| pin.clone())
            .expect("reestablished after reopen");
        assert_eq!((st.ops() - ops).fsyncs, 2);
        for change in ["removed", "replaced", "legacy header"] {
            let path = root.join(LOGPIN);
            match change {
                "removed" => std::fs::remove_file(&path).unwrap(),
                "replaced" => {
                    let staged = root.join("other-pin");
                    std::fs::write(&staged, &pin).unwrap();
                    std::fs::rename(staged, &path).unwrap();
                }
                _ => std::fs::write(&path, pin.replacen("v=2", "v=1", 1)).unwrap(),
            }
            let ops = st.ops();
            st.ensure_logpin_durable(|| pin.clone()).expect(change);
            assert_eq!(
                (st.ops() - ops).fsyncs,
                2,
                "{change}: admission invalidated"
            );
            let ops = st.ops();
            st.set_logpin(&pin).expect("the next own replacement");
            assert_eq!(
                (st.ops() - ops).fsyncs,
                0,
                "{change}: later own writes stay atomic"
            );
        }
        st.refuse_writes_of(Some(LOGPIN));
        assert!(
            st.set_logpin(&pin).is_err(),
            "an ordinary atomic write failed"
        );
        st.refuse_writes_of(None);
        let ops = st.ops();
        st.ensure_logpin_durable(|| pin)
            .expect("readmitted after the error");
        assert_eq!(
            (st.ops() - ops).fsyncs,
            2,
            "any write error revokes admission"
        );
    }

    #[test]
    fn the_mirrors_copies_read_back_and_the_floor_is_only_raised() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("copies")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        assert_eq!(st.logkey().expect("none"), None);
        assert_eq!(st.logfloor().expect("none"), None);
        st.set_logkey(&[3; 16]).expect("the key");
        assert_eq!(st.logkey().expect("reads"), Some([3; 16]));
        assert_eq!(st.raise_logfloor(700).expect("raised"), 700);
        let before = st.ops();
        assert_eq!(st.raise_logfloor(600).expect("kept"), 700, "never lowered");
        assert_eq!((st.ops() - before).replaces, 0, "nothing written");
        assert_eq!(st.raise_logfloor(900).expect("raised"), 900);
        assert_eq!(st.logfloor().expect("reads"), Some(900));
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("a run in boot 1");
        std::fs::write(root.join("logpin"), b"logpin v=2 inc=1 n=0 to").expect("a torn pin");
        st.set_inc_seam(in_boot(2));
        for (name, damage) in [("logfloor", &b"90"[..]), ("logkey", &b"03030303\n"[..])] {
            let good = std::fs::read(root.join(name)).expect("the copy");
            std::fs::write(root.join(name), damage).expect("damaged");
            assert_eq!(
                if name == "logkey" {
                    st.logkey().map(|_| ()).expect_err("refused").kind()
                } else {
                    st.logfloor().map(|_| ()).expect_err("refused").kind()
                },
                io::ErrorKind::InvalidData,
                "{name}"
            );
            assert!(
                st.reconcile_logpin().is_err(),
                "{name}: no move on a copy that does not read"
            );
            assert!(root.join("logpin").exists(), "{name}: the pin stays");
            std::fs::write(root.join(name), good).expect("repaired");
        }
        assert!(matches!(
            st.reconcile_logpin().expect("the reconcile"),
            Reconciled::SetAside(_)
        ));
    }
}
