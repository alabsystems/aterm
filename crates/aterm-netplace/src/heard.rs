// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The evidence every process notes into, `<state>/netplace/heard`, and the fleet book's
//! snapshot beside it, `<state>/netplace/book`, each answer there with the network it was booked
//! on.
//! Direct scan observations live separately under `observations/`: presentation combines them
//! with this evidence, while admission reads only this ledger.
//!
//! THE LOCK RULE: every write of `heard`, by any process (the window, the node job, a vault job,
//! a CLI), holds `heard.lock` for one bounded read-modify-rename: read the file, add the notes,
//! keep the newest answer per member and lane and the newest rounds and internet notes (at most
//! [`MAX_LINES`]), write it whole by atomic rename. The keeper holds `keeper.lock`, never this
//! one beyond its own notes, so no process is ever kept from flushing a note. A writer waits for
//! the lock at most [`LOCK_WAIT`]; one that cannot have it keeps the note for its next. Readers
//! take no lock: the rename replaces the file whole.
//!
//! Only authenticated member answers are noted ([`note_answer`]): a proven node-keyed fleet
//! answer, a verified Commons contact. Each carries its local route address, sampled when
//! noted for fleet answers (with the interface that held it) or captured from the authenticated
//! socket for Commons contacts, so the place counts it for the network it came over and not for
//! the one this Mac moved to.
//!
//! THE HOME-DAY LEDGER, `<state>/netplace/home-days` ([`HomeDays`],
//! `docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.2 M2, §11 B6): for each network, the days on
//! which members were heard on it and which ones ([`note_days`]), and the days the one-time seed
//! credited it with ([`seed_days`]), from which the vault's automatic admission counts the
//! distinct days a network has been home on this Mac (`aterm_secrets::home_lan::HOME_DAYS`). It
//! sits apart from [`FILE`], so a build that predates it, which drops a line it cannot read when
//! it rewrites [`FILE`] and reads such a file as torn, neither loses it nor stops admitting. Its
//! writes hold [`DAYS_LOCK`] as those of [`FILE`] hold [`LOCK`]; readers take no lock.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::place::{
    Answer, Book, BookMember, Evidence, InternetNote, InternetSource, Known, Lane, NetworkId,
    Round, member_key,
};

/// The evidence file's name.
pub const FILE: &str = "heard";

/// The lock every write of [`FILE`] holds.
pub const LOCK: &str = "heard.lock";

/// The fleet book snapshot's file name.
pub const BOOK_FILE: &str = "book";

/// The most lines [`FILE`] keeps.
pub const MAX_LINES: usize = 512;

/// The most rounds kept.
const MAX_ROUNDS: usize = 64;

/// The most internet notes kept.
const MAX_INTERNET: usize = 32;

/// The longest a writer waits for [`LOCK`].
pub const LOCK_WAIT: Duration = Duration::from_millis(200);

/// The most bytes read from either file.
const MAX_BYTES: usize = 256 * 1024;

/// How often one process notes the same member on the same lane, or the same internet verdict,
/// within one stay; the first after a move is noted at once.
pub const COALESCE_MS: u64 = 60_000;

fn answer_line(answer: &Answer) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(96);
    let _ = write!(out, "a {} {} ", answer.lane.word(), answer.member);
    match answer.addr {
        Some(addr) => {
            let _ = write!(out, "{addr} ");
        }
        None => out.push_str("- "),
    }
    match answer.via {
        Some(via) => {
            let _ = write!(out, "{via} ");
        }
        None => out.push_str("- "),
    }
    let _ = write!(
        out,
        "{} {} {} {} {}",
        answer
            .iface
            .as_deref()
            .filter(|iface| valid_iface(iface))
            .unwrap_or("-"),
        answer.network.as_ref().map_or("-", NetworkId::as_str),
        if answer.logged { LOGGED } else { "-" },
        answer.began_ms,
        answer.at_ms
    );
    out
}

/// An answer line's word for [`Answer::logged`] (`-` for not).
const LOGGED: &str = "log";

/// The longest interface name an answer line takes.
const MAX_IFACE: usize = 16;

/// Whether `iface` is an interface name an answer line holds: letters and digits, at most
/// [`MAX_IFACE`] of them (`en0`, `utun4`).
fn valid_iface(iface: &str) -> bool {
    !iface.is_empty()
        && iface.len() <= MAX_IFACE
        && iface.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn round_line(round: &Round) -> String {
    format!(
        "r {} {} {} {} {}",
        round.lane.word(),
        round.answered,
        round.began_ms,
        round.at_ms,
        round.blocked
    )
}

fn internet_line(note: &InternetNote) -> String {
    format!(
        "i {} {} {} {}",
        if note.ok { "ok" } else { "down" },
        note.source.word(),
        note.at_ms,
        endpoint_word(&note.endpoint)
    )
}

/// The longest endpoint word an internet line holds.
const MAX_ENDPOINT: usize = 128;

fn valid_endpoint(endpoint: &str) -> bool {
    !endpoint.is_empty()
        && endpoint.len() <= MAX_ENDPOINT
        && endpoint.bytes().all(|b| b.is_ascii_graphic())
}

/// One endpoint word: a host name or address, no space or control character.
fn endpoint_word(endpoint: &str) -> std::borrow::Cow<'_, str> {
    if valid_endpoint(endpoint) {
        return std::borrow::Cow::Borrowed(endpoint);
    }
    let word: String = endpoint
        .chars()
        .filter(|c| c.is_ascii_graphic())
        .take(MAX_ENDPOINT)
        .collect();
    if word.is_empty() {
        std::borrow::Cow::Borrowed("-")
    } else {
        std::borrow::Cow::Owned(word)
    }
}

fn parse_opt<T: std::str::FromStr>(word: &str) -> Option<Option<T>> {
    if word == "-" {
        Some(None)
    } else {
        word.parse().ok().map(Some)
    }
}

/// The evidence an [`FILE`] text holds; lines this build cannot read are passed over.
#[must_use]
pub fn parse(text: &str) -> Evidence {
    parse_counted(text).0
}

