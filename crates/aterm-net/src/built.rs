// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE `built` OP'S WIRE (DC-24b, [`crate::fleet::FleetOp::Built`]): a fleet member whose build
//! would compile a shared dependency unit asks another member for it by key only, the SHA-256 of
//! the fingerprint record its targo computed, and the member that holds a unit of targo's store
//! under that key sends it file by file, each with its size and SHA-256 ahead of its bytes.
//!
//! On one proved connection the asker sends any number of asks, one line each, and reads one
//! answer to each before it sends the next:
//!
//! ```text
//! ask:     has <key>                     whether this member holds a unit under the key
//! answer:  yes | none
//! ask:     get <key>                     the unit itself
//! answer:  none                          this member holds no unit under the key (now)
//!          unit <dir> <n> <bytes>        the unit at <dir> (relative to the store), then n files
//!                                        of <bytes> in all:
//!          file <path> <size> <sha256>   <path> relative to the unit (fingerprint/… or out/…),
//!          <size bytes>                  then its bytes
//! either:  err <why>                     the member refuses (sharing off, an ask out of
//!                                        grammar), and closes
//! ```
//!
//! A key is 64 lowercase hex digits. Nothing lists what a store holds: a member answers only the
//! key it is asked. An asker asks every member it may with `has` at once and takes the unit with
//! `get` from one that said `yes`, so a unit costs one round trip to each member and one
//! transfer. The asker keeps a file only where what landed matches the size and SHA-256 that
//! were sent ([`get`]); whether the unit is the one its build computed is its build's own
//! judgement. Every read of the asker is bounded by its ask's [`Bounds`]: a phase's own timeout
//! and the ask's one deadline, which a member that trickles bytes cannot outrun.

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_digest::Sha256;

/// The digits of a key: a SHA-256 in lowercase hex.
pub const KEY_DIGITS: usize = 64;

/// The most files one unit is sent with.
pub const MAX_FILES: usize = 512;

/// The most bytes one unit is sent with, and so the largest file: a shared dependency's archive
/// is far below it, and a larger unit compiles here. It also bounds what one ask may land on
/// this Mac's disk before its bytes are judged.
pub const MAX_UNIT: u64 = 2 << 30;

/// The longest line either end reads: an ask, an answer's head or a file's.
const LINE_MAX: u64 = 1024;

/// How long a member may take to answer an ask's line: a LAN member answers at once, and one
/// that does not is passed over and the unit compiles here (atpkg's `PEER_CONNECT`).
pub const ANSWER_WITHIN: Duration = Duration::from_millis(750);

/// No byte of a unit for this long ends its transfer: inside the 2 s of silence after which
/// targo passes its unit source over (Trust's `unit_source::SILENCE`), so a member that stalls
/// is passed over before the source is.
pub const IDLE: Duration = Duration::from_millis(1500);

/// Each [`STEP`] bytes of a unit must move within this of the step before (atpkg's
/// `PEER_PACE`: about 100 KiB/s at least).
pub const WINDOW: Duration = Duration::from_secs(10);

/// The bytes each [`WINDOW`] must move.
pub const STEP: u64 = 1024 * 1024;

/// Whether `key` is one: exactly [`KEY_DIGITS`] lowercase hex digits.
#[must_use]
pub fn is_key(key: &str) -> bool {
    key.len() == KEY_DIGITS
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The lowercase hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    digest.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Whether `dir` names a unit's directory in targo's store: `build/<name>/<hash>`, or
/// `build/git/<commit>/<name>/<hash>` for a Git source, each a plain name.
#[must_use]
pub fn is_unit_dir(dir: &str) -> bool {
    let parts: Vec<&str> = dir.split('/').collect();
    parts.first() == Some(&"build")
        && matches!(parts.len(), 3 | 5)
        && parts[1..].iter().all(|part| plain_name(part))
        && (parts.len() == 3 || parts[1] == "git")
}

/// Whether `path` names a file of a unit that is sent: `fingerprint/<name>` or `out/<name>`.
#[must_use]
pub fn is_unit_file(path: &str) -> bool {
    match path.split_once('/') {
        Some(("fingerprint" | "out", name)) => plain_name(name),
        _ => false,
    }
}

/// A file or folder name with no separator, no leading dot and no control or space character.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| !c.is_control() && c != '/' && c != '\\' && c != ' ')
        && Path::new(name)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// A unit a member holds, as its answer sends it.
pub struct Held {
    /// Its directory, relative to the store ([`is_unit_dir`]).
    pub dir: String,
    /// Each file sent: its path relative to the unit ([`is_unit_file`]) and where it is read.
    pub files: Vec<(String, PathBuf)>,
    /// What keeps the unit as it is while it is sent: its build lock, held shared.
    pub hold: Box<dyn std::any::Any + Send>,
}

/// What one connection's answers came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Served {
    /// The keys asked, by `has` or `get`.
    pub asked: u64,
    /// The units sent.
    pub units: u64,
    /// The bytes of the files sent.
    pub bytes: u64,
    /// Why the connection ended other than by the asker's close, if it did.
    pub ended: Option<String>,
}

