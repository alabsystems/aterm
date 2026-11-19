// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm pkg unit-source` (DC-24b): the program a Full Access Mac's build front names to targo
//! (`atpkg::build_cache::unit_source`), which asks it for each shared dependency unit its build
//! would compile, and the fleet listener's answer to another member's ask
//! ([`serve_connection`]). docs/DESIGN-disk-custody-2026-11-12.md §5.
//!
//! THE SOURCE ([`main_entry`]) speaks Trust's DC-24a wire on its standard input and output:
//! targo sends one JSON line per ask (`{"key":…,"staging":…,"version":1}`), and the source sends
//! `working` while it makes progress on the ask (a member dialled or answered, bytes landed),
//! then `staged` or `none`. It reads its first ask at once and this Mac's fleet on a thread of
//! its own. It asks only the members this Mac's listener lets drive it
//! (`fleet_member::admitted_members`: a granted member whose key the books bind, on no deny list
//! or membership log hold), read again before each ask that dials and with this Mac's access
//! read again before each ask, over the fleet's `atf/2` channel ([`FleetOp::Built`]), and reads
//! an answer only from a member that proved the key the books bind it to. Every member that may
//! be asked is asked `has` at once; the first to say yes sends the unit, and the next that said
//! yes is asked only where that one's unit is refused. A member that does not answer within
//! [`aterm_net::built::ANSWER_WITHIN`], stalls, falls below the transfer's pace, sends what it
//! did not say or refuses is passed over for the rest of the build; a connection idle for
//! [`KEEP_IDLE`] is closed (a listener's slot is not held between a build's asks), and one older
//! than [`REDIAL_AFTER`] is dialled again before an ask. Each ask ends by [`ASK_WITHIN`], well
//! inside targo's own limit, and no phase of it is silent for longer than targo's 2 s.
//!
//! A unit lands file by file, each kept only where it matches the size and SHA-256 sent, and
//! only where this Mac's disk keeps its floor with the unit's bytes on it. It is staged only
//! where one landed fingerprint record is the key and its one output receipt names exactly the
//! `out/` files that landed with the size and SHA-256 each landed with; an `out/` file the
//! receipt does not name is not staged. Anything else is refused here, its files removed, and
//! the next member that said yes is asked. Targo then judges what was staged by its own check.
//! Below Full Access the source asks no member and answers `none`.
//!
//! It prints nothing else, to its output or the build's: each build that asked leaves one row in
//! the disk journal (`disk.jsonl`, [`SOURCE_ROW_KIND`]) naming the units taken and their bytes by
//! member, each unit refused with its key, member and reason, and each member passed over with
//! why; each connection a listener answered leaves one ([`SERVE_ROW_KIND`]).

use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_json::{Map, Value};
use aterm_net::built::{self, Bounds, Landed, Missed, Received};
use aterm_net::fleet::{FleetOp, FleetSecret, NodeIdentity, NodeKey};
use aterm_net::fleet_member::{Peer, Reached};

/// The disk journal row a build's source leaves.
pub const SOURCE_ROW_KIND: &str = "unit-source";

/// The disk journal row a listener leaves for each connection it answered.
pub const SERVE_ROW_KIND: &str = "unit-serve";

/// How often the source looks at its ask's progress, saying `working` where there was some:
/// targo passes over a source silent for 2 s.
const HEARTBEAT: Duration = Duration::from_millis(250);

/// The longest a member's dial may take: a connect and a handshake, inside targo's 2 s of
/// silence, every member dialled at once.
const DIAL_WITHIN: Duration = Duration::from_millis(1500);

/// The longest one ask may take, however it progresses: well inside targo's 120 s, after which
/// it would pass the source over for the build.
pub const ASK_WITHIN: Duration = Duration::from_secs(60);

/// A build's connections close once no ask came for this long, so none holds a member's
/// listener slot between a build's bursts of asks.
pub const KEEP_IDLE: Duration = Duration::from_secs(1);

/// A connection this old is dialled again before an ask: a listener cuts a proved connection at
/// `aterm_net::fleet::SERVED_WITHIN` (30 s), and an ask on a fresh one has that long.
pub const REDIAL_AFTER: Duration = Duration::from_secs(20);