fn is_lower_hex16(s: &str) -> bool {
    s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// [`parse`], with how many of the text's lines it passed over: each line that is not empty and
/// gave no note (malformed, torn, unterminated by `'\n'`, or of a kind this build does not write).
#[must_use]
pub fn parse_counted(text: &str) -> (Evidence, usize) {
    let mut evidence = Evidence::default();
    let mut passed = 0;
    for chunk in text.split_inclusive('\n') {
        let Some(line) = chunk.strip_suffix('\n') else {
            if !chunk.is_empty() {
                passed += 1;
            }
            continue;
        };
        if line.ends_with('\r') {
            passed += 1;
            continue;
        }
        let notes = evidence.answers.len() + evidence.rounds.len() + evidence.internet.len();
        parse_line(line, &mut evidence);
        if !line.is_empty()
            && evidence.answers.len() + evidence.rounds.len() + evidence.internet.len() == notes
        {
            passed += 1;
        }
    }
    (evidence, passed)
}

/// Adds the note `line` holds to `evidence`; a line this build cannot read adds none.
fn parse_line(line: &str, evidence: &mut Evidence) {
    let mut words = [""; 11];
    let mut len = 0;
    for w in line.split(' ') {
        if len < words.len() {
            words[len] = w;
            len += 1;
        } else {
            return;
        }
    }
    match &words[..len] {
        [
            "a",
            lane,
            member,
            addr,
            via,
            iface,
            network,
            logged,
            began,
            at,
        ] => {
            let (Some(lane), Some(addr), Some(via), Ok(began_ms), Ok(at_ms)) = (
                Lane::from_word(lane),
                parse_opt::<IpAddr>(addr),
                parse_opt::<Ipv4Addr>(via),
                began.parse(),
                at.parse(),
            ) else {
                return;
            };
            let iface = match *iface {
                "-" => None,
                iface if valid_iface(iface) => Some(iface.to_owned()),
                _ => return,
            };
            let network = match *network {
                "-" => None,
                network => match NetworkId::parse(network) {
                    Some(network) => Some(network),
                    None => return,
                },
            };
            let logged = match *logged {
                LOGGED => true,
                "-" => false,
                _ => return,
            };
            if is_lower_hex16(member) {
                evidence.answers.push(Answer {
                    lane,
                    member: (*member).to_owned(),
                    addr,
                    via,
                    iface,
                    network,
                    logged,
                    began_ms,
                    at_ms,
                });
            }
        }
        ["r", lane, answered, began, at, rest @ ..] if rest.len() <= 1 => {
            // A line with no blocked count (the first build's) reads as none blocked.
            if let (Some(lane), Ok(answered), Ok(began_ms), Ok(at_ms), Some(blocked)) = (
                Lane::from_word(lane),
                answered.parse(),
                began.parse(),
                at.parse(),
                rest.first().map_or(Some(0), |blocked| blocked.parse().ok()),
            ) {
                evidence.rounds.push(Round {
                    lane,
                    answered,
                    blocked,
                    began_ms,
                    at_ms,
                });
            }
        }
        ["i", verdict, source, at, endpoint] if valid_endpoint(endpoint) => {
            if let (Some(ok), Some(source), Ok(at_ms)) = (
                match *verdict {
                    "ok" => Some(true),
                    "down" => Some(false),
                    _ => None,
                },
                InternetSource::from_word(source),
                at.parse(),
            ) {
                evidence.internet.push(InternetNote {
                    ok,
                    source,
                    at_ms,
                    endpoint: (*endpoint).to_owned(),
                });
            }
        }
        _ => {}
    }
}

/// `evidence` bounded: the newest answer per lane and member, the newest [`MAX_ROUNDS`] rounds
/// and [`MAX_INTERNET`] internet notes, at most [`MAX_LINES`] lines in all.
#[must_use]
pub fn compact(mut evidence: Evidence) -> Evidence {
    evidence
        .answers
        .sort_by_key(|note| std::cmp::Reverse(note.at_ms));
    let mut seen_fixed = std::collections::HashSet::with_capacity(evidence.answers.len());
    let mut seen_other = std::collections::HashSet::new();
    evidence.answers.retain(|answer| {
        if let Ok(fixed) = <[u8; 16]>::try_from(answer.member.as_bytes()) {
            seen_fixed.insert((answer.lane, fixed))
        } else {
            seen_other.insert((answer.lane, answer.member.clone()))
        }
    });
    evidence
        .rounds
        .sort_by_key(|note| std::cmp::Reverse(note.at_ms));
    evidence.rounds.truncate(MAX_ROUNDS);
    evidence
        .internet
        .sort_by_key(|note| std::cmp::Reverse(note.at_ms));
    evidence.internet.truncate(MAX_INTERNET);
    evidence.answers.truncate(
        MAX_LINES
            .saturating_sub(evidence.rounds.len())
            .saturating_sub(evidence.internet.len()),
    );
    evidence
}

/// The [`FILE`] text of `evidence`, oldest first.
#[must_use]
pub fn format(evidence: &Evidence) -> String {
    let mut lines: Vec<(u64, String)> = Vec::with_capacity(
        evidence.answers.len() + evidence.rounds.len() + evidence.internet.len(),
    );
    lines.extend(evidence.answers.iter().map(|a| (a.at_ms, answer_line(a))));
    lines.extend(evidence.rounds.iter().map(|r| (r.at_ms, round_line(r))));
    lines.extend(
        evidence
            .internet
            .iter()
            .map(|i| (i.at_ms, internet_line(i))),
    );
    lines.sort_unstable();
    let mut out = String::with_capacity(lines.iter().map(|(_, l)| l.len() + 1).sum());
    for (_, line) in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The evidence in `dir`; empty where there is none or it is not private.
#[must_use]
pub fn read(dir: &Path) -> Evidence {
    aterm_uds::private_file::JudgedDir::judge(dir)
        .ok()
        .map(|judged| read_in(dir, &judged))
        .unwrap_or_default()
}

pub(crate) fn read_in(_dir: &Path, judged: &aterm_uds::private_file::JudgedDir<'_>) -> Evidence {
    judged
        .read(FILE, MAX_BYTES)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// Presentation evidence includes direct scans, stored apart from admission evidence.
/// A newer unlogged scan must neither replace nor postpone a heartbeat's proof.
#[must_use]
pub fn read_for_place(dir: &Path) -> Evidence {
    let mut evidence = read(dir);
    evidence
        .answers
        .extend(read(&dir.join("observations")).answers);
    compact(evidence)
}

pub(crate) fn read_for_place_in(
    dir: &Path,
    judged: &aterm_uds::private_file::JudgedDir<'_>,
) -> Evidence {
    let mut evidence = read_in(dir, judged);
    evidence
        .answers
        .extend(read(&dir.join("observations")).answers);
    compact(evidence)
}

/// Records proven direct-scan replies for presentation only, in a separately bounded store.
/// This never writes the admission ledger or its in-process coalescing state. Each explicit
/// scan writes one batch; it cannot postpone the first heartbeat after a network move.
pub fn note_observations(dir: &Path, mut answers: Vec<Answer>) {
    if answers.is_empty() {
        return;
    }
    for answer in &mut answers {
        answer.logged = false;
        answer.network = None;
    }
    if let Err(error) = add(
        &dir.join("observations"),
        Evidence {
            answers,
            ..Evidence::default()
        },
    ) {
        aterm_log::debug!("network place: scan observations were not recorded: {error}");
    }
}

/// The admission ledger alone, with how many lines it passed over ([`parse_counted`]).
#[must_use]
pub fn read_counted(dir: &Path) -> (Evidence, usize) {
    let Ok(judged) = aterm_uds::private_file::JudgedDir::judge(dir) else {
        return (Evidence::default(), refused(dir, FILE));
    };
    read_counted_in(dir, &judged)
}

pub(crate) fn read_counted_in(
    dir: &Path,
    judged: &aterm_uds::private_file::JudgedDir<'_>,
) -> (Evidence, usize) {
    match judged.read(FILE, MAX_BYTES) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => parse_counted(&text),
            Err(_) => (Evidence::default(), 1),
        },
        Err(aterm_uds::private_file::PrivateFileError::Missing) => (Evidence::default(), 0),
        Err(_) => (Evidence::default(), refused(dir, FILE)),
    }
}

/// How many lines a refused read of `name` in `dir` passed over: one where the file is there,
/// or `dir` is a symbolic link, and cannot be read (not private); none where it is missing.
pub(crate) fn refused(dir: &Path, name: &str) -> usize {
    let dir_is_symlink =
        std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_symlink());
    let file_present = !matches!(
        std::fs::symlink_metadata(dir.join(name)),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    );
    usize::from(dir_is_symlink || file_present)
}

/// Adds `notes` to the evidence in `dir` under [`LOCK`] ([the lock rule](self)), bounded by
/// [`compact`].
///
/// # Errors
/// The directory could not be made private, the lock was held past [`LOCK_WAIT`], or the file
/// could not be written.
pub fn add(dir: &Path, mut notes: Evidence) -> io::Result<()> {
    add_inner(dir, &mut notes)
}

fn add_inner(dir: &Path, notes: &mut Evidence) -> io::Result<()> {
    aterm_uds::private_file::ensure_private_dir(dir)?;
    let _lock = aterm_uds::private_file::PrivateLock::take(
        dir,
        LOCK,
        LOCK_WAIT,
        "the network place's evidence is being written by another process",
    )?;
    let mut evidence = read(dir);
    evidence.answers.append(&mut notes.answers);
    evidence.rounds.append(&mut notes.rounds);
    evidence.internet.append(&mut notes.internet);
    let compacted = compact(evidence);
    let formatted = format(&compacted);
    if let Err(e) = aterm_uds::private_file::write_private(dir, FILE, formatted.as_bytes()) {
        *notes = compacted;
        return Err(e);
    }
    Ok(())
}

/// The fleet book snapshot's text.
#[must_use]
pub fn format_book(book: &Book) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(book.members.len().saturating_mul(48));
    for member in &book.members {
        let _ = write!(out, "m {} ", member.member);
        match member.addr {
            Some(addr) => {
                let _ = write!(out, "{addr} ");
            }
            None => out.push_str("- "),
        }
        match member.answered_ms {
            Some(ms) => {
                let _ = write!(out, "{ms} ");
            }
            None => out.push_str("- "),
        }
        out.push_str(member.network.as_ref().map_or("-", NetworkId::as_str));
        out.push('\n');
    }
    out
}

/// The fleet book snapshot a [`format_book`] text holds; lines this build cannot read are
/// passed over.
#[must_use]
pub fn parse_book(text: &str) -> Book {
    Book {
        members: text
            .lines()
            .filter_map(|line| {
                let mut words = [""; 6];
                let mut len = 0;
                for w in line.split(' ') {
                    if len < words.len() {
                        words[len] = w;
                        len += 1;
                    } else {
                        return None;
                    }
                }
                let ["m", member, addr, answered, network @ ..] = &words[..len] else {
                    return None;
                };
                if !is_lower_hex16(member) {
                    return None;
                }
                // A line with no network (the first build's) seeds nothing.
                let network = match network {
                    [] | ["-"] => None,
                    [id] => Some(NetworkId::parse(id)?),
                    _ => return None,
                };
                Some(BookMember {
                    member: (*member).to_owned(),
                    addr: parse_opt(addr)?,
                    answered_ms: parse_opt(answered)?,
                    network,
                })
            })
            .collect(),
    }
}

/// The fleet book snapshot in `dir`; `None` where the fleet lane wrote none.
#[must_use]
pub fn read_book(dir: &Path) -> Option<Book> {
    let judged = aterm_uds::private_file::JudgedDir::judge(dir).ok()?;
    read_book_in(&judged)
}

pub(crate) fn read_book_in(judged: &aterm_uds::private_file::JudgedDir<'_>) -> Option<Book> {
    let bytes = judged.read(BOOK_FILE, MAX_BYTES).ok()?;
    Some(parse_book(std::str::from_utf8(&bytes).ok()?))
}

