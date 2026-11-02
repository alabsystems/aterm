// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `vault-join` ([`FleetOp::VaultJoin`]): a Mac that is ALab's already but whose vault key changed
//! proves itself to a member over the fleet's own channel and is let back into the vault with
//! nobody's Allow (`aterm_secrets::client::readmit`,
//! `docs/DESIGN-automatic-alab-lan-admission-2026-10-29.md` "Re-admission by proof").
//!
//! The channel (`atf/2`) proved the fleet secret the listener holds and the dialer's node key,
//! bound to this connection's TLS exporter, so the proof is fresh. On it the member's desk then
//! runs the same `prove` exchange its own port serves ([`aterm_secrets::members::desk::serve_with`]),
//! which must name the node key the channel proved. Here the listener judges what the channel is
//! worth ([`judge`]): the fleet's key counts only where the secret it proved is the vault's
//! current fleet code (a removal rotates it, so a removed Mac's copy proves nothing) and this
//! Mac's own drive admission takes the node, so the vault is never easier to enter than the fleet.
//! The joining Mac's side is [`prove_over_fleet`].
//! A new Mac with no fleet secret uses [`AutomaticJoins`] on the node job's anonymous `ask`
//! channel: its signed request reaches the existing vault queue without opening a window.

use aterm_net::fleet::{FleetMark, FleetOp, FleetSecret, NodeIdentity, NodeKey, NodeProof};
use aterm_net::fleet_member::{MemberKeys, Peer};
use aterm_secrets::client::{self, Book, Paths};
use aterm_secrets::members::desk::{self, Desk, Local};
use aterm_secrets::members::join::{self, Dial, Stream};
use aterm_secrets::members::joiner::Joining;
use aterm_types::MutexExt as _;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod automatic;
pub use automatic::{AutomaticJoinHandle, AutomaticJoins};

/// What a member's listener made of a `vault-join` channel ([`judge`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Judged {
    /// The node key the channel proved.
    pub node: [u8; 32],
    /// The channel proved the vault's current fleet key too, and this Mac's drive admission takes
    /// the node: the desk's proof counts as the fleet's (`Proving::fleet`).
    pub fleet: bool,
    /// What this Mac's fleet book says of the node and of the fleet ([`book_word`]), with the
    /// node id the channel proved.
    pub book: Book,
}

/// The first aterm whose membership evaluator reads a seat moved by its node key as the same Mac
/// back (`(major, minor)`; the rule landed after v0.147.0, in 0.148.0).
pub const READS_MOVED_SEATS: (u64, u64) = (0, 148);

/// How long after the book last heard a member it still counts for [`moved_seats_read`]: two
/// weeks, past which its updater has had its chance.
pub const HEARD_WITHIN_MS: u64 = 14 * 24 * 60 * 60 * 1000;

/// Whether `version` (a book entry's, `0.148.0` or `0.148.0+g<sha>`) is [`READS_MOVED_SEATS`] or
/// later; a version this cannot read is not.
fn reads_moved_seats(version: &str) -> bool {
    let mut parts = version.split(['.', '+', '-']);
    let mut number = || parts.next().and_then(|part| part.parse::<u64>().ok());
    match (number(), number()) {
        (Some(major), Some(minor)) => (major, minor) >= READS_MOVED_SEATS,
        _ => false,
    }
}

/// Whether every member of the book `peers` heard within [`HEARD_WITHIN_MS`] of `now_ms` runs
/// [`READS_MOVED_SEATS`] or later ([`Book::moved_seats_read`]); an entry that names no version is
/// an older aterm's. A peer a person named (no node id) is not a member the book heard.
#[must_use]
pub fn moved_seats_read(peers: &[Peer], now_ms: u64) -> bool {
    peers
        .iter()
        .filter(|peer| {
            peer.node.is_some()
                && peer
                    .seen_ms
                    .is_some_and(|seen| now_ms.saturating_sub(seen) < HEARD_WITHIN_MS)
        })
        .all(|peer| peer.version.as_deref().is_some_and(reads_moved_seats))
}

/// Whether the book `peers` heard `node` within its live window
/// ([`aterm_net::fleet_member::LIVE_WITHIN_MS`]) of `now_ms` from an address other than `from`,
/// the one the proof came from (`None`: not known, so any address is another):
/// [`Book::node_elsewhere`].
#[must_use]
pub fn node_elsewhere(peers: &[Peer], node: &[u8; 32], from: Option<IpAddr>, now_ms: u64) -> bool {
    peers.iter().any(|peer| {
        peer.key.is_some_and(|key| key.bytes() == node)
            && peer.is_live_at(now_ms)
            && peer.addr.is_some_and(|addr| {
                from.is_none_or(|from| addr.to_canonical() != from.to_canonical())
            })
    })
}