/// One ask a member reads.
#[derive(Debug, PartialEq, Eq)]
enum Ask<'a> {
    Has(&'a str),
    Get(&'a str),
}

fn ask_of(line: &str) -> Option<Ask<'_>> {
    match line.split_once(' ')? {
        ("has", key) if is_key(key) => Some(Ask::Has(key)),
        ("get", key) if is_key(key) => Some(Ask::Get(key)),
        _ => None,
    }
}

/// Answer every ask on `stream` until the asker closes it: `has` with `yes` where `find` holds a
/// unit under its key and `none` where it does not, `get` with the unit ([`Held`]) or `none`.
/// `refusal`, where given, is answered as `err` to the first ask and ends the connection (the
/// person turned sharing off); so is an ask out of the grammar. The caller bounds each read and
/// write by the stream's own timeouts.
///
/// # Errors
/// A write to the asker failed.
pub fn serve<S: Read + Write>(
    stream: &mut S,
    refusal: Option<&str>,
    find: &mut dyn FnMut(&str) -> Option<Held>,
) -> io::Result<Served> {
    let mut served = Served::default();
    loop {
        let line = match read_line(stream, None) {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(served),
            Err(error) => {
                served.ended = Some(error.to_string());
                return Ok(served);
            }
        };
        if let Some(why) = refusal {
            stream.write_all(format!("err {why}\n").as_bytes())?;
            stream.flush()?;
            served.ended = Some(why.to_owned());
            return Ok(served);
        }
        let Some(ask) = ask_of(&line) else {
            stream.write_all(b"err not an ask\n")?;
            stream.flush()?;
            served.ended = Some("an ask out of grammar".to_owned());
            return Ok(served);
        };
        served.asked += 1;
        match ask {
            Ask::Has(key) => {
                let held = find(key).is_some();
                stream.write_all(if held { b"yes\n" } else { b"none\n" })?;
                stream.flush()?;
            }
            Ask::Get(key) => {
                let Some(held) = find(key) else {
                    stream.write_all(b"none\n")?;
                    stream.flush()?;
                    continue;
                };
                match send(stream, &held)? {
                    Sent::Whole(bytes) => {
                        served.units += 1;
                        served.bytes += bytes;
                    }
                    Sent::Unread => {}
                    Sent::Cut => {
                        served.ended = Some("a held file could not be read whole".to_owned());
                        return Ok(served);
                    }
                }
            }
        }
    }
}

/// What [`send`] came to.
enum Sent {
    /// The unit went whole: its files' bytes.
    Whole(u64),
    /// A file could not be read before anything went, or the unit is larger than the wire
    /// carries: answered `none`.
    Unread,
    /// A file changed or could not be read once its line went: the connection is cut, so the
    /// asker keeps nothing of the unit.
    Cut,
}

/// Send `held`: its head, then each file's line and bytes, hashed first so the line says them.
fn send<S: Write>(stream: &mut S, held: &Held) -> io::Result<Sent> {
    let mut measured = Vec::with_capacity(held.files.len());
    let mut total = 0_u64;
    for (rel, path) in &held.files {
        match measure(path) {
            Ok(Some((size, sha))) => {
                total = total.saturating_add(size);
                measured.push((rel, path, size, sha));
            }
            Ok(None) | Err(_) => {
                stream.write_all(b"none\n")?;
                stream.flush()?;
                return Ok(Sent::Unread);
            }
        }
    }
    if measured.is_empty() || measured.len() > MAX_FILES || total > MAX_UNIT {
        stream.write_all(b"none\n")?;
        stream.flush()?;
        return Ok(Sent::Unread);
    }
    stream.write_all(format!("unit {} {} {total}\n", held.dir, measured.len()).as_bytes())?;
    for (rel, path, size, sha) in measured {
        stream.write_all(format!("file {rel} {size} {sha}\n").as_bytes())?;
        let Ok(mut file) = open_plain(path) else {
            return Ok(Sent::Cut);
        };
        let copied = io::copy(&mut (&mut file).take(size), stream)?;
        if copied != size {
            return Ok(Sent::Cut);
        }
    }
    stream.flush()?;
    Ok(Sent::Whole(total))
}

