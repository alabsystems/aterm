// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fabric over the mesh (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, §4.11, §11 C3):
//! every Mac is one node of the fleet [`FLEET`], named by its node key, and its mail goes one hop.
//!
//! - **Names.** A Mac's node is `n-` and the first 16 hex digits of its node key
//!   ([`node_of_key`]), the principal astream binds and the link proves. Its fabric bridge writes
//!   `bus.log` under `producer_id_of` that node, and nothing else on this Mac does; the person
//!   carry writes its node's lanes under [`CARRIER`]'s.
//! - **Mail goes one hop.** For each Mac this Mac's sessions wrote to, the node service runs one
//!   push bridge ([`Lane::Mail`]) from this Mac's `bus.log` straight to that Mac's, over the
//!   mesh link. Its consumer group is the name the mail rule gives that lane's reader
//!   (`broker_rules.rs` `MAIL_READER`, [`mail_group`]), so the rule holds every record on the
//!   sender until the addressed Mac acknowledged it, through any partition. The receiving
//!   broker takes only mail addressed to itself and sent as the proven peer ([`peer_ring`]), and
//!   deduplicates on (producer, source offset), so each record lands there once.
//! - **A node-key rotation** changes a Mac's node. Its senders carry the mail they still hold
//!   for the old node to the new one under the new subjects, once ([`Lane::Move`]): the same push,
//!   from the old node's group, its subjects rewritten.
//! - **Topics go one hop** to each Mac that holds one ([`Lane::Say`]).
//! - **The hand-off.** The node service reads the membership log; the bridge does not. It writes
//!   what the bridge must know to route, [`Handoff`], beside `bus.sock` ([`HANDOFF_FILE`]): the
//!   current members, and which node each rotated one became.
//! - **A person's post to another Mac** (§11 C3c) is written on the person's own lane, sealed by
//!   this Mac's person key under the node key's certificate ([`crate::person_key`]). No push
//!   carries a person's lane and no peer's ring takes one. The node service carries each such
//!   post whose seal verifies onto this Mac's own lane to that Mac, naming the person
//!   ([`Routes`], [`carry_group`]); that Mac's bridge hands it to the session as the person's only
//!   once the seal verifies under the node key that names this Mac, which the certificate carries.
//!   A post that does not verify is not carried: the journal and the person's inbox say why.
//!
//! [`Routes`] runs the pushes. It watches the lanes this Mac writes, so a push runs only to a Mac
//! this Mac has mail for, and it runs each push until it is stopped, backing off after each
//! failure and trying again at once when the node service says a link came up. It runs the
//! person carry beside them.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use astream_broker::{
    Closer, Event, FederationBridge, FederationBridgeCloser, FederationConfig, FederationEndpoint,
    FederationError, FederationReport, StreamDial, SubjectRewrite,
};
use astream_cap::Capability;
use aterm_types::MutexExt;

use crate::body::Body;
use crate::person_key;
use crate::subject;

/// The one fleet every Mac of the lab belongs to.
pub const FLEET: &str = "alab";

/// The node key's file in aterm's state folder: 64 lowercase hex digits of its Ed25519 seed and a
/// newline, mode 0600, as aterm-net writes it.
pub const NODE_KEY_FILE: &str = "fleet-node-key";

/// The hand-off's file, beside `bus.sock` in the fabric root.
pub const HANDOFF_FILE: &str = "mesh-handoff";

/// The most records one push sends before it waits for their acknowledgement.
pub const BURST: usize = 256;

/// The first wait before a failed push or watch is tried again, doubled each failure up to
/// [`BACKOFF_MAX`].
pub const BACKOFF_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between two tries of one push.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Where a fabric bridge finds, with the mesh on, the node key that names it and the hand-off it
/// routes by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshPaths {
    /// aterm's state folder, which holds the node key ([`NODE_KEY_FILE`]).
    pub keys: PathBuf,
    /// The fabric root, beside `bus.sock`, where the node service writes the hand-off
    /// ([`HANDOFF_FILE`]).
    pub root: PathBuf,
}

impl MeshPaths {
    /// A bridge's, for the `bus.sock` it dials (`broker`): aterm's state folder, and the folder
    /// the socket is in. `None` with the mesh off (`mesh`, the switch as the caller read it), or
    /// where either folder cannot be named.
    #[must_use]
    pub fn of(mesh: bool, broker: &str) -> Option<Self> {
        if !mesh {
            return None;
        }
        Some(Self {
            keys: aterm_types::dirs::state_dir()?,
            root: Path::new(broker)
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())?
                .to_path_buf(),
        })
    }
}

/// The node id astream and the link derive from a node key: `n-` and the key's first 8 bytes in
/// lowercase hex.
#[must_use]
pub fn node_of(key: &[u8; 32]) -> String {
    astream_broker::identity_principal(key)
}

/// This Mac's node, from the node key in `dir` ([`NODE_KEY_FILE`]): `None` while there is none.
/// The seed is read, its public key derived, and every copy of it scrubbed.
///
/// # Errors
/// A key file that is not private to this user, cannot be read, or holds no seed.
pub fn node_of_key(dir: &Path) -> io::Result<Option<String>> {
    Ok(node_key(dir)?.map(|key| node_of(&key)))
}

/// This Mac's node key's public half, from the node key in `dir` ([`NODE_KEY_FILE`]): `None` while
/// there is none. The seed is read, its public key derived, and every copy of it scrubbed.
///
/// # Errors
/// A key file that is not private to this user, cannot be read, or holds no seed.
pub fn node_key(dir: &Path) -> io::Result<Option<[u8; 32]>> {
    let mut bytes = match aterm_uds::private_file::read_private(dir, NODE_KEY_FILE, 65) {
        Ok(bytes) => bytes,
        Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(None),
        Err(e) => {
            return Err(io::Error::other(format!(
                "{}: {e}",
                dir.join(NODE_KEY_FILE).display()
            )));
        }
    };
    let mut seed = [0u8; 32];
    let decoded = std::str::from_utf8(&bytes).ok().is_some_and(|text| {
        let digits = text.strip_suffix('\n').unwrap_or(text);
        aterm_codec::hex::is_lower(digits, 64)
            && aterm_codec::hex::decode_into(digits.as_bytes(), &mut seed)
    });
    aterm_uds::secret::scrub(&mut bytes);
    let key = decoded.then(|| astream_broker::origin::public_key(&seed));
    aterm_uds::secret::scrub(&mut seed);
    key.map(Some).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} holds no node key", dir.join(NODE_KEY_FILE).display()),
        )
    })
}