/// What the fleet book `peers` says of a Mac proving `node` from `from` at `now_ms`, with no
/// node id (the desk's own port names none): [`Book`].
#[must_use]
pub fn book_word(peers: &[Peer], node: &[u8; 32], from: Option<IpAddr>, now_ms: u64) -> Book {
    Book {
        node_id_held_out: false,
        node_id_seated: false,
        node_elsewhere: node_elsewhere(peers, node, from, now_ms),
        moved_seats_read: moved_seats_read(peers, now_ms),
    }
}

/// [`book_word`] of a book read as `peers`: where it could not be read, the book that lets nothing
/// move ([`Book::default`]), since an empty list would read as a fleet with no older member.
#[must_use]
pub fn book_word_read(
    peers: Option<&[Peer]>,
    node: &[u8; 32],
    from: Option<IpAddr>,
    now_ms: u64,
) -> Book {
    peers.map_or_else(Book::default, |peers| book_word(peers, node, from, now_ms))
}

/// Judge a `vault-join` channel from what it proved: `proof` (none on an `atf/1` channel that
/// sent `node -`, which is refused), whether the fleet secret it proved is the vault's current
/// fleet code (`code` = (listener's mark, vault's mark)), whether this Mac's drive admission takes
/// the node (`driven`), the key the fleet book binds the proved node id to (`bound`), the
/// roster's deny rows, the membership log's keys, and the book's peers at `now_ms` with the
/// address the channel came from ([`book_word_read`]; `None` where the book could not be read).
///
/// # Errors
/// The channel proved no node, or a node or key the deny rows name.
pub fn judge(
    proof: Option<&NodeProof>,
    code: (FleetMark, Option<FleetMark>),
    driven: bool,
    (bound, denied, log): (Option<NodeKey>, &[String], Option<&MemberKeys>),
    (peers, from, now_ms): (Option<&[Peer]>, Option<IpAddr>, u64),
) -> Result<Judged, String> {
    let proof = proof.ok_or("vault-join needs a channel that proved its node key")?;
    if aterm_net::fleet::is_denied_node(&proof.node, denied)
        || aterm_net::fleet::is_denied_key(&proof.key, denied)
    {
        return Err("that node is on the swarm roster's deny-list".to_owned());
    }
    let (listening, vault) = code;
    // Another key the book binds the proved node id to.
    let other = bound.filter(|key| *key != proof.key);
    Ok(Judged {
        node: *proof.key.bytes(),
        fleet: vault == Some(listening) && driven,
        book: Book {
            node_id_held_out: other.is_some_and(|key| log.is_some_and(|log| log.holds_out(&key))),
            node_id_seated: other.is_some_and(|key| log.is_some_and(|log| log.admits(&key))),
            ..book_word_read(peers, proof.key.bytes(), from, now_ms)
        },
    })
}

/// The mark of the fleet code the vault at `paths` holds now, if it holds one.
#[must_use]
pub fn vault_code_mark(paths: &Paths) -> Option<FleetMark> {
    let code = client::alab_fleet_code(paths).ok().flatten()?;
    let code = aterm_net::fleet_member::JoinCode::parse(code.as_str()?).ok()?;
    FleetSecret::new(&code.secret)
        .ok()
        .map(|secret| secret.mark())
}

/// The desk a Mac with no window (the node job) serves joining Macs on, kept across its
/// connections so the desk's limit of runs a source starts a minute holds there as on a window's
/// desk; made again once this Mac's member key changes.
static HEADLESS: Mutex<Option<Arc<Mutex<Desk>>>> = Mutex::new(None);

/// [`HEADLESS`]'s desk for the member `local` names, made where it holds none or another
/// member's.
fn headless_desk(local: Local) -> Arc<Mutex<Desk>> {
    let mut held = HEADLESS.lock_or_recover();
    if let Some(desk) = held
        .as_ref()
        .filter(|desk| desk.lock_or_recover().local().key() == local.key())
    {
        desk.lock_or_recover().refresh(local);
        return Arc::clone(desk);
    }
    let desk = Arc::new(Mutex::new(Desk::new(local)));
    *held = Some(Arc::clone(&desk));
    desk
}