/// The longest the first ask waits for this Mac's fleet to be read (its keys, books, grants and
/// deny list): reading on, it answers `none` until the read is done.
const LOAD_WITHIN: Duration = Duration::from_secs(10);

/// How long a listener waits for a build's next ask on a connection it answered: the source
/// closes one idle for [`KEEP_IDLE`], so one idle this long is not a build's.
pub const SERVE_IDLE: Duration = Duration::from_secs(5);

/// The longest ask line read from targo.
const ASK_MAX: u64 = 64 * 1024;

/// Why a unit was not taken, as its journal row says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No fingerprint record that landed has the key asked.
    Fingerprint,
    /// The unit carries no one output receipt, or its outputs differ from it.
    Receipt,
    /// Staged, and targo did not keep it.
    NotKept,
}

impl Refusal {
    /// Its word in the journal.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fingerprint => "fingerprint",
            Self::Receipt => "receipt",
            Self::NotKept => "not-kept",
        }
    }
}

/// A member the source may ask, and where it stands in this build.
struct Member {
    peer: Peer,
    node: String,
    key: NodeKey,
    conn: Option<Conn>,
    passed: Option<String>,
}

/// A member's connection and when it was dialled.
struct Conn {
    reached: Reached,
    at: Instant,
}

/// Who this Mac is in its fleet, and whom it may ask.
struct Fleet {
    me: (FleetSecret, NodeIdentity),
    members: Vec<Member>,
}

/// This Mac's fleet: being read on a thread of its own, or read (`None` where this Mac is below
/// Full Access or in no fleet).
enum Loading {
    Reading(Receiver<Option<Fleet>>),
    Read(Option<Fleet>),
}