/// Whether `node` names a Mac: `n-` and 16 lowercase hex digits.
#[must_use]
pub fn is_node(node: &str) -> bool {
    node.strip_prefix("n-")
        .is_some_and(|hex| aterm_codec::hex::is_lower(hex, 16))
}

/// Every mail lane this Mac (`me`) writes to `dst`'s sessions: `/f/alab/in/<dst>/*/<me>/*`.
#[must_use]
pub fn mail_lanes(dst: &str, me: &str) -> String {
    format!("/f/{FLEET}/in/{dst}/*/{me}/*")
}

/// Every mail lane this Mac (`me`) writes, to any node: `/f/alab/in/*/*/<me>/*`.
#[must_use]
pub fn written_lanes(me: &str) -> String {
    format!("/f/{FLEET}/in/*/*/{me}/*")
}

/// The group the mail rule names the reader of `dst`'s lanes by
/// (`/f/alab/cur/<dst>/node/inbox`): a push to `dst` follows under it, so the rule holds what it
/// has not carried.
#[must_use]
pub fn mail_group(dst: &str) -> String {
    subject::inbox_group(FLEET, dst)
}

/// The node a mail lane `subject` this Mac (`me`) wrote is addressed to: `None` for any other
/// subject.
#[must_use]
pub fn addressee<'a>(subject: &'a str, me: &str) -> Option<&'a str> {
    let rest = subject.strip_prefix(&format!("/f/{FLEET}/in/"))?;
    let mut parts = rest.split('/');
    let (dst, sid, src, kind) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (parts.next().is_none()
        && src == me
        && is_node(dst)
        && subject::is_principal(sid)
        && !kind.is_empty())
    .then_some(dst)
}

/// Every topic broadcast this Mac (`me`) writes: `/f/alab/pub/<me>/*/say/>`.
#[must_use]
pub fn say_lanes(me: &str) -> String {
    format!("/f/{FLEET}/pub/{me}/*/say/>")
}

/// The leaf of a node's face that names the topics its sessions hold.
pub const TOPICS_LEAF: &str = "topics";

/// Where this Mac's (`me`) bridge names the topics its sessions hold,
/// `/f/alab/pub/<me>/node/topics`: the node service reads the newest into this Mac's presence
/// row, and the other Macs push their topic broadcasts to a Mac whose row names one (§4.11).
/// The bridge writes it only when the set changes, so the default retention rule bounds it and
/// it needs no class of its own (one would change astream's `aterm-broker.retain` fixture).
#[must_use]
pub fn topics_face(me: &str) -> String {
    crate::subject::node_face(FLEET, me, TOPICS_LEAF)
}

/// A topics face's body: `v=1 t=<ms> topics=<topic>,<topic>…`, the topics in order.
#[must_use]
pub fn topics_body(t_ms: u64, topics: &BTreeSet<String>) -> String {
    let named: Vec<&str> = topics.iter().map(String::as_str).collect();
    format!("v=1 t={t_ms} topics={}", named.join(","))
}

/// The topics a topics face's `body` names; `None` for a body without the `topics=` word, or
/// one naming a word outside the topic grammar.
#[must_use]
pub fn topics_of(body: &[u8]) -> Option<BTreeSet<String>> {
    let text = std::str::from_utf8(body).ok()?;
    let named = text
        .split(' ')
        .find_map(|token| token.strip_prefix("topics="))?;
    named
        .split(',')
        .filter(|topic| !topic.is_empty())
        .map(|topic| subject::is_topic(topic).then(|| topic.to_owned()))
        .collect()
}

/// The group a push of this Mac's (`me`) topics to `dst` follows under.
#[must_use]
pub fn say_group(me: &str, dst: &str) -> String {
    format!("/f/{FLEET}/cur/{me}/say/{dst}")
}

/// The principal the person carry writes this Mac's lanes under (§11 C3c): a producer of its own,
/// so the sequence numbers of its records, the offsets of the posts it carries, never meet the
/// bridge's. Not a node's key-named principal, so no link's proof binds it.
pub const CARRIER: &str = "n-carry";

/// Every post on a Mac's `bus.log`, which the person carry reads: `/f/alab/in/*/*/*/*`.
#[must_use]
pub fn posts() -> String {
    format!("/f/{FLEET}/in/*/*/*/*")
}

/// The group the person carry of this Mac (`me`) follows under: `/f/alab/cur/<me>/node/carry`.
#[must_use]
pub fn carry_group(me: &str) -> String {
    format!("/f/{FLEET}/cur/{me}/node/carry")
}

/// A person's post this Mac (`me`) holds for another Mac's session, `subject` being
/// `/f/alab/in/<dst>/<sid>/<person>/<kind>`: `(dst, sid, person, kind)`, or `None` for any other
/// subject.
#[must_use]
pub fn person_post<'a>(subject: &'a str, me: &str) -> Option<(&'a str, &'a str, &'a str, &'a str)> {
    let rest = subject.strip_prefix(&format!("/f/{FLEET}/in/"))?;
    let mut parts = rest.split('/');
    let (dst, sid, person, kind) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    (parts.next().is_none()
        && is_node(dst)
        && dst != me
        && sid.starts_with("s-")
        && subject::is_principal(sid)
        && crate::person::is_person_id(person)
        && !kind.is_empty())
    .then_some((dst, sid, person, kind))
}