/// Serve one judged `vault-join` stream from `peer`: the member's desk `prove` exchange, on
/// `shared` (the window's desk) or, with none (the node job), the desk this process keeps for
/// it ([`HEADLESS`]) from the vault at `paths`. Runs on the listener's connection thread, never
/// a window's main thread.
///
/// # Errors
/// This Mac is no member, or the exchange ended without letting the Mac in (why, for the
/// caller's log; the Mac read it on a `NO` line).
pub fn serve(
    stream: &mut dyn aterm_net::fleet::Duplex,
    peer: IpAddr,
    judged: &Judged,
    shared: Option<&Mutex<Desk>>,
    paths: &Paths,
) -> aterm_secrets::Result<()> {
    let decide = |proving: &desk::Proving<'_>| client::prove_at(paths, proving, judged.book);
    let refuses = |proving: &desk::Proving<'_>| {
        client::refuses_before_possession(paths, proving, judged.book)
    };
    serve_deciding(stream, peer, judged, shared, paths, (&decide, &refuses))
}

/// [`serve`], the desk's decision and its check before possession `decide`'s: the seam a test
/// serves a judged channel through with a decision of its own, the rest as `serve` runs it.
fn serve_deciding(
    stream: &mut dyn aterm_net::fleet::Duplex,
    peer: IpAddr,
    judged: &Judged,
    shared: Option<&Mutex<Desk>>,
    paths: &Paths,
    (decide, refuses): (desk::Prove<'_>, desk::Refuses<'_>),
) -> aterm_secrets::Result<()> {
    let own;
    let desk = match shared {
        Some(desk) => desk,
        None => {
            let Some(local) = Local::of(paths)? else {
                let why = "this Mac is not an ALab member";
                let _ = aterm_secrets::members::refuse(stream, why);
                return Err(aterm_secrets::Error::new(why));
            };
            own = headless_desk(local);
            &*own
        }
    };
    desk::serve_with(
        desk,
        stream,
        peer,
        &|_| Err(aterm_secrets::Error::new("no phrase runs over vault-join")),
        Some(desk::Proof {
            decide,
            refuses,
            fleet: judged.fleet.then_some(judged.node),
        }),
    )
    .map(drop)
}

/// How long one `vault-join` dial and exchange may take.
pub const VAULT_JOIN_WITHIN: Duration = Duration::from_secs(20);

/// The joining Mac's side: prove this Mac to the fleet's members (its book's peers, `peers`, each
/// `(host, addr, port)`), over [`FleetOp::VaultJoin`] with the fleet `secret` and node `me`, one
/// after another ([`join::prove_first`]): the first that lets it in hands the log bundle, which
/// this Mac writes.
///
/// # Errors
/// No member let it in ([`join::Missed`]).
pub fn prove_over_fleet(
    paths: &Paths,
    joining: &Joining<'_>,
    (secret, me): (&FleetSecret, &NodeIdentity),
    peers: &[(String, Option<IpAddr>, u16)],
) -> Result<join::Joined, join::Missed> {
    let ats: Vec<String> = peers
        .iter()
        .map(|(host, _, port)| aterm_net::fleet_member::host_port(host, *port))
        .collect();
    let dial = |at: &str| -> aterm_secrets::Result<Box<dyn Stream>> {
        let (host, addr, port) = peers
            .iter()
            .find(|(host, _, port)| aterm_net::fleet_member::host_port(host, *port) == at)
            .ok_or_else(|| aterm_secrets::Error::new(format!("{at} is in no book")))?;
        let reached = aterm_net::fleet_member::dial(
            host,
            *addr,
            *port,
            (secret, Some(me)),
            FleetOp::VaultJoin,
            VAULT_JOIN_WITHIN,
        )
        .map_err(|error| aterm_secrets::Error::new(error.to_string()))?;
        let stream = reached.transport.into_stream();
        stream.sock.set_read_timeout(Some(VAULT_JOIN_WITHIN))?;
        stream.sock.set_write_timeout(Some(VAULT_JOIN_WITHIN))?;
        Ok(Box::new(stream))
    };
    let dial: &Dial<'_> = &dial;
    let sign_node = |message: &[u8]| -> [u8; 64] { me.sign_record(desk::PROVE_RECORD, message) };
    join::prove_first(paths, dial, joining, &sign_node, &ats)
}

#[cfg(test)]
#[path = "vault_join_tests.rs"]
mod tests;