/// A plain file's size and SHA-256, where it is one and within [`MAX_UNIT`].
fn measure(path: &Path) -> io::Result<Option<(u64, String)>> {
    let mut file = open_plain(path)?;
    let size = file.metadata()?.len();
    if size > MAX_UNIT {
        return Ok(None);
    }
    let mut hash = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    let mut read = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        hash.update(&buf[..n]);
    }
    Ok((read == size).then(|| (size, hex(&hash.finalize()))))
}

/// `path` opened for reading where it is a plain file, never through a link.
fn open_plain(path: &Path) -> io::Result<std::fs::File> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(io::Error::other("not a plain file"));
    }
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a plain file"));
    }
    Ok(file)
}

/// A file of a unit as it landed in the staging folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landed {
    /// Its path relative to the unit ([`is_unit_file`]).
    pub rel: String,
    /// Its size.
    pub size: u64,
    /// Its SHA-256, which the member sent and the bytes that landed match.
    pub sha256: String,
}

/// A unit that landed whole: its directory relative to the store and its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    /// The unit's directory relative to the store ([`is_unit_dir`]).
    pub dir: String,
    /// Each file, as it landed.
    pub files: Vec<Landed>,
}

/// Why an ask came to nothing from this member.
#[derive(Debug)]
pub enum Missed {
    /// It did not answer within [`ANSWER_WITHIN`], its transfer stalled for [`IDLE`] or fell
    /// below the pace, or the ask's deadline passed.
    Slow(String),
    /// What it sent did not match what it said, or broke the wire's grammar.
    Transfer(String),
    /// It refused (`err …`), or could not be reached or proved.
    Refused(String),
    /// It closed the connection before answering: a listener ends one it has served for its
    /// lifetime, so the asker may dial it again.
    Closed,
    /// It closed the connection partway through a unit: as [`Self::Closed`], on a connection
    /// the listener may have ended for its lifetime.
    Cut(String),
    /// This Mac has no room for the unit it announced: its disk would fall below the floor.
    /// The member is not at fault; the connection, with the unit's bytes still on it, is not
    /// read again.
    NoRoom(u64),
    /// The connection or this Mac's own disk failed.
    Io(io::Error),
}

impl std::fmt::Display for Missed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Slow(why) => write!(f, "too slow: {why}"),
            Self::Transfer(why) => write!(f, "transfer: {why}"),
            Self::Refused(why) => write!(f, "refused: {why}"),
            Self::Closed => f.write_str("closed before it answered"),
            Self::Cut(why) => write!(f, "closed mid-unit: {why}"),
            Self::NoRoom(bytes) => write!(f, "no room here for {bytes} bytes"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

/// What bounds an asker's reads: `timeout` sets the stream's read timeout, and no read starts
/// after `deadline`, the ask's own, so the slowest a member can be is one timeout past it.
pub struct Bounds<'a> {
    /// Sets the stream's read timeout.
    pub timeout: &'a dyn Fn(Duration) -> io::Result<()>,
    /// The ask's deadline.
    pub deadline: Instant,
}

impl Bounds<'_> {
    /// The stream's read timeout set to `within`, or what is left of the ask where less is.
    fn arm(&self, within: Duration) -> Result<(), Missed> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(past_deadline());
        }
        (self.timeout)(within.min(left)).map_err(Missed::Io)
    }
}

fn past_deadline() -> Missed {
    Missed::Slow("the ask's deadline passed".to_owned())
}

/// Send `line` as an ask.
fn send_ask<S: Write + ?Sized>(stream: &mut S, line: &str) -> Result<(), Missed> {
    stream
        .write_all(format!("{line}\n").as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|error| match error.kind() {
            io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof => Missed::Closed,
            _ => Missed::Io(error),
        })
}