/// The grants a Mac (`me`) serves its `bus.log` of `fleet` ([`FLEET`] in the lab) to the proven
/// neighbour `peer` with (§4.11), each bound to `peer`, so only its own stream can attach them:
///
/// - mail addressed to this Mac, sent as the peer itself (`rw …/in/<me>/*/<peer>/*`): it can
///   forge neither another Mac's mail nor a person's, nor leave mail here for a third Mac;
/// - its own topic broadcasts (`rw …/pub/<peer>/*/say/>`);
/// - reading the halt rows (`ro …/fleet/>`);
/// - relaying halts (`rw,relay …/halt/>`), each origin-signed and checked against the seats, and
///   reading its own, so it finds the high water this Mac holds of it.
///
/// Nothing else: no presence row, no session log (`ev`), no person's lane, and no reader's group:
/// this Mac pushes the mail it holds, so nothing reads it here. A person's post from the peer
/// (§11 C3c) comes on the peer's own lane, sealed by the person key the peer's node key vouched
/// for, and is the person's only where this Mac's bridge verified that seal: the ring lets the
/// peer write it, never makes it human.
#[must_use]
pub fn peer_ring(fleet: &str, me: &str, peer: &str) -> Vec<String> {
    vec![
        format!("rw,p={peer}:/f/{fleet}/in/{me}/*/{peer}/*"),
        format!("rw,p={peer}:/f/{fleet}/pub/{peer}/*/say/>"),
        format!("ro,p={peer}:/f/{fleet}/fleet/>"),
        format!("rw,relay,p={peer}:/f/{fleet}/halt/>"),
        format!("ro,p={peer}:/f/{fleet}/halt/{peer}"),
    ]
}

/// What the node service hands the fabric bridge ([`HANDOFF_FILE`]): this Mac's node, the
/// current members, and which node each member that rotated its key became.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Handoff {
    /// This Mac's node as the node service read it.
    pub me: String,
    /// Every current member's node, this Mac's among them.
    pub members: BTreeSet<String>,
    /// Each node a key rotation retired, and the node its seat holds now.
    pub moved: BTreeMap<String, String>,
}

/// The hand-off's first line.
const HANDOFF_HEAD: &str = "mesh-handoff v1";

/// The longest hand-off a reader takes.
pub const HANDOFF_MAX: usize = 1 << 20;

impl Handoff {
    /// Its file's text: the head line, then `me`, `member` and `moved` lines.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("{HANDOFF_HEAD}\nme {}\n", self.me);
        for member in &self.members {
            out.push_str(&format!("member {member}\n"));
        }
        for (old, new) in &self.moved {
            out.push_str(&format!("moved {old} {new}\n"));
        }
        out
    }

    /// Read a file's text. A line of a kind this build does not know is skipped, so a later
    /// node service can say more.
    ///
    /// # Errors
    /// A head of another version, no `me`, or a line of a known kind whose nodes are not nodes.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text.lines();
        if lines.next() != Some(HANDOFF_HEAD) {
            return Err(format!("not a hand-off (`{HANDOFF_HEAD}` first)"));
        }
        let mut out = Self::default();
        for line in lines {
            let words: Vec<&str> = line.split(' ').collect();
            let bad = || {
                Err(format!(
                    "a malformed line: {}",
                    crate::render::safe(line, 80)
                ))
            };
            match words.as_slice() {
                ["me", node] if is_node(node) => (*node).clone_into(&mut out.me),
                ["member", node] if is_node(node) => {
                    out.members.insert((*node).to_owned());
                }
                ["moved", old, new] if is_node(old) && is_node(new) && old != new => {
                    out.moved.insert((*old).to_owned(), (*new).to_owned());
                }
                ["me" | "member" | "moved", ..] => return bad(),
                _ => {}
            }
        }
        if out.me.is_empty() {
            return Err("names no `me`".to_owned());
        }
        Ok(out)
    }

    /// Read it from `dir`: `None` while there is none.
    ///
    /// # Errors
    /// A file that is not private, cannot be read, or does not parse.
    pub fn read(dir: &Path) -> io::Result<Option<Self>> {
        match aterm_uds::private_file::read_private(dir, HANDOFF_FILE, HANDOFF_MAX) {
            Ok(bytes) => {
                let text = String::from_utf8(bytes).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "a hand-off that is not text")
                })?;
                Self::parse(&text)
                    .map(Some)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
            Err(aterm_uds::private_file::PrivateFileError::Missing) => Ok(None),
            Err(e) => Err(io::Error::other(format!(
                "{}: {e}",
                dir.join(HANDOFF_FILE).display()
            ))),
        }
    }

    /// Write it to `dir`, atomically, mode 0600.
    ///
    /// # Errors
    /// A folder that is not private, or a write that fails.
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        aterm_uds::private_file::write_private(dir, HANDOFF_FILE, self.render().as_bytes())
    }

    /// The current member `node` names: itself when it is one, or the member its seat holds now
    /// when a key rotation retired it, followed through each later rotation. `None` for a node
    /// that is neither.
    #[must_use]
    pub fn current(&self, node: &str) -> Option<&str> {
        let mut at = self.members.get(node).map(String::as_str);
        let mut seen = node;
        for _ in 0..=self.moved.len() {
            if at.is_some() {
                return at;
            }
            let next = self.moved.get(seen)?;
            seen = next;
            at = self.members.get(next).map(String::as_str);
        }
        at
    }
}

/// One push [`Routes`] runs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lane {
    /// The mail this Mac's sessions wrote to `dst`'s sessions.
    Mail {
        /// The addressed Mac.
        dst: String,
    },
    /// The mail this Mac holds for `old`, which a key rotation retired, carried to `new`, the
    /// node its seat holds now, under `new`'s subjects.
    Move {
        /// The retired node.
        old: String,
        /// The member it became.
        new: String,
    },
    /// This Mac's topic broadcasts, to a Mac that holds a topic.
    Say {
        /// The Mac that holds one.
        dst: String,
    },
}

impl Lane {
    /// The Mac whose link carries it.
    #[must_use]
    pub fn to(&self) -> &str {
        match self {
            Self::Mail { dst } | Self::Say { dst } => dst,
            Self::Move { new, .. } => new,
        }
    }

    /// The records it carries from this Mac (`me`). With `forward_all` (the
    /// [`FORWARD_ALL`] negative control) a mail push carries every lane this Mac wrote,
    /// whatever node it names.
    #[must_use]
    pub fn filter(&self, me: &str, forward_all: bool) -> String {
        match self {
            Self::Mail { .. } if forward_all => written_lanes(me),
            Self::Mail { dst } => mail_lanes(dst, me),
            Self::Move { old, .. } => mail_lanes(old, me),
            Self::Say { .. } => say_lanes(me),
        }
    }