/// Writes the fleet book snapshot: its admitted members, this Mac aside, by node id, with each
/// one's address in the book and when it last answered this Mac (`members`), and the network each
/// answer was booked on: `network` (the network this Mac is on now) for an answer the snapshot did
/// not hold yet, the snapshot's own for one it did. Whole, by rename; the last writer stands.
///
/// # Errors
/// The directory could not be made private or the file could not be written.
pub fn write_book(
    dir: &Path,
    members: impl IntoIterator<Item = (String, Option<IpAddr>, Option<u64>)>,
    network: Option<&NetworkId>,
) -> io::Result<()> {
    let held = read_book(dir).unwrap_or_default();
    let mut held_by_member: HashMap<String, (Option<u64>, Option<NetworkId>)> =
        HashMap::with_capacity(held.members.len());
    for row in held.members {
        held_by_member
            .entry(row.member)
            .or_insert((row.answered_ms, row.network));
    }
    let network = network.filter(|network| network.is_strong());
    let book = Book {
        members: members
            .into_iter()
            .map(|(node, addr, answered_ms)| {
                let member = member_key(Lane::Fleet, &node);
                let booked = held_by_member
                    .remove(&member)
                    .filter(|(held_ms, _)| *held_ms == answered_ms)
                    .map_or_else(
                        || answered_ms.and_then(|_| network.cloned()),
                        |(_, booked_net)| booked_net,
                    );
                BookMember {
                    member,
                    addr,
                    answered_ms,
                    network: booked,
                }
            })
            .collect(),
    };
    aterm_uds::private_file::ensure_private_dir(dir)?;
    aterm_uds::private_file::write_private(dir, BOOK_FILE, format_book(&book).as_bytes())
}

/// What this process last noted for a key, and the notes that could not be written yet.
#[derive(Default)]
struct Noted {
    /// Per directory and key (`lane member`, `internet ok`), the time last noted.
    last: HashMap<PathBuf, HashMap<String, u64>>,
    /// Notes a held lock kept back, per directory.
    pending: HashMap<PathBuf, Evidence>,
}

static NOTED: Mutex<Option<Noted>> = Mutex::new(None);

/// The current stay's start in `dir`'s record, which a note after a move is flushed at once for.
fn stay_start(dir: &Path) -> Option<u64> {
    crate::record::read(dir).and_then(|written| written.record.current().map(|stay| stay.start_ms))
}

/// Whether a note for `key` at `now` is written now: never noted, noted [`COALESCE_MS`] or more
/// ago, or noted before the current stay began (the first after a move).
fn due(noted: &Noted, dir: &Path, key: &str, now: u64, stay: &mut Option<Option<u64>>) -> bool {
    noted
        .last
        .get(dir)
        .and_then(|by_key| by_key.get(key))
        .is_none_or(|last| {
            now.saturating_sub(*last) >= COALESCE_MS
                || stay
                    .get_or_insert_with(|| stay_start(dir))
                    .is_some_and(|start| *last < start)
        })
}

/// Writes `notes` (with any held back) to `dir`, recording `keys` as noted at `now`; a held
/// lock keeps them for the next note.
fn flush(dir: &Path, mut notes: Evidence, keys: Vec<String>, now: u64) {
    let mut guard = NOTED.lock().unwrap_or_else(PoisonError::into_inner);
    let noted = guard.get_or_insert_with(Noted::default);
    if let Some(held) = noted.pending.remove(dir) {
        notes.answers.extend(held.answers);
        notes.rounds.extend(held.rounds);
        notes.internet.extend(held.internet);
    }
    if !keys.is_empty() {
        let dir_last = match noted.last.get_mut(dir) {
            Some(dir_last) => dir_last,
            None => noted.last.entry(dir.to_path_buf()).or_default(),
        };
        for key in keys {
            dir_last.insert(key, now);
        }
    }
    drop(guard);
    if let Err(e) = add_inner(dir, &mut notes) {
        aterm_log::debug!("network place: a note waits for the next ({e})");
        let mut guard = NOTED.lock().unwrap_or_else(PoisonError::into_inner);
        let pending = &mut guard.get_or_insert_with(Noted::default).pending;
        let held = match pending.get_mut(dir) {
            Some(held) => held,
            None => pending.entry(dir.to_path_buf()).or_default(),
        };
        held.answers.extend(notes.answers);
        held.rounds.extend(notes.rounds);
        held.internet.extend(notes.internet);
        *held = compact(std::mem::take(held));
    }
}

/// Formats `"<lane> <member_key>"` on a 32-byte stack buffer when it fits.
fn noted_key_buf(lane: Lane, key_bytes: &[u8]) -> Option<([u8; 32], usize)> {
    let word = lane.word().as_bytes();
    let len = word.len() + 1 + key_bytes.len();
    if len > 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf[..word.len()].copy_from_slice(word);
    buf[word.len()] = b' ';
    buf[word.len() + 1..len].copy_from_slice(key_bytes);
    Some((buf, len))
}

/// NOTES ONE AUTHENTICATED MEMBER ANSWER into the evidence in `dir` (the place's directory,
/// [`crate::record::dir`]): `member` (a node id, a Commons identity) answered on `lane` from
/// `addr` at `at_ms`, the dial having begun at `began_ms` in a round that began on `began_on`
/// (the strong network id [`crate::facts::network_now`] read then). The answer is booked on that
/// network only where this Mac's traffic still leaves on it now ([`Answer::network`]): a move
/// under the round, or a gateway not read at either end, books it on none. `logged`: the round
/// ran under this Mac's vault's membership log ([`Answer::logged`]). Call it only for a proven
/// answer. Coalesced in-process: the same member on the same lane at most once a
/// [`COALESCE_MS`] within a stay, the first after a move at once.
pub fn note_answer(
    dir: &Path,
    (lane, member): (Lane, &str),
    addr: Option<IpAddr>,
    (began_on, logged): (Option<&NetworkId>, bool),
    (began_ms, at_ms): (u64, u64),
) {
    let key_bytes = crate::place::member_key_bytes(lane, member);
    let key_str = std::str::from_utf8(&key_bytes).unwrap_or_default();
    let (noted_buf, noted_len) = noted_key_buf(lane, &key_bytes).expect("member_key fits in 32B");
    let noted_key_str = std::str::from_utf8(&noted_buf[..noted_len]).unwrap_or_default();
    let mut stay = None;
    {
        let mut guard = NOTED.lock().unwrap_or_else(PoisonError::into_inner);
        if !due(
            guard.get_or_insert_with(Noted::default),
            dir,
            noted_key_str,
            at_ms,
            &mut stay,
        ) {
            return;
        }
    }
    let key = key_str.to_owned();
    let noted_key = noted_key_str.to_owned();
    let via = addr.and_then(crate::facts::route_via);
    let network = began_on
        .filter(|began_on| crate::facts::network_now().as_ref() == Some(*began_on))
        .cloned();
    let answer = Answer {
        lane,
        member: key,
        addr: addr.map(|addr| addr.to_canonical()),
        via,
        iface: via.and_then(crate::facts::interface_holding),
        network,
        logged,
        began_ms,
        at_ms,
    };
    flush(
        dir,
        Evidence {
            answers: vec![answer],
            ..Evidence::default()
        },
        vec![noted_key],
        at_ms,
    );
}

/// Notes a batch of authenticated answers with their original routes and clocks.
/// Callers resolve each identity through current membership and use [`member_key`].
/// Unlike [`note_answer`], this does not look up a route at collection time: each
/// answer already carries the local address of its authenticated socket. Coalesces
/// repeated members and waits for the evidence lock at most once for the batch.
pub fn note_answers(dir: &Path, answers: Vec<Answer>) {
    if answers.is_empty() {
        return;
    }
    let mut stay = None;
    let mut notes = compact(Evidence {
        answers,
        ..Evidence::default()
    });
    let mut keys = Vec::new();
    let mut now = 0;
    {
        let mut guard = NOTED.lock().unwrap_or_else(PoisonError::into_inner);
        let noted = guard.get_or_insert_with(Noted::default);
        notes.answers.retain(|answer| {
            if let Some((buf, len)) = noted_key_buf(answer.lane, answer.member.as_bytes()) {
                let key_str = std::str::from_utf8(&buf[..len]).unwrap_or_default();
                if !due(noted, dir, key_str, answer.at_ms, &mut stay) {
                    return false;
                }
                keys.push(key_str.to_owned());
            } else {
                let key = format!("{} {}", answer.lane.word(), answer.member);
                if !due(noted, dir, &key, answer.at_ms, &mut stay) {
                    return false;
                }
                keys.push(key);
            }
            now = now.max(answer.at_ms);
            true
        });
    }
    if !notes.answers.is_empty() {
        flush(dir, notes, keys, now);
    }
}

/// Notes one finished fleet round in `dir`: `answered` members answered it, and `blocked` of its
/// dials met something other than silence ([`Round::blocked`]). Note a round only where the book
/// held members to dial.
pub fn note_round(
    dir: &Path,
    lane: Lane,
    (answered, blocked): (usize, usize),
    (began_ms, at_ms): (u64, u64),
) {
    flush(
        dir,
        Evidence {
            rounds: vec![Round {
                lane,
                answered,
                blocked,
                began_ms,
                at_ms,
            }],
            ..Evidence::default()
        },
        Vec::new(),
        at_ms,
    );
}