impl Loading {
    /// The fleet, waiting for its read up to [`LOAD_WITHIN`] (the wait is progress: the read is
    /// this Mac's own).
    fn ready(&mut self, progress: &AtomicU64) -> Option<&mut Fleet> {
        if let Self::Reading(read) = self {
            let until = Instant::now() + LOAD_WITHIN;
            loop {
                match read.recv_timeout(HEARTBEAT) {
                    Ok(fleet) => {
                        *self = Self::Read(fleet);
                        break;
                    }
                    Err(RecvTimeoutError::Timeout) if Instant::now() < until => {
                        progress.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(RecvTimeoutError::Timeout) => return None,
                    Err(RecvTimeoutError::Disconnected) => {
                        *self = Self::Read(None);
                        break;
                    }
                }
            }
        }
        match self {
            Self::Read(fleet) => fleet.as_mut(),
            Self::Reading(_) => None,
        }
    }

    /// Close every connection.
    fn close_all(&mut self) {
        if let Self::Read(Some(fleet)) = self {
            for member in &mut fleet.members {
                member.conn = None;
            }
        }
    }
}

/// What the source reads of this Mac again as it runs.
struct Here {
    /// Whether this Mac is at Full Access now.
    full: Box<dyn Fn() -> bool + Send + Sync>,
    /// The members this Mac's listener lets drive it now, by node and the key its books bind.
    admitted: Box<dyn Fn() -> Vec<(String, NodeKey)> + Send + Sync>,
    /// Whether `bytes` more may land in the folder: its volume keeps its floor.
    room: Box<dyn Fn(&Path, u64) -> bool + Send + Sync>,
}

/// A unit the source staged: where each of its files landed, to tell at the build's end whether
/// targo placed them (a placed file is the staged one, renamed).
struct Staged {
    key: String,
    member: String,
    store: PathBuf,
    dir: String,
    files: Vec<(Landed, Option<(u64, u64)>)>,
}

/// What one build's asks came to.
#[derive(Default)]
struct Outcome {
    asked: u64,
    staged: Vec<Staged>,
    refused: Vec<(String, String, Refusal)>,
    passed: Vec<(String, String)>,
}

/// `aterm pkg unit-source`: serve targo's asks on standard input and output until it closes
/// them, then leave the build's row in the disk journal. Always succeeds: targo passes over a
/// source that exits.
pub fn main_entry() -> ExitCode {
    let state = aterm_types::dirs::state_dir();
    let (read, fleet) = mpsc::channel();
    {
        let state = state.clone();
        std::thread::spawn(move || {
            let _ = read.send(state.as_deref().and_then(fleet_now));
        });
    }
    let here = Here {
        full: Box::new({
            let state = state.clone();
            move || state.as_deref().is_some_and(full_access)
        }),
        admitted: Box::new(|| {
            crate::fleet_member::admitted_members()
                .into_iter()
                .filter_map(|(peer, key)| Some((peer.node?, key)))
                .collect()
        }),
        room: Box::new(|folder, bytes| {
            atpkg::freespace::available_bytes(folder)
                .is_some_and(|free| free.saturating_sub(atpkg::cost::FREE_FLOOR) >= bytes)
        }),
    };
    let asks = lines(io::BufReader::new(io::stdin()));
    let stdout = io::stdout();
    let outcome = run(&asks, &mut stdout.lock(), Loading::Reading(fleet), &here);
    if let Some(state) = state
        && outcome.asked > 0
    {
        journal(&state, &source_row(&outcome));
    }
    ExitCode::SUCCESS
}

fn full_access(state: &Path) -> bool {
    atpkg::build_cache::unit_source::level(state) == aterm_types::fleet_access::FleetAccess::Full
}

/// This Mac's fleet, where it is at Full Access and in one: who it is, and the members to ask
/// now, by node.
fn fleet_now(state: &Path) -> Option<Fleet> {
    if !full_access(state) {
        return None;
    }
    let secret = aterm_net::fleet::read_key_file(state).ok()?;
    let identity = aterm_net::fleet_member::read_node_identity(state).ok()??;
    let mut members: Vec<Member> = crate::fleet_member::admitted_members()
        .into_iter()
        .filter_map(|(peer, key)| {
            Some(Member {
                node: peer.node.clone()?,
                peer,
                key,
                conn: None,
                passed: None,
            })
        })
        .collect();
    members.sort_by(|a, b| a.node.cmp(&b.node));
    Some(Fleet {
        me: (secret, identity),
        members,
    })
}

/// Each line of `input`, read on a thread of its own, until it ends.
fn lines<R: BufRead + Send + 'static>(mut input: R) -> Receiver<Vec<u8>> {
    let (sent, lines) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let mut line = Vec::new();
            match (&mut input).take(ASK_MAX).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            if sent.send(line).is_err() {
                return;
            }
        }
    });
    lines
}