    /// The group it follows under on this Mac (`me`).
    #[must_use]
    pub fn group(&self, me: &str) -> String {
        match self {
            Self::Mail { dst } => mail_group(dst),
            Self::Move { old, .. } => mail_group(old),
            Self::Say { dst } => say_group(me, dst),
        }
    }

    /// How its subjects read at the far end.
    ///
    /// # Errors
    /// A rewrite astream refuses (never, for nodes [`is_node`] takes).
    pub fn rewrite(&self) -> Result<SubjectRewrite, String> {
        match self {
            Self::Mail { .. } | Self::Say { .. } => Ok(SubjectRewrite::passthrough()),
            Self::Move { old, new } => SubjectRewrite::prefix_map(
                &format!("/f/{FLEET}/in/{old}"),
                &format!("/f/{FLEET}/in/{new}"),
            ),
        }
    }

    /// Its word in a journal line.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Mail { dst } => format!("mail to {dst}"),
            Self::Move { old, new } => format!("mail held for {old}, moved to {new}"),
            Self::Say { dst } => format!("topics to {dst}"),
        }
    }
}

/// The pushes this Mac (`me`) runs: one per node this Mac's sessions wrote to (`written`) that is
/// a current member, one per retired node it wrote to whose seat holds a current member now, and
/// one per member that holds a topic (`holders`). With `forward_all` ([`FORWARD_ALL`]) every
/// member gets a mail push, written to or not.
#[must_use]
pub fn plan(
    me: &str,
    handoff: &Handoff,
    holders: &BTreeSet<String>,
    written: &BTreeSet<String>,
    forward_all: bool,
) -> BTreeSet<Lane> {
    let mut lanes = BTreeSet::new();
    for dst in written {
        if dst == me {
            continue;
        }
        if handoff.members.contains(dst) {
            lanes.insert(Lane::Mail { dst: dst.clone() });
        } else if let Some(new) = handoff.current(dst).filter(|new| *new != me) {
            lanes.insert(Lane::Move {
                old: dst.clone(),
                new: new.to_owned(),
            });
        }
    }
    if forward_all {
        for dst in handoff.members.iter().filter(|m| *m != me) {
            lanes.insert(Lane::Mail { dst: dst.clone() });
        }
    }
    for dst in holders {
        if dst != me && handoff.members.contains(dst) {
            lanes.insert(Lane::Say { dst: dst.clone() });
        }
    }
    lanes
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the mail route (`forward-all`): [`Routes`] started on a thread
    /// where it is set drops each mail push's destination filter, so it pushes every lane this Mac
    /// wrote to every member, written to or not. A lab instance turns it on for its whole process
    /// with `forward-all` in `ATERM_TEST_NEGATIVE_CONTROL`.
    pub static FORWARD_ALL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the held mail (`another-group`): [`Routes`] started on a thread
    /// where it is set follows each mail push under a group of its own, not the one the mail rule
    /// names the lane's reader by, so a carried record's hold is never released on the sender.
    pub static ANOTHER_GROUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The negative control of the person carry (`unsealed-carry`): [`Routes`] started on a
    /// thread where it is set carries every person's post to a member without checking its seal,
    /// so the far end's bridge is the only check left.
    pub static UNSEALED_CARRY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether [`UNSEALED_CARRY`] is on: set on this thread, or named for this process.
fn unsealed_carry() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        UNSEALED_CARRY.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("unsealed-carry")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether [`FORWARD_ALL`] is on: set on this thread, or named for this process.
fn forward_all() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        FORWARD_ALL.with(std::cell::Cell::get) || aterm_types::negative_control::on("forward-all")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether [`ANOTHER_GROUP`] is on: set on this thread, or named for this process.
fn another_group() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        ANOTHER_GROUP.with(std::cell::Cell::get)
            || aterm_types::negative_control::on("another-group")
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Where a push's records go: streams to the Mac's `bus.log`, and the grants its ring handed
/// this Mac there.
pub struct Dest {
    /// Opens one stream to the Mac's `bus.log`.
    pub dial: Arc<dyn StreamDial>,
    /// The ring the Mac serves this one, presented on every stream.
    pub grants: Vec<Capability>,
}

/// Opens a [`Dest`] to a node, by its node id.
pub type DestFn = Arc<dyn Fn(&str) -> io::Result<Dest> + Send + Sync>;

/// Mints a grant on this Mac's `bus.log` under its mint secret.
pub type MintFn = Arc<dyn Fn(&str) -> io::Result<Capability> + Send + Sync>;

/// Where [`Routes`] says what it did, beside the node service's journal.
pub type NoteFn = Arc<dyn Fn(&str) + Send + Sync>;

/// What [`Routes`] runs on.
pub struct RoutesConfig {
    /// This Mac's node.
    pub me: String,
    /// This Mac's node key, which vouches for its person key: the person carry verifies a
    /// person's post under it.
    pub key: [u8; 32],
    /// Streams to this Mac's own `bus.log`, adopted under its node key.
    pub source: Arc<dyn StreamDial>,
    /// Mints the grants each push and the watch read this Mac's `bus.log` with.
    pub mint: MintFn,
    /// Opens the link to a node's `bus.log`.
    pub dest: DestFn,
    /// The journal.
    pub note: NoteFn,
}

/// What one push did, for status and tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RouteStatus {
    /// It follows its group now, its records going out as they land.
    pub following: bool,
    /// The records it appended at the far end.
    pub appended: u64,
    /// The records the far end held already (a resend after a lost acknowledgement).
    pub deduped: u64,
    /// Why its last try ended, if one did.
    pub last_error: Option<String>,
}

/// What the person carry did, for status and tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CarryStatus {
    /// It follows its group now.
    pub following: bool,
    /// The posts it carried: each written once, a retry after a lost answer counted once.
    pub carried: u64,
    /// The posts it did not carry, each said in the journal and in the person's inbox.
    pub refused: u64,
    /// Why it did not carry the last post it refused.
    pub last_refusal: Option<String>,
    /// Why its last try ended, if one did.
    pub last_error: Option<String>,
}

/// The pushes this Mac runs over the mesh's links, the watch on the lanes it writes that says
/// which, and the person carry. Dropped, every push stops and is joined.
pub struct Routes {
    shared: Arc<Shared>,
}