/// Notes one measured internet answer in `dir`: `endpoint` answered (`ok`) or did not, as
/// `source` measured it at `at_ms`. Coalesced as [`note_answer`] is, per verdict: a change of
/// verdict is always noted.
pub fn note_internet(dir: &Path, ok: bool, source: InternetSource, endpoint: &str, at_ms: u64) {
    let (noted_key, other) = if ok {
        ("internet ok", "internet down")
    } else {
        ("internet down", "internet ok")
    };
    let mut stay = None;
    {
        let mut guard = NOTED.lock().unwrap_or_else(PoisonError::into_inner);
        let noted = guard.get_or_insert_with(Noted::default);
        let dir_last = noted.last.get(dir);
        let changed = dir_last
            .and_then(|by_key| by_key.get(other))
            .is_some_and(|other_at| {
                dir_last
                    .and_then(|by_key| by_key.get(noted_key))
                    .is_none_or(|this_at| other_at > this_at)
            });
        if !changed && !due(noted, dir, noted_key, at_ms, &mut stay) {
            return;
        }
    }
    flush(
        dir,
        Evidence {
            internet: vec![InternetNote {
                ok,
                source,
                at_ms,
                endpoint: endpoint_word(endpoint).into_owned(),
            }],
            ..Evidence::default()
        },
        vec![noted_key.to_owned()],
        at_ms,
    );
}

/// The home-day ledger's file name ([the module](self)).
pub const DAYS_FILE: &str = "home-days";

/// The lock every write of [`DAYS_FILE`] holds, for one bounded read-modify-rename.
pub const DAYS_LOCK: &str = "home-days.lock";

/// The ledger's day: a UTC day of this Mac's clock, the unix ms an answer came at divided by it.
pub const DAY_MS: u64 = 24 * 60 * 60_000;

/// The most networks the ledger keeps of each kind ([`compact_days`]): credited networks (the
/// seed credits at most as many as the place remembers as the ALab LAN), and apart from them the
/// networks still earning their days, the most recently heard first, so the network a note books
/// today always keeps its place and a lab that moves earns its new home's days.
pub const DAYS_NETWORKS: usize = crate::place::REMEMBERED_MAX;

/// The most days the ledger keeps for one network: the newest (the day a note is booking, so its
/// members gather), then the days most members were heard on, then the newest, so a stretch of
/// days with one member heard never pushes out a home's days.
pub const DAYS_KEPT: usize = 32;

/// The most members the ledger keeps for one day of one network, past what any rule counts.
pub const DAY_MEMBERS: usize = 8;

/// What the home-day ledger holds ([the module](self)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HomeDays {
    /// When the one-time seed ran on this Mac (unix ms); `None` before it has ([`seed_days`]).
    pub seeded_ms: Option<u64>,
    /// Each network's entry, by its id.
    pub networks: Vec<NetworkDays>,
}

/// One network's entry in the home-day ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkDays {
    /// The network (strong id).
    pub network: NetworkId,
    /// The home days the one-time seed credited it with ([`seed_days`]).
    pub credited: u32,
    /// The days members were heard on it, oldest first.
    pub days: Vec<HeardDay>,
}

/// One day on one network in the home-day ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeardDay {
    /// The day ([`DAY_MS`] since the epoch).
    pub day: u64,
    /// The distinct members heard on the network that day ([`member_key`]), sorted, at most
    /// [`DAY_MEMBERS`].
    pub members: Vec<String>,
}

impl HomeDays {
    /// `network`'s entry; `None` where the ledger holds none.
    #[must_use]
    pub fn of(&self, network: &NetworkId) -> Option<&NetworkDays> {
        self.networks.iter().find(|entry| entry.network == *network)
    }
}

impl NetworkDays {
    /// The days on which at least `members` distinct members were heard on this network.
    pub fn heard_by(&self, members: usize) -> impl Iterator<Item = u64> + '_ {
        self.days
            .iter()
            .filter(move |day| day.members.len() >= members)
            .map(|day| day.day)
    }
}

/// What [`seed_days`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seeding {
    /// This call seeded the ledger: it credited these networks.
    Now(Vec<NetworkId>),
    /// The ledger was seeded before, at this time (unix ms): this call credited nothing.
    Before(u64),
}

/// A strong network id word of the ledger; `None` for anything else.
fn strong_word(word: &str) -> Option<NetworkId> {
    NetworkId::parse(word).filter(NetworkId::is_strong)
}

/// Adds the entry `line` holds to `days`; `false` where this build cannot read it.
fn parse_days_line(line: &str, days: &mut HomeDays) -> bool {
    let mut words = [""; 5];
    let mut count = 0;
    for word in line.split(' ') {
        let Some(slot) = words.get_mut(count) else {
            return false;
        };
        *slot = word;
        count += 1;
    }
    match &words[..count] {
        ["s", at] if days.seeded_ms.is_none() => at.parse().is_ok_and(|at| {
            days.seeded_ms = Some(at);
            true
        }),
        ["c", network, credited] => {
            let (Some(network), Ok(credited)) = (strong_word(network), credited.parse::<u32>())
            else {
                return false;
            };
            days.networks.push(NetworkDays {
                network,
                credited,
                days: Vec::new(),
            });
            true
        }
        ["d", network, day, members] => {
            let (Some(network), Ok(day)) = (strong_word(network), day.parse()) else {
                return false;
            };
            let mut parsed = Vec::with_capacity(DAY_MEMBERS);
            for member in members.split(',') {
                if parsed.len() >= DAY_MEMBERS || !is_lower_hex16(member) {
                    return false;
                }
                parsed.push(member.to_owned());
            }
            days.networks.push(NetworkDays {
                network,
                credited: 0,
                days: vec![HeardDay {
                    day,
                    members: parsed,
                }],
            });
            true
        }
        _ => false,
    }
}

/// The home-day ledger a [`DAYS_FILE`] text holds, with how many of its lines it passed over:
/// each line that is not empty and this build cannot read (malformed, torn, unterminated by
/// `'\n'`, a second seed line, or of a kind this build does not write).
#[must_use]
pub fn parse_days(text: &str) -> (HomeDays, usize) {
    let mut days = HomeDays::default();
    let mut passed = 0;
    for chunk in text.split_inclusive('\n') {
        let read = chunk
            .strip_suffix('\n')
            .filter(|line| !line.ends_with('\r'))
            .is_some_and(|line| line.is_empty() || parse_days_line(line, &mut days));
        if !read && !chunk.is_empty() {
            passed += 1;
        }
    }
    (compact_days(days), passed)
}

/// `days` as one entry per network, each day once with its members sorted, within
/// [`DAYS_NETWORKS`], [`DAYS_KEPT`] and [`DAY_MEMBERS`]: of the networks the seed credited, those
/// with the most credit and days, then the most recently heard; apart from them, of the networks
/// still earning their days, the most recently heard, then those with the most days.
#[must_use]
pub fn compact_days(days: HomeDays) -> HomeDays {
    use std::cmp::Reverse;
    use std::collections::{BTreeMap, BTreeSet};
    let mut merged: BTreeMap<NetworkId, (u32, BTreeMap<u64, BTreeSet<String>>)> = BTreeMap::new();
    for entry in days.networks {
        let slot = merged.entry(entry.network).or_default();
        slot.0 = slot.0.max(entry.credited);
        for day in entry.days {
            slot.1.entry(day.day).or_default().extend(day.members);
        }
    }
    let networks: Vec<NetworkDays> = merged
        .into_iter()
        .map(|(network, (credited, heard))| {
            let mut days: Vec<HeardDay> = heard
                .into_iter()
                .map(|(day, members)| HeardDay {
                    day,
                    members: members.into_iter().take(DAY_MEMBERS).collect(),
                })
                .collect();
            if days.len() > DAYS_KEPT {
                let newest = days.last().map(|day| day.day);
                days.sort_unstable_by_key(|day| {
                    (
                        Reverse(Some(day.day) == newest),
                        Reverse(day.members.len()),
                        Reverse(day.day),
                    )
                });
                days.truncate(DAYS_KEPT);
                days.sort_unstable_by_key(|day| day.day);
            }
            NetworkDays {
                network,
                credited,
                days,
            }
        })
        .collect();
    let (mut credited, mut earning): (Vec<NetworkDays>, Vec<NetworkDays>) =
        networks.into_iter().partition(|entry| entry.credited > 0);
    let heard = |entry: &NetworkDays| {
        u64::from(entry.credited)
            .saturating_add(u64::try_from(entry.days.len()).unwrap_or(u64::MAX))
    };
    if credited.len() > DAYS_NETWORKS {
        credited.sort_by_key(|entry| {
            (
                Reverse(heard(entry)),
                Reverse(entry.days.last().map(|day| day.day)),
            )
        });
        credited.truncate(DAYS_NETWORKS);
    }
    if earning.len() > DAYS_NETWORKS {
        earning.sort_by_key(|entry| {
            (
                Reverse(entry.days.last().map(|day| day.day)),
                Reverse(heard(entry)),
            )
        });
        earning.truncate(DAYS_NETWORKS);
    }
    let mut networks = credited;
    networks.append(&mut earning);
    networks.sort_unstable_by(|a, b| a.network.cmp(&b.network));
    HomeDays {
        seeded_ms: days.seeded_ms,
        networks,
    }
}