/// Answer each ask of `asks` on `output` until they end.
fn run(
    asks: &Receiver<Vec<u8>>,
    output: &mut dyn Write,
    mut fleet: Loading,
    here: &Here,
) -> Outcome {
    let mut outcome = Outcome::default();
    loop {
        let line = match asks.recv_timeout(KEEP_IDLE) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => {
                fleet.close_all();
                match asks.recv() {
                    Ok(line) => line,
                    Err(_) => break,
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let Some((key, staging)) = parsed(&line) else {
            // Not an ask this source understands: targo passes over a source that answers
            // anything else, so it answers what it can.
            if say(output, "none").is_err() {
                break;
            }
            continue;
        };
        outcome.asked += 1;
        let progress = AtomicU64::new(0);
        let staged = std::thread::scope(|scope| {
            let (done, finished) = mpsc::channel();
            let (fleet, outcome, progress) = (&mut fleet, &mut outcome, &progress);
            let (key, staging) = (key.as_str(), staging.as_path());
            scope.spawn(move || {
                let _ = done.send(answer(fleet, here, key, staging, progress, outcome));
            });
            let mut said = 0;
            loop {
                match finished.recv_timeout(HEARTBEAT) {
                    Ok(staged) => return Some(staged),
                    Err(RecvTimeoutError::Timeout) => {
                        let now = progress.load(Ordering::Relaxed);
                        if now != said {
                            said = now;
                            if say(output, "working").is_err() {
                                return None;
                            }
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return Some(false),
                }
            }
        });
        let Some(staged) = staged else { break };
        if say(output, if staged { "staged" } else { "none" }).is_err() {
            break;
        }
    }
    outcome
}

fn say(output: &mut dyn Write, line: &str) -> io::Result<()> {
    output.write_all(format!("{line}\n").as_bytes())?;
    output.flush()
}

/// An ask's key and staging folder: version 1, a key that is one, and a staging folder inside a
/// store's `staging/<build>/` that targo made new for it ([`fresh_folder`]).
fn parsed(line: &[u8]) -> Option<(String, PathBuf)> {
    let ask: Value = aterm_json::from_slice(line).ok()?;
    if ask.get("version")?.as_u64()? != 1 {
        return None;
    }
    let key = ask.get("key")?.as_str()?;
    let staging = PathBuf::from(ask.get("staging")?.as_str()?);
    (built::is_key(key) && store_of(&staging).is_some() && fresh_folder(&staging))
        .then(|| (key.to_owned(), staging))
}

/// Whether `folder` is as targo makes an ask's: an absolute plain directory reached through no
/// link (its real path is the one given), this user's, private (0700) and empty. A store
/// reached through a link is asked for nothing, and its units compile.
fn fresh_folder(folder: &Path) -> bool {
    if !folder.is_absolute() || std::fs::canonicalize(folder).ok().as_deref() != Some(folder) {
        return false;
    }
    let Ok(meta) = std::fs::symlink_metadata(folder) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != aterm_uds::peer::our_uid() || meta.mode() & 0o777 != 0o700 {
            return false;
        }
    }
    meta.is_dir() && std::fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_none())
}

/// The store an ask's staging folder is in: `<store>/staging/<build>/<n>`.
fn store_of(staging: &Path) -> Option<&Path> {
    let staging_root = staging.parent()?.parent()?;
    (staging_root.file_name()? == "staging").then_some(staging_root.parent()?)
}

/// What one member's `get` came to.
enum Fetched {
    /// Landed and judged: these files are staged.
    Staged(Received),
    /// Landed and refused here; its files are removed.
    Refused(Refusal),
    /// It holds no unit under the key now.
    None,
    /// It missed ([`Missed`]).
    Missed(Missed),
}

/// Answer one ask: every member that may be asked is asked `has` at once, the first to say yes
/// sends the unit, and the others that said yes are asked in turn only while the units sent are
/// refused. Whether a unit was staged in `staging`.
fn answer(
    loading: &mut Loading,
    here: &Here,
    key: &str,
    staging: &Path,
    progress: &AtomicU64,
    outcome: &mut Outcome,
) -> bool {
    let Some(fleet) = loading.ready(progress) else {
        return false;
    };
    if !(here.full)() {
        // Access fell below Full during the build: ask no one again.
        fleet.members.clear();
        return false;
    }
    let Some(store) = store_of(staging) else {
        return false;
    };
    let deadline = Instant::now() + ASK_WITHIN;
    for member in &mut fleet.members {
        if member
            .conn
            .as_ref()
            .is_some_and(|conn| conn.at.elapsed() >= REDIAL_AFTER)
        {
            member.conn = None;
        }
    }
    if fleet
        .members
        .iter()
        .any(|member| member.passed.is_none() && member.conn.is_none())
    {
        // A member is dialled only where this Mac's listener still lets it drive here.
        let admitted = (here.admitted)();
        progress.fetch_add(1, Ordering::Relaxed);
        for member in &mut fleet.members {
            if member.passed.is_none()
                && member.conn.is_none()
                && !admitted
                    .iter()
                    .any(|(node, key)| *node == member.node && *key == member.key)
            {
                member.passed = Some("no longer admitted".to_owned());
                outcome
                    .passed
                    .push((member.node.clone(), "no longer admitted".to_owned()));
            }
        }
    }
    let me = &fleet.me;
    let room = |bytes| (here.room)(staging, bytes);
    let claimed = AtomicBool::new(false);
    let first: Mutex<Option<(usize, Fetched)>> = Mutex::new(None);
    let (said_yes, yeses) = mpsc::channel::<usize>();
    std::thread::scope(|scope| {
        for (at, member) in fleet
            .members
            .iter_mut()
            .enumerate()
            .filter(|(_, member)| member.passed.is_none())
        {
            let (claimed, first, said_yes, room) = (&claimed, &first, said_yes.clone(), &room);
            scope.spawn(move || {
                let held = exchange(member, me, deadline, progress, &mut |stream, bounds| {
                    built::has(stream, key, bounds)
                });
                match held {
                    Ok(true) if !claimed.swap(true, Ordering::SeqCst) => {
                        let fetched =
                            fetch(member, me, (key, staging, store), deadline, progress, room);
                        *first.lock().unwrap_or_else(PoisonError::into_inner) = Some((at, fetched));
                    }
                    Ok(true) => {
                        let _ = said_yes.send(at);
                    }
                    Ok(false) => {}
                    Err(missed) => member.passed = Some(missed.to_string()),
                }
            });
        }
    });
    drop(said_yes);
    for member in &fleet.members {
        if let Some(why) = &member.passed
            && !outcome.passed.iter().any(|(node, _)| *node == member.node)
        {
            outcome.passed.push((member.node.clone(), why.clone()));
        }
    }
    let mut next = first.into_inner().unwrap_or_else(PoisonError::into_inner);
    let mut yeses = yeses.try_iter();
    loop {
        let Some((at, fetched)) = next.take() else {
            let Some(at) = yeses.next() else {
                return false;
            };
            let member = &mut fleet.members[at];
            next = Some((
                at,
                fetch(member, me, (key, staging, store), deadline, progress, &room),
            ));
            continue;
        };
        let member = &mut fleet.members[at];
        match fetched {
            Fetched::Staged(received) => {
                let files = received
                    .files
                    .into_iter()
                    .map(|landed| {
                        let at = inode(&staging.join(&landed.rel));
                        (landed, at)
                    })
                    .collect();
                outcome.staged.push(Staged {
                    key: key.to_owned(),
                    member: member.node.clone(),
                    store: store.to_path_buf(),
                    dir: received.dir,
                    files,
                });
                return true;
            }
            Fetched::Refused(refusal) => {
                outcome
                    .refused
                    .push((key.to_owned(), member.node.clone(), refusal));
            }
            Fetched::None => {}
            Fetched::Missed(Missed::NoRoom(_)) => {
                // This Mac's disk, not the member: compile here, and ask on another ask.
                return false;
            }
            Fetched::Missed(missed) => {
                let why = missed.to_string();
                outcome.passed.push((member.node.clone(), why.clone()));
                member.passed = Some(why);
            }
        }
    }
}

/// Take the unit under `key` from `member` into `staging` (of `store`) and judge it.
fn fetch(
    member: &mut Member,
    me: &(FleetSecret, NodeIdentity),
    (key, staging, store): (&str, &Path, &Path),
    deadline: Instant,
    progress: &AtomicU64,
    room: &dyn Fn(u64) -> bool,
) -> Fetched {
    let received = exchange(member, me, deadline, progress, &mut |stream, bounds| {
        built::get(stream, key, staging, bounds, room, &mut |_| {
            progress.fetch_add(1, Ordering::Relaxed);
        })
    });
    match received {
        Ok(Some(received)) => match judged(&received, key, store, staging) {
            Ok(unlisted) => {
                for rel in &unlisted {
                    let _ = std::fs::remove_file(staging.join(rel));
                }
                Fetched::Staged(Received {
                    dir: received.dir,
                    files: received
                        .files
                        .into_iter()
                        .filter(|landed| !unlisted.contains(&landed.rel))
                        .collect(),
                })
            }
            Err(refusal) => {
                remove_landed(staging, &received);
                Fetched::Refused(refusal)
            }
        },
        Ok(None) => Fetched::None,
        Err(missed) => Fetched::Missed(missed),
    }
}

/// A stream an ask is made on.
trait Wire: Read + Write {}

impl<T: Read + Write> Wire for T {}

/// Make one ask (`call`) of `member` on its connection: dialled where it has none, and dialled
/// once more where a connection it already had was closed (a listener ends one at its lifetime).
/// A connection whose ask failed otherwise is dropped: what is left on it is unknown.
fn exchange<T>(
    member: &mut Member,
    me: &(FleetSecret, NodeIdentity),
    deadline: Instant,
    progress: &AtomicU64,
    call: &mut dyn FnMut(&mut dyn Wire, &Bounds<'_>) -> Result<T, Missed>,
) -> Result<T, Missed> {
    for attempt in 0..2 {
        let reused = member.conn.is_some();
        if !reused {
            let dialled = dial(member, me);
            progress.fetch_add(1, Ordering::Relaxed);
            member.conn = Some(Conn {
                reached: dialled?,
                at: Instant::now(),
            });
        }
        let Some(conn) = member.conn.as_mut() else {
            return Err(Missed::Closed);
        };
        let answered = (|| {
            let socket = conn
                .reached
                .transport
                .stream()
                .get_ref()
                .try_clone()
                .map_err(Missed::Io)?;
            socket
                .set_write_timeout(Some(seamed(built::IDLE)))
                .map_err(Missed::Io)?;
            let timeout = |within: Duration| socket.set_read_timeout(Some(seamed(within)));
            let bounds = Bounds {
                timeout: &timeout,
                deadline,
            };
            let answered = call(conn.reached.transport.stream(), &bounds);
            progress.fetch_add(1, Ordering::Relaxed);
            answered
        })();
        match answered {
            Ok(answer) => return Ok(answer),
            Err(Missed::Closed | Missed::Cut(_)) if reused && attempt == 0 => {
                member.conn = None;
            }
            Err(missed) => {
                member.conn = None;
                return Err(missed);
            }
        }
    }
    Err(Missed::Closed)
}

/// Dial `member` on [`FleetOp::Built`] and hold it to the key its books bind it to.
fn dial(
    member: &Member,
    (secret, identity): &(FleetSecret, NodeIdentity),
) -> Result<Reached, Missed> {
    let reached = aterm_net::fleet_member::dial(
        &member.peer.host,
        member.peer.addr,
        member.peer.port,
        (secret, Some(identity)),
        FleetOp::Built,
        dial_within(),
    )
    .map_err(|why| Missed::Refused(why.to_string()))?;
    let proved = reached
        .peer
        .as_ref()
        .is_some_and(|proof| proof.node == member.node && proof.key == member.key);
    if !proved {
        return Err(Missed::Refused(
            "it did not prove the key this Mac's books bind it to".to_owned(),
        ));
    }
    Ok(reached)
}

/// [`DIAL_WITHIN`], or longer where the fleet's handshake seam names longer ([`seamed`]).
fn dial_within() -> Duration {
    seamed(DIAL_WITHIN)
}

/// `within`, or in a debug build what the fleet's handshake seam names where that is longer: a
/// test's members are aterms on this machine, which answer however loaded it is, so each of the
/// wire's reads (an answer, a unit's next bytes) and a dial wait the test's hang detector.
fn seamed(within: Duration) -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var(aterm_net::fleet::HANDSHAKE_SEAM)
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
    {
        return within.max(Duration::from_millis(ms));
    }
    within
}

/// Whether `received` is staged, and if so which landed `out/` files its receipt does not name
/// (they are not staged); else why it is refused here. It is staged only where one landed
/// fingerprint record has `key`, and exactly one output receipt (`fingerprint/outputs-*.json`)
/// landed whose every entry (an output named by the SHA-256 of its absolute path in `store`)
/// is a landed `out/` file of the entry's size and SHA-256.
fn judged(
    received: &Received,
    key: &str,
    store: &Path,
    staging: &Path,
) -> Result<Vec<String>, Refusal> {
    let is_record = |rel: &str| {
        rel.strip_prefix("fingerprint/")
            .is_some_and(|name| name.ends_with(".json") && !name.starts_with("outputs-"))
    };
    if !received
        .files
        .iter()
        .any(|landed| is_record(&landed.rel) && landed.sha256 == key)
    {
        return Err(Refusal::Fingerprint);
    }
    let receipts: Vec<&Landed> = received
        .files
        .iter()
        .filter(|landed| {
            landed
                .rel
                .strip_prefix("fingerprint/outputs-")
                .is_some_and(|rest| rest.ends_with(".json"))
        })
        .collect();
    let [receipt] = receipts.as_slice() else {
        return Err(Refusal::Receipt);
    };
    let outputs = receipt_outputs(&staging.join(&receipt.rel)).ok_or(Refusal::Receipt)?;
    if outputs.is_empty() {
        return Err(Refusal::Receipt);
    }
    let out: Vec<(String, &Landed)> = received
        .files
        .iter()
        .filter(|landed| landed.rel.starts_with("out/"))
        .map(|landed| {
            let path = store.join(&received.dir).join(&landed.rel);
            (
                built::sha256_hex(path.as_os_str().as_encoded_bytes()),
                landed,
            )
        })
        .collect();
    let mut named = Vec::with_capacity(outputs.len());
    for (at, size, sha) in &outputs {
        let Some((_, landed)) = out.iter().find(|(digest, _)| digest == at) else {
            return Err(Refusal::Receipt);
        };
        if landed.size != *size || landed.sha256 != *sha {
            return Err(Refusal::Receipt);
        }
        named.push(landed.rel.as_str());
    }
    Ok(out
        .iter()
        .filter(|(_, landed)| !named.contains(&landed.rel.as_str()))
        .map(|(_, landed)| landed.rel.clone())
        .collect())
}

/// The `outputs` a receipt records: each output's path digest, size and SHA-256.
fn receipt_outputs(path: &Path) -> Option<Vec<(String, u64, String)>> {
    let bytes = std::fs::read(path).ok()?;
    let receipt: Value = aterm_json::from_slice(&bytes).ok()?;
    receipt
        .get("outputs")?
        .as_array()?
        .iter()
        .map(|output| {
            Some((
                output.get("path_sha256")?.as_str()?.to_owned(),
                output.get("size")?.as_u64()?,
                output.get("sha256")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

fn remove_landed(staging: &Path, received: &Received) {
    for landed in &received.files {
        let _ = std::fs::remove_file(staging.join(&landed.rel));
    }
}

#[cfg(unix)]
fn inode(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn inode(_: &Path) -> Option<(u64, u64)> {
    None
}

/// Whether targo kept `staged`: its fingerprint record now stands in the unit's directory as
/// the file the source staged (targo renames a staged file into place, and removes what it
/// placed for a unit it refuses).
fn kept(staged: &Staged) -> bool {
    staged.files.iter().any(|(landed, at)| {
        landed.sha256 == staged.key
            && at.is_some()
            && inode(&staged.store.join(&staged.dir).join(&landed.rel)) == *at
    })
}

/// The build's row: the units kept and their bytes by member, each unit refused, and each member
/// passed over with why.
fn source_row(outcome: &Outcome) -> String {
    let mut taken: std::collections::BTreeMap<&str, (u64, u64)> = std::collections::BTreeMap::new();
    let mut refused: Vec<(String, String, Refusal)> = outcome.refused.clone();
    for staged in &outcome.staged {
        if kept(staged) {
            let entry = taken.entry(staged.member.as_str()).or_default();
            entry.0 += 1;
            entry.1 += staged
                .files
                .iter()
                .filter(|(landed, at)| {
                    at.is_some() && inode(&staged.store.join(&staged.dir).join(&landed.rel)) == *at
                })
                .map(|(landed, _)| landed.size)
                .sum::<u64>();
        } else {
            refused.push((staged.key.clone(), staged.member.clone(), Refusal::NotKept));
        }
    }
    let mut row = head(SOURCE_ROW_KIND);
    row.insert("asked".to_owned(), Value::from(outcome.asked));
    row.insert(
        "taken".to_owned(),
        Value::Array(
            taken
                .into_iter()
                .map(|(member, (units, bytes))| {
                    let mut entry = Map::new();
                    entry.insert("member".to_owned(), Value::from(member.to_owned()));
                    entry.insert("units".to_owned(), Value::from(units));
                    entry.insert("bytes".to_owned(), Value::from(bytes));
                    Value::Object(entry)
                })
                .collect(),
        ),
    );
    row.insert(
        "refused".to_owned(),
        Value::Array(
            refused
                .into_iter()
                .map(|(key, member, refusal)| {
                    let mut entry = Map::new();
                    entry.insert("key".to_owned(), Value::from(key));
                    entry.insert("member".to_owned(), Value::from(member));
                    entry.insert("reason".to_owned(), Value::from(refusal.word().to_owned()));
                    Value::Object(entry)
                })
                .collect(),
        ),
    );
    row.insert(
        "passed".to_owned(),
        Value::Array(
            outcome
                .passed
                .iter()
                .map(|(member, why)| {
                    let mut entry = Map::new();
                    entry.insert("member".to_owned(), Value::from(member.clone()));
                    entry.insert(
                        "why".to_owned(),
                        Value::from(aterm_codec::tty::one_line(why, 200)),
                    );
                    Value::Object(entry)
                })
                .collect(),
        ),
    );
    Value::Object(row).to_string()
}

fn head(kind: &str) -> Map {
    let now = aterm_time::unix_secs()
        .and_then(|secs| i64::try_from(secs).ok())
        .unwrap_or(0);
    let mut row = Map::new();
    row.insert(
        "ts".to_owned(),
        Value::from(crate::harness::usage::rfc3339_utc(now)),
    );
    row.insert("sid".to_owned(), Value::from(String::new()));
    row.insert("kind".to_owned(), Value::from(kind.to_owned()));
    row
}

/// Append `row` to the disk journal under the aterm state `state`, cut back as every writer of
/// it does first.
fn journal(state: &Path, row: &str) {
    let harness = crate::harness::cli::harness_state_in(state);
    #[cfg(unix)]
    for dir in [harness.parent().unwrap_or(state), harness.as_path()] {
        if aterm_uds::private_file::ensure_private_dir(dir).is_err() {
            return;
        }
    }
    let path = harness.join(crate::harness::cli::DISK_LEDGER);
    let _ = crate::harness::cli::bound_ledger(
        &path,
        crate::harness::cli::DISK_LEDGER_MAX_BYTES,
        crate::harness::cli::DISK_LEDGER_KEEP_ROWS,
    );
    let _ = aterm_uds::line_log::append(&path, row);
}

/// The fleet listener's answer to a member's [`FleetOp::Built`] connection (`member`, the node it
/// proved): every key it asks is answered from targo's store here
/// (`atpkg::build_cache::unit_source::held` over `shared_units::roots`), never from a project's
/// build folder; `err` to all where the person turned sharing off (`aterm pkg serve-units
/// --off`) or the member proved no node. One journal row per connection.
///
/// # Errors
/// A write to the member failed.
pub fn serve_connection<S: Read + Write>(stream: &mut S, member: Option<&str>) -> io::Result<()> {
    let off =
        atpkg::store::resolve_configured().is_some_and(|layout| atpkg::peers::turned_off(&layout));
    let refusal = if off {
        Some("sharing is off")
    } else if member.is_none() {
        Some("no node proved")
    } else {
        None
    };
    let roots = aterm_types::dirs::home_dir()
        .map(|home| atpkg::build_cache::shared_units::roots(&home))
        .unwrap_or_default();
    let served = built::serve(stream, refusal, &mut |key| {
        let held = atpkg::build_cache::unit_source::held(&roots, key)?;
        Some(built::Held {
            dir: held.dir,
            files: held
                .files
                .into_iter()
                .filter(|(rel, _)| built::is_unit_file(rel))
                .collect(),
            hold: Box::new(held.lock),
        })
    })?;
    if let Some(state) = aterm_types::dirs::state_dir() {
        let mut row = head(SERVE_ROW_KIND);
        row.insert(
            "member".to_owned(),
            Value::from(member.unwrap_or("-").to_owned()),
        );
        row.insert("asked".to_owned(), Value::from(served.asked));
        row.insert("units".to_owned(), Value::from(served.units));
        row.insert("bytes".to_owned(), Value::from(served.bytes));
        if let Some(ended) = served.ended {
            row.insert("ended".to_owned(), Value::from(ended));
        }
        journal(&state, &Value::Object(row).to_string());
    }
    Ok(())
}

#[cfg(test)]
#[path = "unit_source_tests.rs"]
mod tests;