struct Shared {
    cfg: RoutesConfig,
    forward_all: bool,
    another_group: bool,
    unsealed_carry: bool,
    state: Mutex<State>,
    bell: Condvar,
}

#[derive(Default)]
struct State {
    stopped: bool,
    handoff: Handoff,
    holders: BTreeSet<String>,
    written: BTreeSet<String>,
    running: BTreeMap<Lane, Running>,
    ending: Vec<JoinHandle<()>>,
    watcher: Option<JoinHandle<()>>,
    watch_closer: Option<Closer>,
    carry: CarryStatus,
    carrier: Option<JoinHandle<()>>,
    carry_closers: Vec<Closer>,
    /// Counts the kicks: a push that failed waits for its back-off or the next kick.
    kicks: u64,
}

struct Running {
    stop: Arc<AtomicBool>,
    closer: Option<FederationBridgeCloser>,
    handle: Option<JoinHandle<()>>,
    status: RouteStatus,
}

impl Routes {
    /// Start the watch over the lanes this Mac writes and the person carry, with no member yet:
    /// no push runs, and the carry reads nothing, until [`Routes::set`] names some.
    /// [`FORWARD_ALL`], [`ANOTHER_GROUP`] and [`UNSEALED_CARRY`] are read on the calling thread.
    ///
    /// # Errors
    /// Where the watch's or the carry's thread cannot start.
    pub fn start(cfg: RoutesConfig) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            cfg,
            forward_all: forward_all(),
            another_group: another_group(),
            unsealed_carry: unsealed_carry(),
            state: Mutex::new(State::default()),
            bell: Condvar::new(),
        });
        let routes = Self {
            shared: Arc::clone(&shared),
        };
        let watching = Arc::clone(&shared);
        let watcher = std::thread::Builder::new()
            .name("aterm-link-mail-watch".to_owned())
            .spawn(move || watching.watch())?;
        shared.state.lock_or_recover().watcher = Some(watcher);
        let carrying = Arc::clone(&shared);
        let carrier = std::thread::Builder::new()
            .name("aterm-link-person-carry".to_owned())
            .spawn(move || carrying.carry())?;
        shared.state.lock_or_recover().carrier = Some(carrier);
        Ok(routes)
    }

    /// Route by `handoff` (the members, and each rotated node's successor) and `holders` (the
    /// members that hold a topic): pushes no longer planned stop, new ones start, and the person
    /// carry carries to the members it names.
    pub fn set(&self, handoff: &Handoff, holders: &BTreeSet<String>) {
        let mut state = self.shared.state.lock_or_recover();
        handoff.clone_into(&mut state.handoff);
        holders.clone_into(&mut state.holders);
        self.shared.reconcile(&mut state);
        drop(state);
        // The carry waits for its first members.
        self.shared.bell.notify_all();
    }

    /// What the person carry did.
    #[must_use]
    pub fn carry_status(&self) -> CarryStatus {
        self.shared.state.lock_or_recover().carry.clone()
    }

    /// A link came up: every push waiting out its back-off tries again now.
    pub fn kick(&self) {
        let mut state = self.shared.state.lock_or_recover();
        state.kicks = state.kicks.wrapping_add(1);
        drop(state);
        self.shared.bell.notify_all();
    }

    /// Each push running now, and what it did.
    #[must_use]
    pub fn status(&self) -> BTreeMap<Lane, RouteStatus> {
        self.shared
            .state
            .lock_or_recover()
            .running
            .iter()
            .map(|(lane, running)| (lane.clone(), running.status.clone()))
            .collect()
    }

    /// The nodes the watch saw this Mac write to.
    #[must_use]
    pub fn written(&self) -> BTreeSet<String> {
        self.shared.state.lock_or_recover().written.clone()
    }

    /// Ask every push and the watch to stop, without waiting: each followed group's bridge and
    /// the watch's stream close now; a push still opening ends once its dial answers, which the
    /// caller hastens by closing the links it dials over. [`Routes::stop`] joins them.
    pub fn stop_soon(&self) {
        let mut state = self.shared.state.lock_or_recover();
        state.stopped = true;
        if let Some(closer) = state.watch_closer.take() {
            closer.close();
        }
        for closer in state.carry_closers.drain(..) {
            closer.close();
        }
        for running in state.running.values() {
            running.stop.store(true, Ordering::Release);
            if let Some(closer) = &running.closer {
                closer.close();
            }
        }
        drop(state);
        self.shared.bell.notify_all();
    }

    /// Stop every push and the watch, and join them.
    pub fn stop(&self) {
        self.stop_soon();
        let mut state = self.shared.state.lock_or_recover();
        let mut handles: Vec<JoinHandle<()>> = std::mem::take(&mut state.ending);
        for running in state.running.values_mut() {
            handles.extend(running.handle.take());
        }
        state.running.clear();
        handles.extend(state.watcher.take());
        handles.extend(state.carrier.take());
        drop(state);
        for handle in handles {
            let _ = handle.join();
        }
    }
}

impl Drop for Routes {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Shared {
    fn note(&self, line: &str) {
        (self.cfg.note)(line);
    }

    /// Start each planned push that is not running and stop each running one that is not
    /// planned, joining none: a stopped push's thread is joined at the next reconcile once it
    /// ended, or at [`Routes::stop`].
    fn reconcile(self: &Arc<Self>, state: &mut State) {
        if state.stopped {
            return;
        }
        let planned = plan(
            &self.cfg.me,
            &state.handoff,
            &state.holders,
            &state.written,
            self.forward_all,
        );
        let gone: Vec<Lane> = state
            .running
            .keys()
            .filter(|lane| !planned.contains(*lane))
            .cloned()
            .collect();
        for lane in gone {
            if let Some(mut running) = state.running.remove(&lane) {
                running.stop.store(true, Ordering::Release);
                if let Some(closer) = &running.closer {
                    closer.close();
                }
                state.ending.extend(running.handle.take());
                self.note(&format!("mesh: stopped the push of {}", lane.describe()));
                // A push waiting out its back-off hears its stop now.
                self.bell.notify_all();
            }
        }
        let mut ended = Vec::new();
        for handle in std::mem::take(&mut state.ending) {
            if handle.is_finished() {
                ended.push(handle);
            } else {
                state.ending.push(handle);
            }
        }
        for handle in ended {
            let _ = handle.join();
        }
        let wanted: Vec<Lane> = planned
            .into_iter()
            .filter(|lane| !state.running.contains_key(lane))
            .collect();
        for lane in wanted {
            let stop = Arc::new(AtomicBool::new(false));
            let pushing = Arc::clone(self);
            let (for_thread, stop_for_thread) = (lane.clone(), Arc::clone(&stop));
            let handle = std::thread::Builder::new()
                .name("aterm-link-mail-push".to_owned())
                .spawn(move || pushing.push(&for_thread, &stop_for_thread));
            match handle {
                Ok(handle) => {
                    state.running.insert(
                        lane,
                        Running {
                            stop,
                            closer: None,
                            handle: Some(handle),
                            status: RouteStatus::default(),
                        },
                    );
                }
                Err(e) => self.note(&format!(
                    "mesh: the push of {} could not start ({e}); the next change tries again",
                    lane.describe()
                )),
            }
        }
    }

