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
    parse_projected::<true>(text)
}

fn parse_projected<const NOTES: bool>(text: &str) -> (Evidence, usize) {
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
        if !parse_line::<NOTES>(line, &mut evidence) && !line.is_empty() {
            passed += 1;
        }
    }
    (evidence, passed)
}

/// Validates one line, retaining answers and, with `NOTES`, the other notes it holds.
fn parse_line<const NOTES: bool>(line: &str, evidence: &mut Evidence) -> bool {
    let mut words = [""; 11];
    let mut len = 0;
    for w in line.split(' ') {
        if len < words.len() {
            words[len] = w;
            len += 1;
        } else {
            return false;
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
                return false;
            };
            let iface = match *iface {
                "-" => None,
                iface if valid_iface(iface) => Some(iface.to_owned()),
                _ => return false,
            };
            let network = match *network {
                "-" => None,
                network => match NetworkId::parse(network) {
                    Some(network) => Some(network),
                    None => return false,
                },
            };
            let logged = match *logged {
                LOGGED => true,
                "-" => false,
                _ => return false,
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
                return true;
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
                if NOTES {
                    evidence.rounds.push(Round {
                        lane,
                        answered,
                        blocked,
                        began_ms,
                        at_ms,
                    });
                }
                return true;
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
                if NOTES {
                    evidence.internet.push(InternetNote {
                        ok,
                        source,
                        at_ms,
                        endpoint: (*endpoint).to_owned(),
                    });
                }
                return true;
            }
        }
        _ => {}
    }
    false
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
    read_projected_in::<true>(dir, judged)
}

/// Validate the complete admission ledger while retaining only its answers.
#[cfg(unix)]
pub(crate) fn read_answers_in(
    dir: &Path,
    judged: &aterm_uds::private_file::JudgedDir<'_>,
) -> (Evidence, usize) {
    read_projected_in::<false>(dir, judged)
}

fn read_projected_in<const NOTES: bool>(
    dir: &Path,
    judged: &aterm_uds::private_file::JudgedDir<'_>,
) -> (Evidence, usize) {
    match judged.read(FILE, MAX_BYTES) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => parse_projected::<NOTES>(&text),
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
pub fn add(dir: &Path, notes: Evidence) -> io::Result<()> {
    add_or_hold(dir, notes, aterm_uds::private_file::write_private).map_err(|(e, _)| e)
}

/// [`add`] by `write`, answering on failure `notes` alone to hold for the next: what the file
/// holds stays there, and held with them it would be written a second time.
fn add_or_hold(
    dir: &Path,
    notes: Evidence,
    write: fn(&Path, &str, &[u8]) -> io::Result<()>,
) -> Result<(), (io::Error, Evidence)> {
    if let Err(e) = aterm_uds::private_file::ensure_private_dir(dir) {
        return Err((e, notes));
    }
    let _lock = match aterm_uds::private_file::PrivateLock::take(
        dir,
        LOCK,
        LOCK_WAIT,
        "the network place's evidence is being written by another process",
    ) {
        Ok(lock) => lock,
        Err(e) => return Err((e, notes)),
    };
    let mut evidence = read(dir);
    evidence.answers.extend_from_slice(&notes.answers);
    evidence.rounds.extend_from_slice(&notes.rounds);
    evidence.internet.extend_from_slice(&notes.internet);
    write(dir, FILE, format(&compact(evidence)).as_bytes()).map_err(|e| (e, notes))
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
    if let Err((e, notes)) = add_or_hold(dir, notes, aterm_uds::private_file::write_private) {
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
#[path = "heard_tests.rs"]
mod tests;