/// The answer's head to an ask just sent, read within [`ANSWER_WITHIN`].
fn answer_head<S: Read + ?Sized>(stream: &mut S, bounds: &Bounds<'_>) -> Result<String, Missed> {
    bounds.arm(ANSWER_WITHIN)?;
    let head = match read_line(stream, Some(bounds.deadline)) {
        Ok(Some(head)) => head,
        Ok(None) => return Err(Missed::Closed),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
            ) =>
        {
            return Err(Missed::Closed);
        }
        Err(error) if timed_out(&error) => {
            return Err(Missed::Slow(format!(
                "no answer within {} ms",
                ANSWER_WITHIN.as_millis()
            )));
        }
        Err(error) => return Err(Missed::Io(error)),
    };
    if let Some(why) = head.strip_prefix("err ") {
        return Err(Missed::Refused(aterm_codec::tty::one_line(why, 200)));
    }
    Ok(head)
}

/// Ask whether the member holds a unit under `key`.
///
/// # Errors
/// Why the member is passed over ([`Missed`]).
pub fn has<S: Read + Write + ?Sized>(
    stream: &mut S,
    key: &str,
    bounds: &Bounds<'_>,
) -> Result<bool, Missed> {
    bounds.arm(ANSWER_WITHIN)?;
    send_ask(stream, &format!("has {key}"))?;
    match answer_head(stream, bounds)?.as_str() {
        "yes" => Ok(true),
        "none" => Ok(false),
        _ => Err(Missed::Transfer(
            "an answer that is not yes or none".to_owned(),
        )),
    }
}

/// Ask for the unit under `key` on `stream` and land what the member sends in `into` (the
/// staging folder, which exists and is empty), each file at its path relative to the unit,
/// written new and hashed as it lands. `room` says whether the bytes the unit's head announces
/// may land on this Mac's disk; `working` is called as bytes land. `Ok(None)`: the member holds
/// no unit under the key now. A file that does not match what was sent, a transfer that stalls
/// or falls below the pace, or one past the ask's deadline removes every file this ask wrote.
///
/// # Errors
/// Why the unit was not taken from the member ([`Missed`]).
pub fn get<S: Read + Write + ?Sized>(
    stream: &mut S,
    key: &str,
    into: &Path,
    bounds: &Bounds<'_>,
    room: &dyn Fn(u64) -> bool,
    working: &mut dyn FnMut(u64),
) -> Result<Option<Received>, Missed> {
    bounds.arm(ANSWER_WITHIN)?;
    send_ask(stream, &format!("get {key}"))?;
    let head = answer_head(stream, bounds)?;
    if head == "none" {
        return Ok(None);
    }
    let mut words = head.split(' ');
    let (Some("unit"), Some(dir), Some(count), Some(bytes), None) = (
        words.next(),
        words.next(),
        words.next(),
        words.next(),
        words.next(),
    ) else {
        return Err(Missed::Transfer("an answer that is not a unit".to_owned()));
    };
    let count = count
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=MAX_FILES).contains(n));
    let bytes = bytes.parse::<u64>().ok().filter(|bytes| *bytes <= MAX_UNIT);
    let (Some(count), Some(bytes)) = (count, bytes) else {
        return Err(Missed::Transfer("an answer that is not a unit".to_owned()));
    };
    if !is_unit_dir(dir) {
        return Err(Missed::Transfer("an answer that is not a unit".to_owned()));
    }
    if !room(bytes) {
        return Err(Missed::NoRoom(bytes));
    }
    let dir = dir.to_owned();
    let mut written = Vec::new();
    let landed = land(stream, into, (count, bytes), bounds, &mut written, working);
    match landed {
        Ok(files) => Ok(Some(Received { dir, files })),
        Err(missed) => {
            for path in written {
                let _ = std::fs::remove_file(path);
            }
            Err(missed)
        }
    }
}