    fn stopping(&self, stop: &AtomicBool) -> bool {
        stop.load(Ordering::Acquire) || self.state.lock_or_recover().stopped
    }

    /// Wait out `wait` after a failure, or less: until a kick after `kicks`, or a stop.
    fn back_off(&self, stop: &AtomicBool, kicks: u64, wait: Duration) {
        let state = self.state.lock_or_recover();
        let _ = self
            .bell
            .wait_timeout_while(state, wait, |state| {
                !state.stopped && !stop.load(Ordering::Acquire) && state.kicks == kicks
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }

    fn kicks(&self) -> u64 {
        self.state.lock_or_recover().kicks
    }

    fn with_status(&self, lane: &Lane, f: impl FnOnce(&mut Running)) {
        if let Some(running) = self.state.lock_or_recover().running.get_mut(lane) {
            f(running);
        }
    }

    /// Run `lane`'s push until it is stopped: open it, follow its group until the link or the
    /// source ends, then back off and open it again. Each open resumes where its group's commit
    /// stands, and the far end's dedup takes a resent record once.
    fn push(self: Arc<Self>, lane: &Lane, stop: &AtomicBool) {
        let mut wait = BACKOFF_FIRST;
        let mut told: Option<String> = None;
        loop {
            if self.stopping(stop) {
                return;
            }
            let kicks = self.kicks();
            let ended = match self.open(lane) {
                Ok(mut bridge) => {
                    let mut state = self.state.lock_or_recover();
                    if state.stopped || stop.load(Ordering::Acquire) {
                        return;
                    }
                    if let Some(running) = state.running.get_mut(lane) {
                        running.closer = Some(bridge.closer());
                        running.status.following = true;
                        running.status.last_error = None;
                    }
                    drop(state);
                    told = None;
                    wait = BACKOFF_FIRST;
                    self.note(&format!("mesh: pushes {}", lane.describe()));
                    let ran = bridge.run(BURST, &mut |report: &FederationReport| {
                        let appended = report.mirrored.saturating_sub(report.deduped);
                        self.with_status(lane, |running| {
                            running.status.appended += appended as u64;
                            running.status.deduped += report.deduped as u64;
                        });
                    });
                    self.with_status(lane, |running| {
                        running.closer = None;
                        running.status.following = false;
                    });
                    match ran {
                        Ok(()) => return,
                        Err(e) => io_of(e).to_string(),
                    }
                }
                Err(e) => e.to_string(),
            };
            if self.stopping(stop) {
                return;
            }
            self.with_status(lane, |running| {
                running.status.last_error = Some(ended.clone());
            });
            if told.as_deref() != Some(ended.as_str()) {
                self.note(&format!(
                    "mesh: the push of {} ended ({ended}); it tries again within {}s, at once \
                     once a link comes up",
                    lane.describe(),
                    wait.as_secs()
                ));
                told = Some(ended);
            }
            self.back_off(stop, kicks, wait);
            wait = (wait * 2).min(BACKOFF_MAX);
        }
    }

    /// Open `lane`'s push: this Mac's `bus.log` under the grants it needs, to the far end's
    /// over its link, in mirror mode under this Mac's producer id.
    fn open(&self, lane: &Lane) -> io::Result<FederationBridge> {
        let me = &self.cfg.me;
        let filter = lane.filter(me, self.forward_all);
        let group = match lane {
            Lane::Mail { dst } if self.another_group => format!("/f/{FLEET}/cur/{me}/push/{dst}"),
            _ => lane.group(me),
        };
        let dest = (self.cfg.dest)(lane.to())?;
        let caps = vec![
            (self.cfg.mint)(&format!("ro:{filter}"))?,
            (self.cfg.mint)(&format!("rw,p={me}:{group}"))?,
        ];
        let rewrite = lane
            .rewrite()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        FederationBridge::open(FederationConfig {
            source: FederationEndpoint::stream(me, Arc::clone(&self.cfg.source)).with_caps(caps),
            dest: FederationEndpoint::stream(lane.to(), dest.dial).with_caps(dest.grants),
            group,
            filter,
            producer_id: astream_cap::producer_id_of(me),
            rewrite,
            relay: false,
        })
        .map_err(io_of)
    }

    /// Watch the lanes this Mac writes until stopped, noting each node they name, reconnecting
    /// after each failure.
    fn watch(self: Arc<Self>) {
        let none = AtomicBool::new(false);
        let mut wait = BACKOFF_FIRST;
        let mut told: Option<String> = None;
        loop {
            if self.stopping(&none) {
                return;
            }
            let kicks = self.kicks();
            let why = match self.watch_once() {
                Ok(()) => return,
                Err(e) => e.to_string(),
            };
            if self.stopping(&none) {
                return;
            }
            if told.as_deref() != Some(why.as_str()) {
                self.note(&format!(
                    "mesh: the watch on this Mac's mail lanes ended ({why}); it tries again \
                     within {}s",
                    wait.as_secs()
                ));
                told = Some(why);
            }
            self.back_off(&none, kicks, wait);
            wait = (wait * 2).min(BACKOFF_MAX);
        }
    }

    /// One watch: every node a lane this Mac wrote names, then each new one as it is written.
    /// `Ok` once stopped.
    fn watch_once(self: &Arc<Self>) -> io::Result<()> {
        let filter = written_lanes(&self.cfg.me);
        let cap = (self.cfg.mint)(&format!("ro:{filter}"))?;
        let (mut client, closer) = astream_broker::connect_stream(self.cfg.source.dial()?);
        {
            let mut state = self.state.lock_or_recover();
            if state.stopped {
                return Ok(());
            }
            state.watch_closer = Some(closer);
        }
        client.attach_all([(cap.filter.as_str(), &cap.tag[..])])?;
        let (rows, (next, _)) = client.last_all(&filter)?;
        self.saw(rows.iter().map(|(_, subject, _)| subject.as_str()));
        let mut sub = client.subscribe(next, &filter)?;
        loop {
            match sub.recv_event() {
                Ok(Some(Event::Delivery { subject, .. })) => self.saw([subject.as_str()]),
                Ok(Some(_)) => {}
                Ok(None) => {
                    return if self.state.lock_or_recover().stopped {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "the broker ended the watch",
                        ))
                    };
                }
                Err(_) if self.state.lock_or_recover().stopped => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    /// Note the node each of `subjects` is addressed to, and start what that plans.
    fn saw<'a>(self: &Arc<Self>, subjects: impl IntoIterator<Item = &'a str>) {
        let me = &self.cfg.me;
        let mut state = self.state.lock_or_recover();
        let mut grew = false;
        for subject in subjects {
            if let Some(dst) = addressee(subject, me) {
                grew |= state.written.insert(dst.to_owned());
            }
        }
        if grew {
            self.reconcile(&mut state);
        }
    }

    fn with_carry(&self, f: impl FnOnce(&mut CarryStatus)) {
        f(&mut self.state.lock_or_recover().carry);
    }

    /// Run the person carry until stopped (§11 C3c): once [`Routes::set`] named the members, follow
    /// [`carry_group`] over every post on this Mac's `bus.log`, reconnecting after each failure.
    fn carry(self: Arc<Self>) {
        let none = AtomicBool::new(false);
        let mut wait = BACKOFF_FIRST;
        let mut told: Option<String> = None;
        loop {
            {
                let state = self.state.lock_or_recover();
                let state = self
                    .bell
                    .wait_while(state, |state| !state.stopped && state.handoff.me.is_empty())
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.stopped {
                    return;
                }
            }
            let kicks = self.kicks();
            let mut followed = false;
            let why = match self.carry_once(&mut followed) {
                Ok(()) => return,
                Err(e) => e.to_string(),
            };
            if self.stopping(&none) {
                return;
            }
            // A run that reached its tail starts the back-off, and what the journal was told, over.
            if followed {
                wait = BACKOFF_FIRST;
                told = None;
            }
            self.with_carry(|carry| carry.last_error = Some(why.clone()));
            if told.as_deref() != Some(why.as_str()) {
                self.note(&format!(
                    "mesh: the person carry ended ({why}); it tries again within {}s",
                    wait.as_secs()
                ));
                told = Some(why);
            }
            self.back_off(&none, kicks, wait);
            wait = (wait * 2).min(BACKOFF_MAX);
        }
    }

    /// One run of the carry: its group's records from where it committed, then each new one as it
    /// lands. A record that is no person's post for another Mac is passed over, and committed past
    /// once the tail is caught up. `followed` once it follows its group. `Ok` once stopped.
    fn carry_once(self: &Arc<Self>, followed: &mut bool) -> io::Result<()> {
        let me = self.cfg.me.clone();
        let (filter, group) = (posts(), carry_group(&me));
        let grants = [
            format!("ro:{filter}"),
            format!("rw,p={CARRIER}:{group}"),
            format!("rw,p={CARRIER}:{}", written_lanes(&me)),
            format!("rw,p={CARRIER}:/f/{FLEET}/in/p/*/{me}/*"),
        ];
        let caps = grants
            .iter()
            .map(|grant| (self.cfg.mint)(grant))
            .collect::<io::Result<Vec<Capability>>>()?;
        let (mut writer, writer_closer) = astream_broker::connect_stream(self.cfg.source.dial()?);
        let (mut reader, reader_closer) = astream_broker::connect_stream(self.cfg.source.dial()?);
        {
            let mut state = self.state.lock_or_recover();
            if state.stopped {
                return Ok(());
            }
            state.carry_closers = vec![writer_closer, reader_closer];
        }
        for client in [&mut writer, &mut reader] {
            client.attach_all(caps.iter().map(|cap| (cap.filter.as_str(), &cap.tag[..])))?;
        }
        let mut sub = reader.subscribe_group(&group, &filter)?;
        *followed = true;
        self.with_carry(|carry| {
            carry.following = true;
            carry.last_error = None;
        });
        let stopped = || self.state.lock_or_recover().stopped;
        let mut passed: Option<u64> = None;
        let ended = loop {
            match sub.recv_event() {
                Ok(Some(Event::Delivery {
                    offset,
                    subject,
                    body,
                })) => match self.carry_one(&mut writer, &group, offset, &subject, &body) {
                    Ok(true) => passed = None,
                    Ok(false) => passed = Some(offset),
                    Err(e) => break Err(e),
                },
                Ok(Some(Event::Mark { .. })) => {
                    if let Some(through) = passed.take() {
                        if let Err(e) = writer.commit(&group, through) {
                            break Err(e);
                        }
                    }
                }
                Ok(Some(Event::Gap(gap))) => {
                    // A group with no commit starts at the log's oldest record: nothing below it
                    // was this carry's to read.
                    if gap.from > 0 {
                        self.note(&format!(
                            "mesh: the person carry resumed at {}: retention gave up the posts \
                             below it ({}) before the carry read them",
                            gap.to,
                            gap.records.map_or_else(
                                || "how many is not known".to_owned(),
                                |n| n.to_string()
                            )
                        ));
                    }
                    passed = gap.to.checked_sub(1).max(passed);
                }
                Ok(None) if stopped() => break Ok(()),
                Ok(None) => {
                    break Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the broker ended the carry's tail",
                    ))
                }
                Err(_) if stopped() => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        self.with_carry(|carry| carry.following = false);
        ended
    }

    /// The carry of the record at `off` (§11 C3c), when it is a person's post for another Mac:
    /// carried onto this Mac's own lane to that Mac's session, naming the person
    /// ([`person_key::BY_TOKEN`]), when that Mac is a current member, or the member a key
    /// rotation made of it ([`Handoff::current`]), and the post's seal verifies under this Mac's
    /// node key; else a notice on the person's lane here says why it was not
    /// ([`NOTICE_REASON`]), and the journal too, as it does for a post this Mac's broker refuses to
    /// carry for good. Either is one write that also commits the carry's group through `off`,
    /// under [`CARRIER`] numbered by `off`, so a retry after a lost answer, or a carry that a node
    /// key's rotation started over, writes and says nothing again. The lane a post was carried on
    /// is given back at once ([`Shared::give_back`]). `false` for any other record, with nothing
    /// written.
    fn carry_one(
        &self,
        writer: &mut astream_broker::AnyClient,
        group: &str,
        off: u64,
        subject: &str,
        raw: &[u8],
    ) -> io::Result<bool> {
        let me = &self.cfg.me;
        let Some((dst, sid, person, kind)) = person_post(subject, me) else {
            return Ok(false);
        };
        let (body, _) = Body::decode(raw);
        let now = crate::now_ms();
        let member = self
            .state
            .lock_or_recover()
            .handoff
            .current(dst)
            .filter(|member| member != me)
            .map(str::to_owned);
        let checked = match member {
            None => Err(Uncarried::NotMember),
            Some(member) if self.unsealed_carry => Ok(member),
            Some(member) => {
                let address = person_key::post_address(FLEET, sid, person, kind);
                let signer = person_key::Signer::Key(&self.cfg.key);
                person_key::verify(signer, &address, kind, &body, now)
                    .map(|_| member)
                    .map_err(Uncarried::Seal)
            }
        };
        let pid = astream_cap::producer_id_of(CARRIER);
        let why = match checked {
            Ok(member) => {
                let mut carried = body;
                carried
                    .unknown
                    .insert(person_key::BY_TOKEN.to_owned(), person.to_owned());
                let lane = subject::post_lane(FLEET, &member, sid, me, kind);
                match writer.process_and_produce(pid, off, &lane, &carried.encode(), group, off) {
                    Ok((at, deduped)) => {
                        if !deduped {
                            self.with_carry(|carry| carry.carried += 1);
                            Self::give_back(writer, &lane, at)?;
                        }
                        return Ok(true);
                    }
                    Err(e) if refused_for_good(&e) => Uncarried::Refused(e.to_string()),
                    Err(e) => return Err(e),
                }
            }
            Err(why) => why,
        };
        let to = format!("@{sid}@{dst}");
        let words = why.describe(dst);
        let mut notice = Body::new(now);
        notice.re = Some(off);
        notice.text = format!("not carried to {to}: {words}");
        notice
            .unknown
            .insert(NOTICE_REASON.to_owned(), why.token().to_owned());
        let lane = format!("{}/undeliverable", subject::person_lane(FLEET, person, me));
        let first = match writer.process_and_produce(pid, off, &lane, &notice.encode(), group, off)
        {
            Ok((_, deduped)) => !deduped,
            Err(e) if refused_for_good(&e) => {
                writer.commit(group, off)?;
                self.note(&format!(
                    "mesh: the notice that {person}'s {kind} at {off} was not carried was \
                     refused too ({e})"
                ));
                true
            }
            Err(e) => return Err(e),
        };
        if first {
            self.note(&format!(
                "mesh: did not carry {person}'s {kind} at {off} to {to}: {words}"
            ));
            self.with_carry(|carry| {
                carry.refused += 1;
                carry.last_refusal = Some(words);
            });
        }
        Ok(true)
    }

    /// Give back the charge the carry's write at `at` put on `lane`, so the carry's producer holds
    /// no subject for the posts it carried (astream's distinct-subject budget, §5.2): the record
    /// stays in the log for the push, and only the lane's last-value entry goes, unless a newer
    /// record replaced it. A log that retention cut, a lane another producer opened first, and a
    /// broker without the verb keep the charge, as they keep the node's own.
    ///
    /// # Errors
    /// The connection failed.
    fn give_back(writer: &mut astream_broker::AnyClient, lane: &str, at: u64) -> io::Result<()> {
        let Some(log) = writer.seen_log_identity() else {
            return Ok(());
        };
        match writer.retire(astream_cap::producer_id_of(CARRIER), lane, at, log) {
            Err(e) if astream_broker::broker_refusal(&e).is_none() => Err(e),
            _ => Ok(()),
        }
    }
}

/// The body token that names why the person carry did not carry a post, in the notice it writes
/// on the person's lane (§11 C3c).
pub const NOTICE_REASON: &str = "reason";

/// Why the person carry did not carry a post (§11 C3c).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Uncarried {
    /// The Mac it is for is no current member of the fleet.
    NotMember,
    /// Its seal does not verify under this Mac's node key.
    Seal(person_key::Refusal),
    /// This Mac's broker refused the carried post for good, in these words.
    Refused(String),
}

impl Uncarried {
    /// Its [`NOTICE_REASON`] word.
    const fn token(&self) -> &'static str {
        match self {
            Self::NotMember => "fleet-not-member",
            Self::Seal(refusal) => refusal.token(),
            Self::Refused(_) => "carry-refused",
        }
    }

    /// Why, in words, of a post for `dst`.
    fn describe(&self, dst: &str) -> String {
        match self {
            Self::NotMember => format!("{dst} is no current member of the fleet"),
            Self::Seal(refusal) => refusal.describe().to_owned(),
            Self::Refused(why) => format!("this Mac's broker refused to carry it ({why})"),
        }
    }
}

/// Whether this Mac's broker refused a write of the person carry for good: sent again, the same
/// write is refused again. A refusal astream calls permanent, but for a grant that expired, which
/// the carry's next run mints again; and the carry's producer at its distinct-subject budget, which
/// the carry, giving back what it charges, cannot lower by waiting.
fn refused_for_good(e: &io::Error) -> bool {
    astream_broker::broker_refusal(e).is_some_and(|refusal| {
        (refusal.is_permanent() && !refusal.is_expired()) || refusal.is_subject_bound()
    })
}

fn io_of(error: FederationError) -> io::Error {
    match error {
        FederationError::Io(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

#[cfg(test)]
#[path = "federate_tests.rs"]
pub(crate) mod tests;
