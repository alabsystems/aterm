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

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::place::{
    Answer, Book, BookMember, Evidence, InternetNote, InternetSource, Lane, NetworkId, Round,
    member_key,
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

fn opt<T: ToString>(value: Option<T>) -> String {
    value.map_or_else(|| "-".to_owned(), |v| v.to_string())
}

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
    let mut seen = std::collections::HashSet::new();
    evidence
        .answers
        .retain(|answer| seen.insert((answer.lane, answer.member.clone())));
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
    lines.sort();
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
    read_counted(dir).0
}

pub(crate) fn read_in(dir: &Path, judged: &aterm_uds::private_file::JudgedDir<'_>) -> Evidence {
    read_counted_in(dir, judged).0
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
        return refused_read(dir);
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
        Err(_) => refused_read(dir),
    }
}

fn refused_read(dir: &Path) -> (Evidence, usize) {
    let dir_is_symlink =
        std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_symlink());
    let file_present = !matches!(
        std::fs::symlink_metadata(dir.join(FILE)),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    );
    (
        Evidence::default(),
        usize::from(dir_is_symlink || file_present),
    )
}

/// Adds `notes` to the evidence in `dir` under [`LOCK`] ([the lock rule](self)), bounded by
/// [`compact`].
///
/// # Errors
/// The directory could not be made private, the lock was held past [`LOCK_WAIT`], or the file
/// could not be written.
pub fn add(dir: &Path, notes: Evidence) -> io::Result<()> {
    aterm_uds::private_file::ensure_private_dir(dir)?;
    let _lock = aterm_uds::private_file::PrivateLock::take(
        dir,
        LOCK,
        LOCK_WAIT,
        "the network place's evidence is being written by another process",
    )?;
    let mut evidence = read(dir);
    evidence.answers.extend(notes.answers);
    evidence.rounds.extend(notes.rounds);
    evidence.internet.extend(notes.internet);
    aterm_uds::private_file::write_private(dir, FILE, format(&compact(evidence)).as_bytes())
}

/// The fleet book snapshot's text.
#[must_use]
pub fn format_book(book: &Book) -> String {
    book.members
        .iter()
        .map(|member| {
            format!(
                "m {} {} {} {}\n",
                member.member,
                opt(member.addr),
                opt(member.answered_ms),
                member.network.as_ref().map_or("-", NetworkId::as_str)
            )
        })
        .collect()
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
    let network = network.filter(|network| network.is_strong());
    let book = Book {
        members: members
            .into_iter()
            .map(|(node, addr, answered_ms)| {
                let member = member_key(Lane::Fleet, &node);
                let booked = held
                    .members
                    .iter()
                    .find(|row| row.member == member && row.answered_ms == answered_ms)
                    .map_or_else(
                        || answered_ms.and(network.cloned()),
                        |row| row.network.clone(),
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
fn due(noted: &Noted, dir: &Path, key: &str, now: u64, stay: Option<u64>) -> bool {
    noted
        .last
        .get(dir)
        .and_then(|by_key| by_key.get(key))
        .is_none_or(|last| {
            now.saturating_sub(*last) >= COALESCE_MS || stay.is_some_and(|start| *last < start)
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
        let dir_last = noted.last.entry(dir.to_path_buf()).or_default();
        for key in keys {
            dir_last.insert(key, now);
        }
    }
    drop(guard);
    if let Err(e) = add(dir, notes.clone()) {
        aterm_log::debug!("network place: a note waits for the next ({e})");
        let mut guard = NOTED.lock().unwrap_or_else(PoisonError::into_inner);
        let held = guard
            .get_or_insert_with(Noted::default)
            .pending
            .entry(dir.to_path_buf())
            .or_default();
        held.answers.extend(notes.answers);
        held.rounds.extend(notes.rounds);
        held.internet.extend(notes.internet);
        *held = compact(std::mem::take(held));
    }
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
    let key = member_key(lane, member);
    let noted_key = format!("{} {key}", lane.word());
    let stay = stay_start(dir);
    {
        let mut guard = NOTED.lock().unwrap_or_else(PoisonError::into_inner);
        if !due(
            guard.get_or_insert_with(Noted::default),
            dir,
            &noted_key,
            at_ms,
            stay,
        ) {
            return;
        }
    }
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
    let stay = stay_start(dir);
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
            let key = format!("{} {}", answer.lane.word(), answer.member);
            if !due(noted, dir, &key, answer.at_ms, stay) {
                return false;
            }
            keys.push(key);
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
    let stay = stay_start(dir);
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
        if !changed && !due(noted, dir, noted_key, at_ms, stay) {
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
}