/// The [`DAYS_FILE`] text of `days`: the seed's line, then each network's credit and days.
#[must_use]
pub fn format_days(days: &HomeDays) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(
        days.networks
            .iter()
            .map(|entry| 32 + entry.days.len().saturating_mul(48))
            .sum::<usize>()
            .saturating_add(32),
    );
    if let Some(at) = days.seeded_ms {
        let _ = writeln!(text, "s {at}");
    }
    for entry in &days.networks {
        let network = entry.network.as_str();
        if entry.credited > 0 {
            let _ = writeln!(text, "c {network} {}", entry.credited);
        }
        for day in entry.days.iter().filter(|day| !day.members.is_empty()) {
            let _ = write!(text, "d {network} {} ", day.day);
            for (idx, member) in day.members.iter().enumerate() {
                if idx > 0 {
                    text.push(',');
                }
                text.push_str(member);
            }
            text.push('\n');
        }
    }
    text
}

/// The home-day ledger in `dir`, with how many of its lines this build passed over
/// ([`parse_days`]): `(empty, 0)` where there is none, `(empty, 1)` where it is there and cannot
/// be read (not private, not UTF-8), so a caller that must not take damage for silence can tell.
#[must_use]
pub fn read_days(dir: &Path) -> (HomeDays, usize) {
    let read = aterm_uds::private_file::JudgedDir::judge(dir)
        .and_then(|judged| judged.read(DAYS_FILE, MAX_BYTES));
    match read {
        Ok(bytes) => String::from_utf8(bytes)
            .map_or_else(|_| (HomeDays::default(), 1), |text| parse_days(&text)),
        Err(aterm_uds::private_file::PrivateFileError::Missing) => (HomeDays::default(), 0),
        Err(_) => (HomeDays::default(), refused(dir, DAYS_FILE)),
    }
}

/// Changes the home-day ledger in `dir` under [`DAYS_LOCK`]: `change` is handed it as read, and
/// how many lines were passed over, and says whether to write it back (whole, by rename, bounded
/// by [`compact_days`]); `true` where it was written.
fn change_days(
    dir: &Path,
    change: impl FnOnce(&mut HomeDays, usize) -> io::Result<bool>,
) -> io::Result<bool> {
    aterm_uds::private_file::ensure_private_dir(dir)?;
    let _lock = aterm_uds::private_file::PrivateLock::take(
        dir,
        DAYS_LOCK,
        LOCK_WAIT,
        "the home-day ledger is being written by another process",
    )?;
    let (mut days, passed) = read_days(dir);
    if !change(&mut days, passed)? {
        return Ok(false);
    }
    aterm_uds::private_file::write_private(
        dir,
        DAYS_FILE,
        format_days(&compact_days(days)).as_bytes(),
    )?;
    Ok(true)
}

/// One member heard on one network on one day: what [`note_days`] books.
type Booking = (NetworkId, u64, String);

/// The bookings this process wrote, per directory: [`note_days`] writes each once.
static BOOKED: Mutex<Option<HashMap<PathBuf, HashSet<Booking>>>> = Mutex::new(None);

/// BOOKS THE DAYS `answers` WERE HEARD ON into the home-day ledger in `dir` (the place's directory,
/// [`crate::record::dir`]): each logged fleet answer booked on a strong network
/// ([`Answer::network`], [`Answer::logged`]) adds its member to the day it came on
/// ([`DAY_MS`]), on that network. Any other answer books nothing. Call it only for node-proven
/// answers, beside noting them ([`note_answers`]). A booking this process wrote already is not
/// written again; one a held lock kept back is written by the next note of it. A note that
/// writes a ledger it read in part counts the seed as run where it read no mark of it
/// ([`seed_days`]).
pub fn note_days(dir: &Path, answers: &[Answer]) {
    let mut wanted: Vec<Booking> = {
        let guard = BOOKED.lock().unwrap_or_else(PoisonError::into_inner);
        let set = guard.as_ref().and_then(|booked| booked.get(dir));
        answers
            .iter()
            .filter(|answer| answer.lane == Lane::Fleet && answer.logged)
            .filter_map(|answer| {
                let network = answer.network.as_ref().filter(|n| n.is_strong())?;
                let day = answer.at_ms / DAY_MS;
                if set.is_some_and(|s| {
                    s.iter()
                        .any(|(n, d, m)| n == network && *d == day && m == &answer.member)
                }) {
                    return None;
                }
                Some((network.clone(), day, answer.member.clone()))
            })
            .collect()
    };
    wanted.sort_unstable();
    wanted.dedup();
    if wanted.is_empty() {
        return;
    }
    let at_ms = answers.iter().map(|answer| answer.at_ms).max().unwrap_or(0);
    let written = change_days(dir, |days, passed| {
        // A ledger read in part may have lost its seed's mark: the seed counts as run, so a
        // damaged ledger never lets it credit a network that did not earn its days.
        if passed > 0 && days.seeded_ms.is_none() {
            days.seeded_ms = Some(at_ms);
        }
        for (network, day, member) in &wanted {
            days.networks.push(NetworkDays {
                network: network.clone(),
                credited: 0,
                days: vec![HeardDay {
                    day: *day,
                    members: vec![member.clone()],
                }],
            });
        }
        Ok(true)
    });
    match written {
        Ok(_) => {
            let mut guard = BOOKED.lock().unwrap_or_else(PoisonError::into_inner);
            let booked = guard.get_or_insert_with(HashMap::new);
            let newest = wanted.iter().map(|(_, day, _)| *day).max().unwrap_or(0);
            // Only today's and yesterday's bookings can come again; older ones are let go.
            booked.retain(|_, set| {
                set.retain(|(_, day, _)| day.saturating_add(1) >= newest);
                !set.is_empty()
            });
            if let Some(set) = booked.get_mut(dir) {
                set.extend(wanted);
            } else {
                booked.insert(dir.to_path_buf(), wanted.into_iter().collect());
            }
        }
        Err(e) => aterm_log::debug!("network place: a home-day booking waits for the next ({e})"),
    }
}