fn timed_out(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

/// The pace a transfer keeps: each [`STEP`] within [`WINDOW`] of the one before.
struct Pace {
    since: Instant,
    moved: u64,
}

impl Pace {
    fn moved(&mut self, n: u64) -> Result<(), Missed> {
        self.moved += n;
        if self.moved >= STEP {
            self.moved = 0;
            self.since = Instant::now();
        } else if self.since.elapsed() > WINDOW {
            return Err(Missed::Slow(format!(
                "fewer than {STEP} bytes in {} s",
                WINDOW.as_secs()
            )));
        }
        Ok(())
    }
}

/// Land a unit's `count` files of `announced` bytes in all.
fn land<S: Read + ?Sized>(
    stream: &mut S,
    into: &Path,
    (count, announced): (usize, u64),
    bounds: &Bounds<'_>,
    written: &mut Vec<PathBuf>,
    working: &mut dyn FnMut(u64),
) -> Result<Vec<Landed>, Missed> {
    bounds.arm(IDLE)?;
    let mut pace = Pace {
        since: Instant::now(),
        moved: 0,
    };
    let mut total = 0_u64;
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        let line = match read_line(stream, Some(bounds.deadline)) {
            Ok(Some(line)) => line,
            Ok(None) => return Err(Missed::Cut("before a file's head".to_owned())),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(Missed::Cut("in a file's head".to_owned()));
            }
            Err(error) if timed_out(&error) => {
                return Err(Missed::Slow("a file's head did not come".to_owned()));
            }
            Err(error) => return Err(Missed::Io(error)),
        };
        let mut words = line.split(' ');
        let (Some("file"), Some(rel), Some(size), Some(sha), None) = (
            words.next(),
            words.next(),
            words.next(),
            words.next(),
            words.next(),
        ) else {
            return Err(Missed::Transfer("a file head out of grammar".to_owned()));
        };
        let size = size
            .parse::<u64>()
            .ok()
            .filter(|size| *size <= MAX_UNIT)
            .ok_or_else(|| Missed::Transfer("a file size out of bounds".to_owned()))?;
        total = total.saturating_add(size);
        if !is_unit_file(rel) || !is_key(sha) {
            return Err(Missed::Transfer("a file head out of grammar".to_owned()));
        }
        if total > announced {
            return Err(Missed::Transfer(
                "more bytes than the unit's head announced".to_owned(),
            ));
        }
        if files.iter().any(|file: &Landed| file.rel == rel) {
            return Err(Missed::Transfer(format!("{rel} sent twice")));
        }
        let path = into.join(rel);
        if let Some(parent) = path.parent() {
            private_dir(parent).map_err(Missed::Io)?;
        }
        let mut out = new_file(&path).map_err(Missed::Io)?;
        written.push(path.clone());
        let mut hash = Sha256::new();
        let mut left = size;
        let mut buf = vec![0; 64 * 1024];
        while left > 0 {
            if Instant::now() >= bounds.deadline {
                return Err(past_deadline());
            }
            let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
            let n = match stream.read(&mut buf[..want]) {
                Ok(0) => return Err(Missed::Cut(format!("{rel} cut short"))),
                Ok(n) => n,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if timed_out(&error) => {
                    return Err(Missed::Slow(format!("no byte for {} ms", IDLE.as_millis())));
                }
                Err(error) => return Err(Missed::Io(error)),
            };
            hash.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(Missed::Io)?;
            left -= n as u64;
            pace.moved(n as u64)?;
            working(n as u64);
        }
        out.flush().map_err(Missed::Io)?;
        if hex(&hash.finalize()) != sha {
            return Err(Missed::Transfer(format!(
                "{rel} does not match the SHA-256 sent"
            )));
        }
        files.push(Landed {
            rel: rel.to_owned(),
            size,
            sha256: sha.to_owned(),
        });
    }
    if total != announced {
        return Err(Missed::Transfer(
            "fewer bytes than the unit's head announced".to_owned(),
        ));
    }
    Ok(files)
}

/// `dir` made private (0700) where it is not there yet; one there must be a plain directory.
fn private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    match builder.create(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(dir)?.is_dir() {
                Ok(())
            } else {
                Err(io::Error::other(
                    "a staging folder is not a plain directory",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

/// `path` created new, readable and writable by this user alone.
fn new_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// One line of at most [`LINE_MAX`] bytes, without its newline; `None` at a clean end. Where a
/// `deadline` is given no byte is read after it (a timed-out read), so a member that sends a line
/// a byte at a time cannot hold the reader past it.
fn read_line<S: Read + ?Sized>(
    stream: &mut S,
    deadline: Option<Instant>,
) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    // One byte at a time, so nothing past the line is taken from the stream: a unit's bytes
    // follow its lines on the same stream.
    let mut byte = [0_u8; 1];
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the ask's deadline passed",
            ));
        }
        match stream.read(&mut byte) {
            Ok(0) if line.is_empty() => return Ok(None),
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "a cut line")),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                line.push(byte[0]);
                if line.len() as u64 > LINE_MAX {
                    return Err(io::Error::other("an overlong line"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| io::Error::other("a line that is not UTF-8"))
}

#[cfg(test)]
#[path = "built_tests.rs"]
mod tests;