/// THE ONE-TIME SEED of the home-day ledger in `dir` (`docs/DESIGN-alab-one-mesh-2026-11-05.md`
/// §4.2 M2, §11 B6): the first call credits each network in `remembered` (the place's networks
/// remembered as the ALab LAN, [`crate::place::Record::remembered`]) with `credit` home days
/// and marks the ledger seeded at `at_ms`; every later call credits nothing, whatever it is
/// handed ([`Seeding::Before`]). So a network this Mac already called home stays home across the
/// first start of a build that counts home days, and one first remembered after that earns its
/// days.
///
/// # Errors
/// The directory could not be made private, the lock was held past [`LOCK_WAIT`], the ledger
/// could not be written, or it holds a line this build cannot read, so it cannot tell whether the
/// seed ran ([`io::ErrorKind::InvalidData`]): it seeds nothing, and the note that next writes the
/// ledger whole counts the seed as run where no mark of it was read ([`note_days`]), so damage
/// never credits a network that did not earn its days.
pub fn seed_days(dir: &Path, remembered: &[Known], credit: u32, at_ms: u64) -> io::Result<Seeding> {
    let mut seeding = None;
    change_days(dir, |days, passed| {
        if passed > 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the home-day ledger holds a line this build cannot read, so it cannot tell \
                 whether the seed ran",
            ));
        }
        if let Some(seeded_ms) = days.seeded_ms {
            seeding = Some(Seeding::Before(seeded_ms));
            return Ok(false);
        }
        let mut credited: Vec<NetworkId> = remembered
            .iter()
            .filter(|known| known.network.is_strong())
            .map(|known| known.network.clone())
            .collect();
        credited.sort_unstable();
        credited.dedup();
        days.networks
            .extend(credited.iter().map(|network| NetworkDays {
                network: network.clone(),
                credited: credit,
                days: Vec::new(),
            }));
        days.seeded_ms = Some(at_ms);
        seeding = Some(Seeding::Now(credited));
        Ok(true)
    })?;
    seeding.ok_or_else(|| io::Error::other("the home-day seed decided nothing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_observations_do_not_replace_or_delay_admission_evidence() {
        let root = aterm_tempfile::tempdir().expect("scratch state");
        let dir = root.path();
        let first = answer("m1", 1_000);
        add(
            dir,
            Evidence {
                answers: vec![first.clone()],
                ..Evidence::default()
            },
        )
        .unwrap();
        let scan = answer("m1", 2_000);
        note_observations(dir, vec![scan]);
        assert_eq!(read_counted(dir).0.answers, [first]);
        let displayed = read_for_place(dir);
        assert_eq!(displayed.answers.len(), 1);
        assert_eq!(displayed.answers[0].at_ms, 2_000);
        assert!(!displayed.answers[0].logged);
        assert_eq!(displayed.answers[0].network, None);

        let heartbeat = answer("m1", 2_001);
        note_answers(dir, vec![heartbeat.clone()]);
        assert_eq!(read_counted(dir).0.answers, [heartbeat]);
        assert_eq!(read_for_place(dir).answers[0].at_ms, 2_001);
    }

    fn answer(member: &str, at_ms: u64) -> Answer {
        Answer {
            lane: Lane::Fleet,
            member: member_key(Lane::Fleet, member),
            addr: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 4, 7))),
            via: Some(Ipv4Addr::new(192, 168, 4, 31)),
            iface: Some("en0".to_owned()),
            network: NetworkId::parse("n00112233445566ff"),
            logged: true,
            began_ms: at_ms - 100,
            at_ms,
        }
    }

    /// The evidence file is bounded and keeps the newest answer per member and lane: many
    /// answers from one member are one line, the newest, and a flood of members stops at the
    /// line bound with the newest kept.
    /// NEGATIVE CONTROL: dropping the per-member `retain` in `compact` keeps every answer of
    /// `m0`, and this test fails.
    #[test]
    fn the_evidence_file_is_bounded_and_keeps_the_newest_answer_per_member() {
        let mut evidence = Evidence::default();
        for k in 0..10 {
            evidence.answers.push(answer("m0", 1_000 + k));
        }
        let kept = compact(evidence.clone());
        assert_eq!(kept.answers.len(), 1);
        assert_eq!(kept.answers[0].at_ms, 1_009);
        for k in 0..2 * MAX_LINES as u64 {
            evidence.answers.push(answer(&format!("x{k}"), 10_000 + k));
        }
        let kept = compact(evidence);
        assert_eq!(kept.answers.len(), MAX_LINES);
        assert!(
            kept.answers
                .iter()
                .all(|a| a.at_ms >= 10_000 + MAX_LINES as u64)
        );
        assert_eq!(parse(&format(&kept)), {
            let mut sorted = kept.clone();
            sorted.answers.sort_by_key(|a| a.at_ms);
            sorted
        });
    }

    /// An answer line keeps the interface its route took, the network it was heard on (`-`
    /// where unread) and whether its round ran under the vault's membership log, and a line whose
    /// interface word is no interface name, whose network word is no network id, whose log word
    /// is neither, or one of an earlier build's shape (no interface, no network, no log word), is
    /// no answer: a member counts only with the interface and network it came over.
    /// NEGATIVE CONTROLS: `answer_line` leaving the interface word out writes a line `parse`
    /// passes over, and "the interface, network and log read back" fails; leaving the network
    /// word out fails it too; writing `-` for every log word reads the logged answer back
    /// unlogged and fails it.
    #[test]
    fn an_answers_interface_network_and_log_read_back() {
        let unread = Answer {
            iface: None,
            network: None,
            logged: false,
            ..answer("m1", 2_000)
        };
        let evidence = Evidence {
            answers: vec![answer("m0", 1_000), unread.clone()],
            ..Evidence::default()
        };
        assert_eq!(
            parse(&format(&evidence)).answers,
            evidence.answers,
            "the interface, network and log read back"
        );
        let member = member_key(Lane::Fleet, "m2");
        for line in [
            format!("a fleet {member} 192.168.4.7 192.168.4.31 en/0 - log 900 1000\n"),
            format!("a fleet {member} 192.168.4.7 192.168.4.31 en0 home log 900 1000\n"),
            format!("a fleet {member} 192.168.4.7 192.168.4.31 en0 - yes 900 1000\n"),
            format!("a fleet {member} 192.168.4.7 192.168.4.31 en0 - 900 1000\n"),
            format!("a fleet {member} 192.168.4.7 192.168.4.31 en0 900 1000\n"),
            format!("a fleet {member} 192.168.4.7 192.168.4.31 900 1000\n"),
        ] {
            assert!(parse(&line).answers.is_empty(), "{line}");
        }
    }

    /// A noted answer is booked on the network its round began on only where this Mac's traffic
    /// still leaves on that network when it is noted: a round that began on another network (this
    /// Mac moved under it), one that began before the gateway was read, and a note made while
    /// the gateway is unread book it on none.
    /// NEGATIVE CONTROL: `note_answer` booking the answer on `began_on` as given (no comparison
    /// with `network_now`) books the round begun on the other network there, and "a round begun
    /// elsewhere is booked on none" fails.
    #[test]
    fn an_answer_is_booked_on_the_network_its_round_began_and_ended_on() {
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = crate::record::dir(root.path());
        let route = |hw: Option<Vec<u8>>| crate::place::RouteFacts {
            interface: "en0".to_owned(),
            address: Ipv4Addr::new(192, 168, 4, 31),
            prefix: 22,
            gateway: Some(Ipv4Addr::new(192, 168, 4, 1)),
            gateway_hw: hw,
        };
        let here = route(Some(vec![0, 1, 2, 3, 4, 5]));
        let there = route(Some(vec![0, 1, 2, 3, 4, 9]));
        let (here_id, there_id) = (NetworkId::of(&here), NetworkId::of(&there));
        let script = |route: crate::place::RouteFacts| crate::facts::Script {
            facts: crate::place::Facts {
                route: Some(route),
                ..crate::place::Facts::default()
            },
            via: Vec::new(),
        };
        let noted =
            |member: &str, facts: crate::place::RouteFacts, began_on: Option<&NetworkId>| {
                crate::facts::script_here(script(facts), || {
                    note_answer(
                        &dir,
                        (Lane::Fleet, member),
                        Some(IpAddr::V4(Ipv4Addr::new(192, 168, 4, 9))),
                        (began_on, true),
                        (1_000, 1_500),
                    );
                });
                read(&dir)
                    .answers
                    .into_iter()
                    .find(|answer| answer.member == member_key(Lane::Fleet, member))
                    .expect("noted")
                    .network
            };
        assert_eq!(noted("m0", here.clone(), here_id.as_ref()), here_id, "here");
        assert_eq!(
            noted("m1", here.clone(), there_id.as_ref()),
            None,
            "a round begun elsewhere is booked on none"
        );
        assert_eq!(noted("m2", here, None), None, "a round begun unread");
        assert_eq!(
            noted("m3", route(None), here_id.as_ref()),
            None,
            "a note made unread"
        );
    }

    /// Every process can add a note while the keeper runs: the lock is held only for one
    /// read-modify-rename, two writers in turn both land, and a writer that finds the lock held
    /// past its wait keeps its note and lands it with its next.
    /// NEGATIVE CONTROL: guarding `add` with `keeper.lock` (which a keeper holds for its whole
    /// life) in place of `heard.lock` makes the first add fail while the keeper's lock is held,
    /// and this test fails.
    #[test]
    fn every_process_can_flush_a_note_while_a_keeper_holds_its_lock() {
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = crate::record::dir(root.path());
        aterm_uds::private_file::ensure_private_dir(&dir).expect("the dir");
        let _keeper = aterm_uds::private_file::PrivateLock::take(
            &dir,
            crate::keeper::LOCK,
            Duration::ZERO,
            "held",
        )
        .expect("the keeper's lock");
        add(
            &dir,
            Evidence {
                answers: vec![answer("m1", 1_000)],
                ..Evidence::default()
            },
        )
        .expect("a note while the keeper runs");
        add(
            &dir,
            Evidence {
                answers: vec![answer("m2", 2_000)],
                ..Evidence::default()
            },
        )
        .expect("a second writer");
        assert_eq!(read(&dir).answers.len(), 2);
        let held = aterm_uds::private_file::PrivateLock::take(&dir, LOCK, Duration::ZERO, "held")
            .expect("the evidence lock");
        flush(
            &dir,
            Evidence {
                answers: vec![answer("m3", 3_000)],
                ..Evidence::default()
            },
            Vec::new(),
            3_000,
        );
        assert_eq!(read(&dir).answers.len(), 2, "held back, not lost");
        drop(held);
        flush(
            &dir,
            Evidence {
                answers: vec![answer("m4", 4_000)],
                ..Evidence::default()
            },
            Vec::new(),
            4_000,
        );
        assert_eq!(
            read(&dir).answers.len(),
            4,
            "the held note landed with the next"
        );
    }

    /// One process notes a member once a minute within a stay, but the first answer after a
    /// move at once, whatever came a moment before it.
    /// NEGATIVE CONTROL: dropping the stay test from `due` holds the answer right after the move
    /// back for the minute, and this test fails.
    #[test]
    fn the_first_answer_after_a_move_is_noted_at_once_and_later_ones_coalesce() {
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = crate::record::dir(root.path());
        let addr = Some(IpAddr::V4(Ipv4Addr::new(10, 9, 8, 7)));
        let t = 1_800_000_000_000;
        note_answer(&dir, (Lane::Commons, "peer-a"), addr, (None, false), (t, t));
        note_answer(
            &dir,
            (Lane::Commons, "peer-a"),
            addr,
            (None, false),
            (t + 1_000, t + 1_000),
        );
        let lines = |dir: &Path| {
            read(dir)
                .answers
                .iter()
                .map(|a| a.at_ms)
                .collect::<Vec<_>>()
        };
        assert_eq!(lines(&dir), [t], "the second within the minute coalesced");
        let moved = crate::record::Written {
            record: crate::place::Record {
                stays: vec![crate::place::Stay {
                    start_ms: t + 2_000,
                    end_ms: None,
                    at: crate::place::StayAt::Checking,
                    network: None,
                    subnet: None,
                    gateway: None,
                    from_start: false,
                }],
                ..crate::place::Record::default()
            },
            ..crate::record::Written::default()
        };
        crate::record::write(&dir, &moved).expect("the record");
        note_answer(
            &dir,
            (Lane::Commons, "peer-a"),
            addr,
            (None, false),
            (t + 3_000, t + 3_000),
        );
        assert_eq!(
            lines(&dir),
            [t + 3_000],
            "the first after the move, at once"
        );
        note_answer(
            &dir,
            (Lane::Commons, "peer-a"),
            addr,
            (None, false),
            (t + 70_000, t + 70_000),
        );
        assert_eq!(lines(&dir), [t + 70_000], "a minute on");
    }

    /// A round's blocked count is written and read back, and a round line with none (the first
    /// build's) reads as none blocked, so an older file never reads a refused round as silence
    /// it did not record.
    /// NEGATIVE CONTROL: writing the round line without its blocked count reads the refused
    /// round back as silence, and "the blocked count reads back" fails.
    #[test]
    fn a_rounds_blocked_count_reads_back() {
        let round = Round {
            lane: Lane::Fleet,
            answered: 0,
            blocked: 2,
            began_ms: 1_000,
            at_ms: 2_000,
        };
        let evidence = Evidence {
            rounds: vec![round.clone()],
            ..Evidence::default()
        };
        assert_eq!(
            parse(&format(&evidence)).rounds,
            [round],
            "the blocked count reads back"
        );
        assert_eq!(parse("r fleet 0 1000 2000\n").rounds[0].blocked, 0);
        assert!(parse("r fleet 0 1000 2000 x\n").rounds.is_empty());
    }

    /// The fleet book snapshot reads back, member keys hashed, with addresses and answer times,
    /// each answer with the network it was booked on: a new answer takes the network this Mac is
    /// on as it writes, and one the snapshot held keeps its own across a write on another
    /// network.
    /// NEGATIVE CONTROL: stamping every answer with the network of the write (`booked` always
    /// `answered_ms.and(network.cloned())`) moves node-a's home answer onto the hotel, and "an
    /// answer keeps the network it came on" fails.
    #[test]
    fn the_book_snapshot_reads_back() {
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = crate::record::dir(root.path());
        assert_eq!(read_book(&dir), None);
        let addr = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 4, 9)));
        let home = NetworkId::parse("n00112233445566aa").expect("an id");
        let hotel = NetworkId::parse("n66778899aabbccdd").expect("an id");
        write_book(
            &dir,
            [
                ("node-a".to_owned(), addr, Some(5)),
                ("node-b".to_owned(), None, None),
            ],
            Some(&home),
        )
        .expect("written");
        assert_eq!(
            read_book(&dir).expect("a book").members[0].network,
            Some(home.clone())
        );
        write_book(
            &dir,
            [
                ("node-a".to_owned(), addr, Some(5)),
                ("node-c".to_owned(), addr, Some(9)),
                ("node-b".to_owned(), None, None),
            ],
            Some(&hotel),
        )
        .expect("written");
        let networks: Vec<Option<NetworkId>> = read_book(&dir)
            .expect("a book")
            .members
            .into_iter()
            .map(|row| row.network)
            .collect();
        assert_eq!(
            networks,
            [Some(home), Some(hotel), None],
            "an answer keeps the network it came on"
        );
        write_book(
            &dir,
            [
                ("node-a".to_owned(), addr, Some(5)),
                ("node-b".to_owned(), None, None),
            ],
            None,
        )
        .expect("written");
        let book = read_book(&dir).expect("a book");
        assert_eq!(book.members.len(), 2);
        assert_eq!(book.members[0].member, member_key(Lane::Fleet, "node-a"));
        assert_eq!(
            (book.members[0].addr, book.members[0].answered_ms),
            (addr, Some(5))
        );
        assert_eq!(
            (book.members[1].addr, book.members[1].answered_ms),
            (None, None)
        );
    }

    /// Torn internet lines (empty, control-character, or oversize endpoints) count as unread in
    /// `parse_counted`, and `parse_book` ignores rows whose member word is not a 16-hex-digit hash.
    /// NEGATIVE CONTROL: accepting `*endpoint` unchecked parses `"i ok api-probe 1000 "` with
    /// `passed == 0`, and skipping the 16-hex check in `parse_book` inflates `book.members`.
    #[test]
    fn torn_internet_endpoints_and_malformed_book_members_are_passed_over() {
        let overlong = "x".repeat(MAX_ENDPOINT + 1);
        for bad in [
            "i ok api-probe 1000 \n".to_owned(),
            "i ok api-probe 1000 host\x07\n".to_owned(),
            format!("i ok api-probe 1000 {overlong}\n"),
        ] {
            let (ev, passed) = parse_counted(&bad);
            assert!(ev.internet.is_empty(), "{bad:?}");
            assert_eq!(passed, 1, "{bad:?}");
        }
        let valid_member = member_key(Lane::Fleet, "node-a");
        let book = parse_book(&format!(
            "m {valid_member} - - -\n\
             m  - - -\n\
             m nothex0123456789 - - -\n\
             m abcd - - -\n"
        ));
        assert_eq!(book.members.len(), 1);
        assert_eq!(book.members[0].member, valid_member);
    }

    /// Unterminated trailing lines, uppercase hex member keys, and unreadable/loose evidence files
    /// count as unread in `parse_counted` / `read_counted` so `proof::read_with` refuses to treat
    /// partial or untrusted files as clean silence.
    /// NEGATIVE CONTROL: iterating `text.lines()` in `parse_counted` accepts a truncated trailing
    /// line without `'\n'`, `is_ascii_hexdigit` admits uppercase member keys, and `unwrap_or_default`
    /// in `read_counted` returns `passed == 0` when `heard` has mode `0o644` or non-UTF-8 bytes.
    #[test]
    fn unterminated_lines_uppercase_members_and_unreadable_ledgers_count_as_unread() {
        let valid_member = member_key(Lane::Fleet, "node-a");
        let upper_member = valid_member.to_ascii_uppercase();
        let truncated = format!("a fleet {valid_member} 192.168.4.7 192.168.4.31 en0 - log 900 10");
        let (ev_trunc, passed_trunc) = parse_counted(&truncated);
        assert!(ev_trunc.answers.is_empty());
        assert_eq!(passed_trunc, 1);

        let upper_line =
            format!("a fleet {upper_member} 192.168.4.7 192.168.4.31 en0 - log 900 1000\n");
        let (ev_upper, passed_upper) = parse_counted(&upper_line);
        assert!(ev_upper.answers.is_empty());
        assert_eq!(passed_upper, 1);
        assert!(
            parse_book(&format!("m {upper_member} - - -\n"))
                .members
                .is_empty()
        );

        let root = aterm_tempfile::tempdir().expect("scratch state");
        let dir = crate::record::dir(root.path());
        assert_eq!(read_counted(&dir), (Evidence::default(), 0));
        aterm_uds::private_file::ensure_private_dir(&dir).expect("private dir");
        let file = dir.join(FILE);
        std::fs::write(&file, [0xff, 0xfe, b'\n']).expect("write non-utf8");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(read_counted(&dir), (Evidence::default(), 1));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::write(&file, "r fleet 0 1000 2000 0\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(read_counted(&dir), (Evidence::default(), 1));
        }
    }

    /// A strong network id of the home-day tests, `n` its last hex digit.
    fn net(n: u8) -> NetworkId {
        NetworkId::parse(&format!("n00112233445566{n:02x}")).expect("a strong id")
    }

    /// `member`'s answer on network `on` (strong, or `None`) at `at_ms`, logged or not.
    fn heard_on(member: &str, on: Option<NetworkId>, at_ms: u64, logged: bool) -> Answer {
        Answer {
            network: on,
            logged,
            ..answer(member, at_ms)
        }
    }

    const DAY0: u64 = 20_400 * DAY_MS;

    /// The ledger books one day per member per network per UTC day: only a logged fleet answer
    /// booked on a strong network books one (an unlogged one, a Commons contact, one booked on no
    /// network or a weak one books none), two answers of a member on one day book that day once,
    /// and the next day is another day. What it wrote reads back whole.
    /// NEGATIVE CONTROL: dropping `answer.logged` from `note_days`' filter books the unlogged
    /// answer's member, and "an unlogged answer books nothing" fails.
    #[test]
    fn only_logged_fleet_answers_on_a_strong_network_book_their_day() {
        let root = aterm_tempfile::tempdir().expect("scratch state");
        let dir = crate::record::dir(root.path());
        let home = net(1);
        note_days(
            &dir,
            &[
                heard_on("m1", Some(home.clone()), DAY0 + 1_000, true),
                heard_on("m1", Some(home.clone()), DAY0 + 60_000, true),
                heard_on("m2", Some(home.clone()), DAY0 + 2_000, true),
            ],
        );
        note_days(
            &dir,
            &[
                heard_on("m3", Some(home.clone()), DAY0 + 3_000, false),
                Answer {
                    lane: Lane::Commons,
                    ..heard_on("m4", Some(home.clone()), DAY0 + 4_000, true)
                },
                heard_on("m5", None, DAY0 + 5_000, true),
                heard_on(
                    "m6",
                    NetworkId::parse("w00112233445566ff"),
                    DAY0 + 6_000,
                    true,
                ),
            ],
        );
        let (days, passed) = read_days(&dir);
        assert_eq!(passed, 0);
        let entry = days.of(&home).expect("the home network's entry");
        assert_eq!(entry.days.len(), 1, "one day: {entry:?}");
        let mut members = vec![member_key(Lane::Fleet, "m1"), member_key(Lane::Fleet, "m2")];
        members.sort();
        assert_eq!(
            entry.days[0].members, members,
            "an unlogged answer books nothing, nor a Commons contact"
        );
        assert_eq!(days.networks.len(), 1, "no weak or missing network books");

        note_days(
            &dir,
            &[heard_on("m1", Some(home.clone()), DAY0 + DAY_MS, true)],
        );
        let (days, _) = read_days(&dir);
        let entry = days.of(&home).expect("the home network's entry");
        assert_eq!(
            entry.days.iter().map(|day| day.day).collect::<Vec<_>>(),
            [DAY0 / DAY_MS, DAY0 / DAY_MS + 1]
        );
        assert_eq!(entry.heard_by(2).collect::<Vec<_>>(), [DAY0 / DAY_MS]);
        assert_eq!(entry.heard_by(1).count(), 2);
        assert_eq!(
            parse_days(&format_days(&days)),
            (days, 0),
            "reads back whole"
        );
    }

    /// THE SEED IS ONCE: the first seed credits each strong network remembered as the ALab LAN
    /// (a weak one never is) and marks the ledger seeded; a second, handed a network remembered
    /// since, credits nothing and leaves the first credit as it was. Days booked before the seed
    /// stay beside its credit.
    /// NEGATIVE CONTROL: dropping `seed_days`' `seeded_ms` check credits the network remembered
    /// since on the second start, and "a second start credits nothing" fails.
    #[test]
    fn the_seed_credits_each_remembered_network_once() {
        let root = aterm_tempfile::tempdir().expect("scratch state");
        let dir = crate::record::dir(root.path());
        let known = |network: NetworkId| Known {
            network,
            learned_ms: DAY0 - 90 * DAY_MS,
            confirmed_ms: DAY0 - DAY_MS,
        };
        note_days(&dir, &[heard_on("m1", Some(net(1)), DAY0 - DAY_MS, true)]);
        let first = seed_days(
            &dir,
            &[
                known(net(1)),
                known(NetworkId::parse("w00112233445566ff").expect("a weak id")),
            ],
            7,
            DAY0,
        )
        .expect("the first seed");
        assert_eq!(first, Seeding::Now(vec![net(1)]));
        let (days, _) = read_days(&dir);
        assert_eq!(days.seeded_ms, Some(DAY0));
        let home = days.of(&net(1)).expect("the remembered network");
        assert_eq!((home.credited, home.days.len()), (7, 1));

        let second = seed_days(&dir, &[known(net(1)), known(net(2))], 7, DAY0 + DAY_MS)
            .expect("the second seed");
        assert_eq!(
            second,
            Seeding::Before(DAY0),
            "a second start credits nothing"
        );
        let (after, _) = read_days(&dir);
        assert_eq!(after, days);
        assert!(after.of(&net(2)).is_none());
    }

    /// A ledger with a line this build cannot read is read as passed over, and the seed, which
    /// cannot tell from it whether it ran, credits nothing; the note that next writes the ledger
    /// whole, reading no mark of the seed, counts it as run, so the damage credits no network.
    ///
    /// NEGATIVE CONTROL: `seed_days` ignoring `passed` seeds the torn ledger, and "a torn ledger
    /// seeds nothing" fails; `note_days` leaving `seeded_ms` unset lets the next seed credit
    /// `net(1)`, and "the damage reopens no seed" fails.
    #[test]
    fn a_torn_ledger_never_reopens_the_seed() {
        let root = aterm_tempfile::tempdir().expect("scratch state");
        let dir = crate::record::dir(root.path());
        aterm_uds::private_file::ensure_private_dir(&dir).expect("private dir");
        let line = format!(
            "d {} 3 {}\n",
            net(1).as_str(),
            member_key(Lane::Fleet, "m1")
        );
        aterm_uds::private_file::write_private(
            &dir,
            DAYS_FILE,
            format!("{line}{}", &line[..line.len() / 2]).as_bytes(),
        )
        .expect("a torn ledger");
        assert_eq!(read_days(&dir).1, 1);
        let known = Known {
            network: net(1),
            learned_ms: 0,
            confirmed_ms: 0,
        };
        let refused = seed_days(&dir, std::slice::from_ref(&known), 7, DAY0)
            .expect_err("a torn ledger seeds nothing");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
        assert_eq!(read_days(&dir).0.seeded_ms, None);
        note_days(&dir, &[heard_on("m2", Some(net(1)), DAY0 + 5, true)]);
        let (whole, passed) = read_days(&dir);
        assert_eq!(passed, 0, "a note writes it whole");
        assert_eq!(whole.seeded_ms, Some(DAY0 + 5));
        assert_eq!(
            whole.of(&net(1)).map(|entry| entry.days.len()),
            Some(2),
            "the day it read is kept"
        );
        assert_eq!(
            seed_days(&dir, &[known], 7, DAY0 + 6).expect("the seed"),
            Seeding::Before(DAY0 + 5),
            "the damage reopens no seed"
        );
        assert_eq!(
            read_days(&dir).0.of(&net(1)).map(|entry| entry.credited),
            Some(0)
        );
    }

    /// Every bound holds as the ledger is written, and none starves what the condition counts: a
    /// day keeps at most [`DAY_MEMBERS`] members; a network keeps its newest day (the one a note
    /// is booking), then the days most members were heard on; the ledger keeps its credited
    /// networks (most credit and days first) and apart from them the networks still earning
    /// their days, most recently heard first, so a new home earns its days among old networks.
    ///
    /// NEGATIVE CONTROL: `compact_days` with the newest-day key dropped loses day 35, and "the
    /// newest day is kept" fails; ranking the earning networks by days first drops `net(50)`, and
    /// "a new network keeps its place" fails; one ranking for both kinds (credited first) drops
    /// `net(50)` too; the credited ranking by id alone drops `net(116)`, and "the most credited"
    /// fails.
    #[test]
    fn every_bound_holds_and_none_starves_a_new_home() {
        let days_up_to = |last: u64| -> Vec<HeardDay> {
            (0..=last)
                .map(|day| HeardDay {
                    day,
                    members: (0..if day >= 32 { 1 } else { DAY_MEMBERS + 2 })
                        .map(|m| member_key(Lane::Fleet, &format!("m{m}")))
                        .collect(),
                })
                .collect()
        };
        let bound = u8::try_from(DAYS_NETWORKS).expect("a small bound");
        let mut networks: Vec<NetworkDays> = (0..bound)
            .map(|n| NetworkDays {
                network: net(n),
                credited: 0,
                days: days_up_to(35),
            })
            .collect();
        // A network first heard after all of them, on one day.
        networks.push(NetworkDays {
            network: net(50),
            credited: 0,
            days: vec![HeardDay {
                day: 40,
                members: vec![member_key(Lane::Fleet, "m0")],
            }],
        });
        // One more credited network than the bound, each with as many days as its place.
        networks.extend((0..=bound).map(|n| {
            NetworkDays {
                network: net(100 + n),
                credited: 7,
                days: (0..u64::from(n))
                    .map(|day| HeardDay {
                        day,
                        members: vec![member_key(Lane::Fleet, "m0")],
                    })
                    .collect(),
            }
        }));
        let bounded = compact_days(HomeDays {
            seeded_ms: Some(DAY0),
            networks,
        });
        let (credited, earning): (Vec<&NetworkDays>, Vec<&NetworkDays>) = bounded
            .networks
            .iter()
            .partition(|entry| entry.credited > 0);
        assert_eq!(
            (credited.len(), earning.len()),
            (DAYS_NETWORKS, DAYS_NETWORKS)
        );
        assert!(
            bounded.of(&net(50)).is_some(),
            "a new network keeps its place"
        );
        assert!(bounded.of(&net(100 + bound)).is_some(), "the most credited");
        assert!(bounded.of(&net(100)).is_none(), "the least credited goes");
        for entry in earning.iter().filter(|entry| entry.network != net(50)) {
            let kept: Vec<u64> = entry.days.iter().map(|day| day.day).collect();
            assert_eq!(kept.len(), DAYS_KEPT);
            assert_eq!(kept.last(), Some(&35), "the newest day is kept");
            assert_eq!(kept[0], 1, "then the days most members were heard on");
            assert!(
                !kept.contains(&32) && !kept.contains(&34),
                "the days one member was heard on go first"
            );
            assert!(
                entry.days[..DAYS_KEPT - 1]
                    .iter()
                    .all(|day| day.members.len() == DAY_MEMBERS)
            );
        }
        assert_eq!(parse_days(&format_days(&bounded)), (bounded, 0));

        // Through the ledger: a network with 32 days of two members gathers a new day's three,
        // one note at a time.
        let root = aterm_tempfile::tempdir().expect("scratch state");
        let dir = crate::record::dir(root.path());
        for day in 0..32u64 {
            for member in ["m1", "m2"] {
                note_days(
                    &dir,
                    &[heard_on(member, Some(net(7)), DAY0 + day * DAY_MS, true)],
                );
            }
        }
        for member in ["m1", "m2", "m3"] {
            note_days(
                &dir,
                &[heard_on(member, Some(net(7)), DAY0 + 40 * DAY_MS, true)],
            );
        }
        let (days, _) = read_days(&dir);
        assert!(
            days.of(&net(7))
                .is_some_and(|entry| entry.heard_by(3).any(|day| day == DAY0 / DAY_MS + 40)),
            "the newest day gathers its members"
        );
    }
}
