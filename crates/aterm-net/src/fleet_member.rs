// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet membership (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5): the state a
//! machine keeps beside the fleet secret ([`crate::fleet::KEY_FILE`]), namely its node id and
//! key, book of peers, join code and copy of the issuer's book, and the exchanges made once the
//! handshake ([`crate::fleet`]) proved both ends: join, book read, survey and drive.

use std::fmt;
use std::io::{self, BufRead, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aterm_types::machine_name::Number;
use rustls::ClientConnection;

use crate::fleet::{
    Dialed, DriveGrant, FleetError, FleetMark, FleetOp, FleetScope, FleetSecret, KEY_FILE,
    NodeIdentity, NodeKey, NodeProof, REPLY_MAX, SIG_LEN, Served, Undialed, client_handshake_by,
};
use crate::slots::{Claim, HandshakeWatchdog};
use crate::tls::TlsTransport;

pub mod fleet_seed;
pub mod name_record;
mod self_entry;
pub use self_entry::{self_entry, self_entry_while};

const MAX_WITHIN: Duration = Duration::from_secs(365 * 24 * 60 * 60);

fn deadline_after(within: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(within.min(MAX_WITHIN)).unwrap_or(now)
}

/// The join code's file in the issuer's state directory: `aterm fleet init` writes it and
/// prints its path, never the code.
pub const CODE_FILE: &str = "fleet-join-code";

/// This machine's node id's file in the state directory.
pub const NODE_FILE: &str = "fleet-node";

/// This machine's node key's file in the state directory: the 32-byte seed of its node's
/// Ed25519 key pair, as 64 lowercase hex digits and a newline, under the key file's rules.
pub const NODE_KEY_FILE: &str = "fleet-node-key";

/// The book of peers' file in the state directory.
pub const PEERS_FILE: &str = "fleet-peers";

/// A member's copy of its issuer's book, in the state directory: a file of its own, so an
/// older aterm reading [`PEERS_FILE`] never meets a copy, and no edit of the book
/// (`peers add`, `rm`, a join) writes one.
pub const ISSUER_BOOK_FILE: &str = "fleet-issuer-book";

/// A leave's record that `aterm fleet sync` joins nothing here, in the state directory: written by
/// a leave that left a fleet, removed by a join, kept by an init.
pub const OFF_FILE: &str = "fleet-off";

/// When each member last answered this machine, in the state directory
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F4): one line per member, `answered node=<id>
/// ms=<unix ms>`, or `answered place=<host:port> ms=<unix ms>` for a peer a person named with no
/// node id ([`answer_key`]), at most [`PEERS_MAX`], the oldest dropped first. A read's answer and
/// a heartbeat's write it ([`HeldBook::note_answered`]); a registration this machine took, or a
/// browse sighting, does not. A time more than [`ANSWER_AHEAD_MAX_MS`] past this machine's clock
/// is no answer ([`read_answered`]): the clock stepped back since it was written, and the next
/// answer stands over it. A heartbeat round also writes `missed node=<id> ms=<unix ms>` for each
/// member it dialled that did not answer ([`read_missed`]), which no answer reader reads, so the
/// next round asks a member gone dark behind every member not marked missed
/// ([`heartbeat_members`]), and `missed-heard node=<id> ms=<unix ms>` for each of those misses
/// that came while the member was marked missed and heard from since ([`read_missed_heard`]). A
/// file of its own, never a field of the book: the book's lines are shared with members, and the
/// copy of the issuer's book is written whole at each read. An older aterm never reads it.
pub const ANSWERED_FILE: &str = "fleet-answered";

/// The vault this machine's fleet state — its key, its book and `[fleet] drive_from`'s grants —
/// was taken under, in the state directory (`docs/DESIGN-fluid-swarm-2026-10-28.md` F17,
/// `docs/KNOWN_ISSUES.md` ATERM-KI-1105-10): written in an init's take of its key, a join's take
/// of aterm.toml, and an issuer's sync's take of the vault's default fleet again, and where none
/// is there yet by the first writer that reads a vault configured ([`HeldBook::keep_vault`]),
/// removed by a leave. What its lines mean is
/// `aterm_agent::fleet_vault`'s: this crate stores and reads its text alone ([`read_vault`]). A
/// file of its own, never a field of a book entry: the book refills from every member under the
/// shared secret, and no entry another Mac sent names the vault this one took its fleet under.
pub const VAULT_FILE: &str = "fleet-vault";

/// The most bytes [`VAULT_FILE`] holds: a vault's name and the keys of its line.
const VAULT_FILE_MAX: usize = 16 * 1024;

/// The text of [`VAULT_FILE`] in `dir`, `None` where there is none.
///
/// # Errors
/// It is there and cannot be read privately (another user's, open to others, a link, past
/// [`VAULT_FILE_MAX`]), or it is not UTF-8.
pub fn read_vault(dir: &Path) -> io::Result<Option<String>> {
    match read_private(dir, VAULT_FILE, VAULT_FILE_MAX) {
        Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{VAULT_FILE} is not UTF-8"),
            )
        }),
        Err(PrivateFileError::Missing) => Ok(None),
        Err(e) => Err(io::Error::other(format!("{VAULT_FILE}: {e}"))),
    }
}

/// How far past this machine's clock a time in [`ANSWERED_FILE`] may be and still read as an
/// answer: each is written from that clock moments after it was taken, so one later than this
/// was written before the clock stepped back (set by hand, a bad clock chip, a time-server
/// correction, a restored virtual machine). Kept, it would read "0s ago" while the member stays
/// away, and hold off every truer answer, until real time caught up with it.
pub const ANSWER_AHEAD_MAX_MS: u64 = 60_000;

const PEERS_LOCK: &str = ".fleet-peers.lock";

/// The most peers one book holds: a fleet is one LAN's machines, and a secret holder cannot
/// grow an issuer's book past this with made-up node ids.
pub const PEERS_MAX: usize = 256;

/// The most lines a book reply's trailer carries after its peer lines
/// (`docs/DESIGN-machine-names-2026-11-04.md` §3.3): the fleet seed, a reservation and 16
/// tombstones. Its head counts them as `more=<k>`, which a reader before the trailer passes over:
/// it reads the head's first count, returns after that many lines and never sees the trailer.
pub const TRAILER_LINES_MAX: usize = 18;

/// The bytes of [`REPLY_MAX`] a book reply keeps for its trailer: [`serve_book`] adds no peer
/// line that would leave less, so a book of long lines sends fewer peers, never a reply a reader
/// refuses whole.
pub const TRAILER_MAX: usize = 20 * 1024;

/// The most trailer lines a book reply's reader takes ([`book_counts`]): room for a later aterm's
/// trailer kinds past [`TRAILER_LINES_MAX`].
pub const TRAILER_READ_MAX: usize = PEERS_MAX;

/// The most bytes a book reply's head takes: `OK `, a count to [`PEERS_MAX`], ` node=` and a node
/// id, ` more=` and a count to [`TRAILER_LINES_MAX`], a newline.
const BOOK_HEAD_MAX: usize = 128;

/// The most members the book records from one address ([`record`]): room for several aterm
/// instances on one host, each on its own fleet port, while a secret holder cannot fill an
/// issuer's book with made-up nodes from one address.
pub const JOINED_PER_ADDRESS: usize = 4;

/// The dev seam that raises [`JOINED_PER_ADDRESS`] in a debug build ([`joined_per_address`]): a
/// lab whose machines all register from this Mac's one loopback address (`lab swarm partition`
/// runs seven) needs more than four from it, as a LAN, where each Mac has its own address, never
/// does. A release build reads no environment for it.
pub const JOINED_PER_ADDRESS_SEAM: &str = "ATERM_DEBUG_FLEET_JOINED_PER_ADDRESS";

/// The allowance `seam`, the value of [`JOINED_PER_ADDRESS_SEAM`], names: a whole number from
/// [`JOINED_PER_ADDRESS`] to [`PEERS_MAX`], else (none, another word, or a number outside that)
/// [`JOINED_PER_ADDRESS`]. It never lowers the allowance.
#[must_use]
pub fn joined_per_address_from(seam: Option<&std::ffi::OsStr>) -> usize {
    seam.and_then(|value| value.to_str()?.trim().parse::<usize>().ok())
        .filter(|n| (JOINED_PER_ADDRESS..=PEERS_MAX).contains(n))
        .unwrap_or(JOINED_PER_ADDRESS)
}

/// How many members the book records from one address now: [`JOINED_PER_ADDRESS`], or in a
/// debug build outside this crate's tests what [`JOINED_PER_ADDRESS_SEAM`] names
/// ([`joined_per_address_from`]), read at each call.
fn joined_per_address() -> usize {
    #[cfg(all(debug_assertions, not(test)))]
    {
        joined_per_address_from(std::env::var_os(JOINED_PER_ADDRESS_SEAM).as_deref())
    }
    #[cfg(not(all(debug_assertions, not(test))))]
    {
        JOINED_PER_ADDRESS
    }
}

/// How often a joined member's listener re-registers with its issuer (`DESIGN-alab-swarm` §5.3).
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(3 * 60);

/// Window (`9 min`, three heartbeat intervals) within which a `joined` entry is considered live
/// when judging whether a 5th node at the same IP may replace a silent entry.
pub const LIVE_WITHIN_MS: u64 = 9 * 60 * 1_000;

/// Silence window (`14 days`) after which a peer renders as `dormant` in `aterm fleet peers`.
pub const DORMANT_AFTER_MS: u64 = 14 * 24 * 60 * 60 * 1_000;

/// Silence window (`90 days`) after which a `joined` peer expires from the issuer's book.
pub const EXPIRE_AFTER_MS: u64 = 90 * 24 * 60 * 60 * 1_000;

/// The fleet port when none is named: `[fleet] listen`'s documented example, one of the person's
/// ports no private aterm reaches ([`aterm_types::reach::FLEET_PORT`]).
pub const DEFAULT_PORT: u16 = aterm_types::reach::FLEET_PORT;

/// The port a place that names none is at, where `init` listens and a host word with no port is
/// dialled: [`DEFAULT_PORT`], save in a Cargo test binary (a debug build,
/// [`aterm_uds::test_binary::is_test_binary`]), where it is a port no aterm running on the machine
/// serves. An aterm whose fleet listens on [`DEFAULT_PORT`] there would otherwise be dialled: a
/// test's `init` with no `--port` would write that place, and its proof of the listener there
/// would dial that aterm and wait out its answer under another secret.
#[must_use]
pub fn default_port() -> u16 {
    #[cfg(debug_assertions)]
    if let Some(port) = held_port() {
        return port;
    }
    DEFAULT_PORT
}

/// A test binary's [`default_port`], none elsewhere: the local end of a loopback connection this
/// process keeps open. No socket listens there as it is chosen, so a dial there is refused at once,
/// and the kernel gives it to no IPv4 loopback bind that leaves the port to it, as a test's
/// listener does. An IPv6 one may get it (macOS's IPv6 allocator looks at IPv6 sockets alone), as
/// it may any free port, so a dual-stack listener elsewhere could still, rarely, take a dial there.
#[cfg(debug_assertions)]
fn held_port() -> Option<u16> {
    static HELD: std::sync::OnceLock<Option<HeldPort>> = std::sync::OnceLock::new();
    let held = HELD.get_or_init(|| hold(&std::env::current_exe().ok()?));
    held.as_ref().map(|(.., port)| *port)
}

/// Both ends of [`held_port`]'s connection, and its port.
#[cfg(debug_assertions)]
type HeldPort = (TcpStream, TcpStream, u16);

/// The connection [`held_port`] keeps for the program at `exe`: one only where `exe` is a Cargo
/// test binary, none for a debug `aterm` (`target/debug/aterm`, what `lab instance` and the
/// integration tests run), which keeps [`DEFAULT_PORT`].
#[cfg(debug_assertions)]
fn hold(exe: &Path) -> Option<HeldPort> {
    if !aterm_uds::test_binary::is_test_binary(exe) {
        return None;
    }
    // A wait for what must happen: a hang detector, never a latency budget.
    const HANG: Duration = Duration::from_secs(60);
    let listener =
        aterm_uds::kept_out(|| std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)))
            .expect("a loopback listener");
    // Both ends wait outside the spawn lock: a loopback handshake still crosses the kernel's
    // input queue, so neither the connect nor the accept is sure to answer at once.
    let near = aterm_uds::connect_tcp(&listener.local_addr().expect("its address"), HANG)
        .expect("a connection to it");
    #[cfg(unix)]
    let (far, _) = aterm_uds::accept(&listener).expect("the connection accepted");
    // Off Unix a socket reaches a child only when it is made inheritable.
    #[cfg(not(unix))]
    let (far, _) = listener.accept().expect("the connection accepted");
    let port = near
        .local_addr()
        .expect("the connection's local end")
        .port();
    Some((near, far, port))
}

/// How many bytes of secret a join code carries: 160 bits, the fleet's shortest secret.
pub const SECRET_LEN: usize = FleetSecret::MIN_LEN;

/// The longest line either end of the join exchange sends: a registration's self-signature
/// ([`Signed`]) takes some 160 bytes of it. An aterm before 0.147 reads 512 at most.
pub const RECORD_MAX: usize = 1024;

/// Maximum bytes in either on-disk fleet book, shared by strict readers of signed claims.
pub const PEERS_FILE_MAX: usize = 256 * 1024;

/// How long a take of the book's lock waits for another writer, which holds it for one
/// read-modify-write ([`write_held`]), before it refuses: in a debug build, a test's wait on its
/// thread ([`lock_waiting`]) or [`LOCK_WAIT_SEAM`]'s replaces it.
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// The dev seam that sets every take of the book's lock in a debug aterm to wait this many
/// milliseconds in place of [`LOCK_WAIT`]: a test of the binary gives it the hang detector, as
/// every holder there is an aterm or a test on the same machine that releases the lock, which a
/// loaded machine can keep past [`LOCK_WAIT`]. A value that names no milliseconds (empty, `0`)
/// leaves [`LOCK_WAIT`]; a release build never reads it.
pub const LOCK_WAIT_SEAM: &str = "ATERM_DEBUG_FLEET_LOCK_WAIT_MS";

/// The dev seam that has a debug aterm write [`LOCK_HELD_LINE`] to stderr once per take of the
/// book's lock that finds it held, before it waits: a test of the binary starts another verb only
/// after it, so the order the two meet the lock in is known. A release build never reads it.
pub const LOCK_TOLD_SEAM: &str = "ATERM_DEBUG_FLEET_LOCK_TOLD";

/// The line [`LOCK_TOLD_SEAM`] has a debug aterm write.
pub const LOCK_HELD_LINE: &str = "aterm fleet: the book's lock is held; waiting for it";

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// A test's wait for the book's lock on its thread in place of [`LOCK_WAIT`], and who is told each time the lock is found held.
    static TEST_WAIT: std::cell::RefCell<Option<(Duration, Option<std::sync::mpsc::Sender<()>>)>> =
        const { std::cell::RefCell::new(None) };
    /// The error a test's [`Leaving::write_off`] on its thread meets in place of any write.
    static OFF_REFUSED: std::cell::Cell<Option<io::ErrorKind>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// A test can hold a writer at its first blocked lock attempt without holding the lock.
    static ON_LOCK_WAIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// `run` with this thread's [`Leaving::write_off`] failing as `kind` before it writes anything, as a full volume fails it (a test seam).
#[cfg(any(test, debug_assertions))]
pub fn off_refused<R>(kind: io::ErrorKind, run: impl FnOnce() -> R) -> R {
    let before = OFF_REFUSED.replace(Some(kind));
    let ran = run();
    OFF_REFUSED.set(before);
    ran
}

/// `run` with this thread's waits for the book's lock lasting `wait` in place of [`LOCK_WAIT`] (a test seam).
#[cfg(any(test, debug_assertions))]
pub fn lock_waiting<R>(wait: Duration, run: impl FnOnce() -> R) -> R {
    let before = TEST_WAIT.with_borrow_mut(|seam| seam.replace((wait, None)));
    let ran = run();
    TEST_WAIT.with_borrow_mut(|seam| *seam = before);
    ran
}

/// [`lock_waiting`], with `told` sent to each time the lock is found held (a test seam).
#[cfg(any(test, debug_assertions))]
pub fn lock_waiting_told<R>(
    wait: Duration,
    told: std::sync::mpsc::Sender<()>,
    run: impl FnOnce() -> R,
) -> R {
    let before = TEST_WAIT.with_borrow_mut(|seam| seam.replace((wait, Some(told))));
    let ran = run();
    TEST_WAIT.with_borrow_mut(|seam| *seam = before);
    ran
}

fn lock_wait() -> Duration {
    #[cfg(any(test, debug_assertions))]
    if let Some(wait) = TEST_WAIT.with_borrow(|seam| seam.as_ref().map(|(wait, _)| *wait)) {
        return wait;
    }
    // This crate's own tests read no environment, as the loopback guard's do not.
    #[cfg(all(debug_assertions, not(test)))]
    if let Some(ms) = std::env::var_os(LOCK_WAIT_SEAM)
        .and_then(|ms| {
            let s = ms.to_str()?.trim();
            s.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| s.parse::<u64>().ok())?
        })
        .filter(|&ms| ms > 0)
    {
        return Duration::from_millis(ms);
    }
    LOCK_WAIT
}

/// The book's lock found held by a take, `first` the first time that take found it so.
fn found_held(first: bool) {
    #[cfg(test)]
    if let Some(wait) = ON_LOCK_WAIT.with_borrow_mut(Option::take) {
        wait();
    }
    #[cfg(any(test, debug_assertions))]
    TEST_WAIT.with_borrow(|seam| {
        if let Some((_, Some(told))) = seam {
            let _ = told.send(());
        }
    });
    #[cfg(all(debug_assertions, not(test)))]
    if first && std::env::var_os(LOCK_TOLD_SEAM).is_some() {
        let _ = aterm_uds::stdout::stderr_line(format_args!("{LOCK_HELD_LINE}"));
    }
    #[cfg(not(all(debug_assertions, not(test))))]
    let _ = first;
}

/// The places tests hold silent ([`HeldSilent`]), read by dials on every thread, each with the
/// stage it holds, the bound the last dial held there would have had, and how many dials it
/// held.
#[cfg(any(test, debug_assertions))]
static HELD_SILENT: std::sync::Mutex<Vec<SilentPlace>> = std::sync::Mutex::new(Vec::new());

/// One place a test holds silent ([`HeldSilent`]).
#[cfg(any(test, debug_assertions))]
#[derive(Debug)]
struct SilentPlace {
    at: SocketAddr,
    /// Whether it lets the handshake through and holds the registration after it
    /// ([`HeldSilent::after_proof`]) rather than the handshake.
    after_proof: bool,
    bound: Option<Duration>,
    dials: usize,
}

/// Whether a test holds `to` silent at the stage `after_proof` names ([`HeldSilent`]), keeping
/// `left`, the bound that stage of a dial there would have had, and counting the dial, where it
/// does.
#[cfg(any(test, debug_assertions))]
fn held_at(to: SocketAddr, after_proof: bool, left: Duration) -> bool {
    let mut held = HELD_SILENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut found = false;
    for place in held
        .iter_mut()
        .filter(|place| place.at == to && place.after_proof == after_proof)
    {
        place.bound = Some(left);
        place.dials += 1;
        found = true;
    }
    found
}

/// Whether a test holds `to`'s handshake silent ([`HeldSilent::at`]), as [`held_at`] keeps it.
#[cfg(any(test, debug_assertions))]
fn held_silent(to: SocketAddr, left: Duration) -> bool {
    held_at(to, false, left)
}

/// A place a test holds silent while this lives: a connection there times out its handshake
/// at once, as a listener that accepts and sends no handshake does at its deadline. The dial
/// may still try another address. A test bounds the dial by its hang detector and waits on no
/// clock (a test seam). The bound that handshake would have had is kept
/// ([`HeldSilent::overrode`]), so a test still checks the deadline it carried, and so is how
/// many dials it held ([`HeldSilent::dials`]).
#[cfg(any(test, debug_assertions))]
#[derive(Debug)]
pub struct HeldSilent(SocketAddr, bool);

#[cfg(any(test, debug_assertions))]
impl HeldSilent {
    /// Hold `at`'s handshake silent.
    #[must_use]
    pub fn at(at: SocketAddr) -> Self {
        Self::hold(at, false)
    }

    /// Let the handshake at `at` through and hold the registration after it silent
    /// ([`register_by`]): a member that proves its node and never answers. The bound kept is
    /// the registration's.
    #[must_use]
    pub fn after_proof(at: SocketAddr) -> Self {
        Self::hold(at, true)
    }

    fn hold(at: SocketAddr, after_proof: bool) -> Self {
        HELD_SILENT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(SilentPlace {
                at,
                after_proof,
                bound: None,
                dials: 0,
            });
        Self(at, after_proof)
    }

    fn read<T>(&self, of: impl Fn(&SilentPlace) -> T) -> Option<T> {
        HELD_SILENT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|place| place.at == self.0 && place.after_proof == self.1)
            .map(of)
    }

    /// The bound the last dial held here would have had: what is left of its deadline once it
    /// reached the stage held. None while no dial has reached it here.
    #[must_use]
    pub fn overrode(&self) -> Option<Duration> {
        self.read(|place| place.bound).flatten()
    }

    /// How many dials reached the stage held here while this lived.
    #[must_use]
    pub fn dials(&self) -> usize {
        self.read(|place| place.dials).unwrap_or_default()
    }
}

#[cfg(any(test, debug_assertions))]
impl Drop for HeldSilent {
    fn drop(&mut self) {
        let mut held = HELD_SILENT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = held
            .iter()
            .position(|place| place.at == self.0 && place.after_proof == self.1)
        {
            held.swap_remove(at);
        }
    }
}

use aterm_uds::private_file::{
    PrivateDir, check_owned_dir, judge_private_dir, remove_private, write_private,
};
pub use aterm_uds::private_file::{PrivateFileError, ensure_private_dir, read_private};

/// A new fleet secret, [`SECRET_LEN`] bytes from the OS CSPRNG.
pub fn mint_secret() -> io::Result<[u8; SECRET_LEN]> {
    let mut secret = [0u8; SECRET_LEN];
    aterm_uds::rand::fill(&mut secret)?;
    Ok(secret)
}

/// Whether `word` is a legacy node id or a key-derived mesh principal.
#[inline]
#[must_use]
pub fn is_node_id(word: &str) -> bool {
    crate::fleet::is_node_id(word)
}

/// astream's identity principal: the first eight raw public-key bytes in lowercase hex.
#[must_use]
pub fn node_principal(key: NodeKey) -> String {
    format!("n-{}", &key.hex()[..16])
}

/// This machine's node id: derived from its node key with mesh enabled, otherwise from
/// [`NODE_FILE`] in `dir`, minted on first use (128 bits from the OS CSPRNG).
pub fn node_id(dir: &Path) -> io::Result<String> {
    if aterm_types::mesh_switch::MESH {
        return node_identity(dir).map(|me| me.node().to_owned());
    }
    legacy_node_id(dir)
}

fn legacy_node_id(dir: &Path) -> io::Result<String> {
    if let Some(id) = read_legacy_node_id(dir)? {
        return Ok(id);
    }
    let id = aterm_uds::rand::hex_token::<16>()?;
    write_private(dir, NODE_FILE, format!("{id}\n").as_bytes())?;
    Ok(id)
}

const CODE_FILE_MAX: usize = 4096;

/// The act that moves this machine's fleet listener to a port of its own: the issuer's rotate
/// (its join code names the port), or a member's join again.
#[must_use]
pub fn moves_listener(dir: &Path) -> &'static str {
    if check_owned_dir(dir).is_ok() && std::fs::symlink_metadata(dir.join(CODE_FILE)).is_ok() {
        "`aterm fleet init --rotate --port <n>` moves it (every member joins again)"
    } else {
        "`aterm fleet join --port <n>` moves it"
    }
}

/// The join code this machine issued, as [`CODE_FILE`] in `dir` holds it: the secret, and
/// the host and port members dial.
pub fn read_join_code(dir: &Path) -> Result<JoinCode, String> {
    let bytes = read_private(dir, CODE_FILE, CODE_FILE_MAX).map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "it is not text".to_owned())?;
    JoinCode::parse(&text).map_err(|e| e.to_string())
}

/// This machine's node id, never minted: derived from its existing key with mesh enabled,
/// otherwise read from [`NODE_FILE`]. `None` while that source is missing.
pub fn read_node_id(dir: &Path) -> io::Result<Option<String>> {
    if aterm_types::mesh_switch::MESH {
        return read_node_seed(dir)?
            .map(|seed| {
                identity_of("00000000000000000000000000000000", &seed)
                    .map(|me| node_principal(me.key()))
            })
            .transpose();
    }
    read_legacy_node_id(dir)
}

fn read_legacy_node_id(dir: &Path) -> io::Result<Option<String>> {
    match read_private(dir, NODE_FILE, 64) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let word = text.strip_suffix('\n').unwrap_or(&text);
            if is_node_id(word) {
                Ok(Some(word.to_owned()))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{NODE_FILE} holds no node id (32 lowercase hex digits)"),
                ))
            }
        }
        Err(PrivateFileError::Missing) => Ok(None),
        Err(e) => Err(io::Error::other(format!("{NODE_FILE}: {e}"))),
    }
}

const NODE_KEY_FILE_MAX: usize = 2 * NODE_SEED_LEN + 1;

const NODE_SEED_LEN: usize = 32;

/// This machine's node, its node id and node key, each minted the first time it is missing
/// ([`node_id`]; the key 256 bits from the OS CSPRNG, written to [`NODE_KEY_FILE`]), under the
/// book's lock, waited for as every writer waits. A legacy node minted already is read without
/// the lock: its two files are written once, each whole, and never change, so only a mint takes
/// it. The vault subscriber asks ALab's members with its node while it waits to be let in, in a
/// launchd band that holds every thread at priority 4, where no lift reaches the lock's holder.
/// With the mesh switch on every call takes the lock, since it rewrites the book's legacy node
/// ids to key-derived principals.
/// While the mesh switch is on, the node dials [`crate::fleet::ALPN_MESH`] first under the
/// verdict this process provided ([`judge_mesh_by`]), and notes each listener it proves there
/// into the network place under `dir` ([`noting_in`]).
pub fn node_identity(dir: &Path) -> io::Result<NodeIdentity> {
    let mesh = aterm_types::mesh_switch::MESH;
    node_identity_with_mesh(dir, mesh)
        .map(|me| noting_in(dialing_by(me, mesh, MESH_VERDICT.get()), dir))
}

/// Observe an actual cold node-key mint under its book lock, before any key is written.
/// The callback runs on the minting thread with the same priority lift as production.
/// Existing identities are read without invoking it. This makes scheduler regressions
/// measurable at the held effect boundary without a timing ceiling or a global test hook.
/// # Errors
/// The ordinary identity reader's path, lock, random source or write failure.
#[cfg(any(test, debug_assertions))]
pub fn node_identity_during_mint(dir: &Path, held: impl FnOnce()) -> io::Result<NodeIdentity> {
    let mesh = aterm_types::mesh_switch::MESH;
    node_identity_before_mint(dir, mesh, held)
        .map(|me| noting_in(dialing_by(me, mesh, MESH_VERDICT.get()), dir))
}

/// The verdict this process's dialers judge an [`crate::fleet::ALPN_MESH`] listener by: the
/// process that reads this Mac's membership log provides it once, at its start
/// ([`judge_mesh_by`]), as aterm-net reads no log itself.
static MESH_VERDICT: std::sync::OnceLock<crate::fleet::DenyFn> = std::sync::OnceLock::new();

/// Provide this process's dialers `verdict`, read fresh for each dial: with the mesh switch on,
/// every node [`node_identity`] and [`read_node_identity`] load dials
/// [`crate::fleet::ALPN_MESH`] first and judges each listener by it
/// ([`NodeIdentity::dialing_mesh`]); with the switch off, or with none provided, they dial
/// [`crate::fleet::ALPN_NODES`] alone, as before the mesh. The first provision stands: `false`
/// where one already did.
pub fn judge_mesh_by(verdict: crate::fleet::DenyFn) -> bool {
    MESH_VERDICT.set(verdict).is_ok()
}

/// The offer [`on_unknown_to`] provided.
static UNKNOWN_TO: std::sync::OnceLock<fn(SocketAddr)> = std::sync::OnceLock::new();

/// Provide what this process does once an [`crate::fleet::ALPN_MESH`] listener's log binds this
/// Mac's node key to no seat (it answered `NO unknown`, [`FleetError::UnknownKey`]): the process
/// that keeps this Mac's membership log offers that member's desk, at the same address, the
/// `Node` record that binds the key (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4), so a later
/// dial meets it on the mesh. `offer` is called on the dial's thread and must return at once; the
/// dial goes again on [`crate::fleet::ALPN_NODES`] without waiting for it. The first provision
/// stands: `false` where one already did.
pub fn on_unknown_to(offer: fn(SocketAddr)) -> bool {
    UNKNOWN_TO.set(offer).is_ok()
}

/// `me` noting each [`crate::fleet::ALPN_MESH`] listener it proves into the heard ledger of the
/// network place under the state root `dir` ([`crate::fleet::heard::Heard::live`]) where it
/// dials the mesh ([`dialing_by`]): every mesh handshake this process dials is a node-proven
/// answer (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B6). Elsewhere it notes nothing.
fn noting_in(me: NodeIdentity, dir: &Path) -> NodeIdentity {
    #[cfg(unix)]
    if me.dials_mesh() {
        return me.noting(Some(crate::fleet::heard::Heard::live(dir)));
    }
    #[cfg(not(unix))]
    let _ = dir;
    me
}

/// `me` dialing the mesh where the switch `mesh` is on and a verdict was provided.
pub(crate) fn dialing_by(
    me: NodeIdentity,
    mesh: bool,
    verdict: Option<&crate::fleet::DenyFn>,
) -> NodeIdentity {
    me.dialing_mesh(verdict.filter(|_| mesh).cloned())
}

fn node_identity_with_mesh(dir: &Path, mesh: bool) -> io::Result<NodeIdentity> {
    node_identity_before_mint(dir, mesh, || {})
}

fn node_identity_before_mint(
    dir: &Path,
    mesh: bool,
    held: impl FnOnce(),
) -> io::Result<NodeIdentity> {
    if !mesh
        && let Ok(Some(node)) = read_legacy_node_id(dir)
        && let Ok(Some(seed)) = read_node_seed(dir)
    {
        return identity_of(&node, &seed);
    }
    let _identity = BookLock::take(dir, lock_wait())
        .map_err(|e| io::Error::new(e.kind(), lock_refused(dir, &e)))?;
    let seed = match read_node_seed(dir)? {
        Some(seed) => seed,
        None => {
            held();
            let mut seed = [0u8; NODE_SEED_LEN];
            aterm_uds::rand::fill(&mut seed)?;
            let mut text = aterm_codec::hex::encode_lower(&seed);
            text.push('\n');
            write_private(dir, NODE_KEY_FILE, text.as_bytes())?;
            seed
        }
    };
    if mesh {
        let me = identity_of("00000000000000000000000000000000", &seed)?;
        rewrite_mesh_book(dir)?;
        identity_of(&node_principal(me.key()), &seed)
    } else {
        identity_of(&legacy_node_id(dir)?, &seed)
    }
}

/// This machine's node as [`NODE_FILE`] and [`NODE_KEY_FILE`] in `dir` hold it, never
/// minted: `None` while either is missing. It dials, and notes, as [`node_identity`]'s does.
pub fn read_node_identity(dir: &Path) -> io::Result<Option<NodeIdentity>> {
    let Some(node) = read_node_id(dir)? else {
        return Ok(None);
    };
    let mesh = aterm_types::mesh_switch::MESH;
    Ok(read_node_seed(dir)?
        .map(|seed| identity_of(&node, &seed))
        .transpose()?
        .map(|me| noting_in(dialing_by(me, mesh, MESH_VERDICT.get()), dir)))
}

fn read_node_seed(dir: &Path) -> io::Result<Option<[u8; NODE_SEED_LEN]>> {
    let bytes = match read_private(dir, NODE_KEY_FILE, NODE_KEY_FILE_MAX) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(None),
        Err(e) => return Err(io::Error::other(format!("{NODE_KEY_FILE}: {e}"))),
    };
    let malformed = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{NODE_KEY_FILE} holds no node key (64 lowercase hex digits)"),
        )
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| malformed())?;
    let digits = text.strip_suffix('\n').unwrap_or(text);
    if !aterm_codec::hex::is_lower(digits, 2 * NODE_SEED_LEN) {
        return Err(malformed());
    }
    crate::hex_decode(digits)
        .and_then(|seed| seed.try_into().ok())
        .map(Some)
        .ok_or_else(malformed)
}

fn identity_of(node: &str, seed: &[u8]) -> io::Result<NodeIdentity> {
    NodeIdentity::from_seed(node, seed).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{NODE_KEY_FILE} gives no node key"),
        )
    })
}

const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// What every join code starts with (the survey's redaction keys on it).
pub const CODE_PREFIX: &str = "atf1-";

const DATA_SYMBOLS: usize = SECRET_LEN * 8 / 5;

const CHECK_SYMBOLS: usize = 4;

const CODE_SYMBOLS: usize = DATA_SYMBOLS + CHECK_SYMBOLS;

const GROUP: usize = 4;

const GF_POLY: u8 = 0b10_0101;

/// A join code: the fleet secret alone (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17: any
/// member admits a join, so the code names none), and the place a text of it named after an `@`,
/// as every code an aterm before 0.147 wrote did.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCode {
    /// The fleet secret.
    pub secret: [u8; SECRET_LEN],
    /// The host name or address and fleet port after the text's `@`, if any: where a member
    /// listened when the code was written, a place a join dials first, never part of the code.
    pub hint: Option<(String, u16)>,
}

impl fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.hint {
            Some((host, port)) => write!(f, "JoinCode(.. @{})", host_port(host, *port)),
            None => f.write_str("JoinCode(..)"),
        }
    }
}

/// Why a text is no join code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeError {
    /// It does not start `atf1-`.
    Prefix,
    /// The character at this 1-based place after `atf1-` (dashes and white space aside) is no
    /// code character.
    Character(usize),
    /// This many characters after `atf1-`, before any `@` (dashes and white space aside), not
    /// [`CODE_SYMBOLS`].
    Length(usize),
    /// The check characters do not match: a typo.
    Typo,
    /// What follows its `@` is no host and port, for this reason.
    Place(&'static str),
}

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prefix => f.write_str(
                "a join code starts `atf1-` (the file `aterm fleet init` wrote holds one)",
            ),
            Self::Character(at) => write!(
                f,
                "the join code's character {at} after its `atf1-`, dashes aside, is none of 0-9 \
                 and a-z without i, l, o, u: it was not copied whole"
            ),
            Self::Length(n) => write!(
                f,
                "a join code has {CODE_SYMBOLS} letters and digits after its `atf1-`, in {} \
                 groups of {GROUP}, not {n}: it was not copied whole",
                CODE_SYMBOLS / GROUP
            ),
            Self::Typo => f.write_str(
                "the join code's check characters do not match: a character is mistyped or \
                 two are swapped; copy it again",
            ),
            Self::Place(why) => write!(
                f,
                "what follows the join code's `@` is no host and port: {why}"
            ),
        }
    }
}

impl std::error::Error for CodeError {}

impl JoinCode {
    /// The code as text: `atf1-` and nine groups of four. It names no member.
    #[must_use]
    pub fn text(&self) -> String {
        let mut symbols = to_symbols(&self.secret);
        let check = check_symbols(&symbols);
        symbols.extend_from_slice(&check);
        let mut out = String::from(CODE_PREFIX);
        for (at, symbol) in symbols.iter().enumerate() {
            if at > 0 && at % GROUP == 0 {
                out.push('-');
            }
            out.push(char::from(ALPHABET[usize::from(*symbol & 31)]));
        }
        out
    }

    /// The code as an aterm before 0.147 reads it, whose parser requires an `@<host>:<port>`:
    /// [`Self::text`], `@`, then `host` and `port`, a member to dial first. The vault's entry of
    /// the code is this until no such aterm is a member (F20).
    #[must_use]
    pub fn text_at(&self, (host, port): (&str, u16)) -> String {
        format!("{}@{}", self.text(), host_port(host, port))
    }

    /// The code as its file holds it: [`Self::text_at`] its [`Self::hint`], so an aterm before
    /// 0.146 reads a code an init here minted, or [`Self::text`] where it names none.
    #[must_use]
    pub fn written(&self) -> String {
        match &self.hint {
            Some((host, port)) => self.text_at((host, *port)),
            None => self.text(),
        }
    }

    /// Read a join code: `atf1-` in either case, its characters in any case with `-` and
    /// white space between them ignored, then, if there is one, `@<host>:<port>` ([`Self::hint`]).
    pub fn parse(text: &str) -> Result<Self, CodeError> {
        let text = text.trim();
        let body = text
            .get(..CODE_PREFIX.len())
            .filter(|head| head.eq_ignore_ascii_case(CODE_PREFIX))
            .and_then(|_| text.get(CODE_PREFIX.len()..))
            .ok_or(CodeError::Prefix)?;
        let (code, place) = match body.rsplit_once('@') {
            Some((code, place)) => (code, Some(place)),
            None => (body, None),
        };
        let mut symbols = Vec::with_capacity(CODE_SYMBOLS);
        let mut seen = 0usize;
        for c in code.chars() {
            if c == '-' || c.is_whitespace() {
                continue;
            }
            seen += 1;
            symbols.push(symbol_of(c).ok_or(CodeError::Character(seen))?);
        }
        if symbols.len() != CODE_SYMBOLS {
            return Err(CodeError::Length(symbols.len()));
        }
        if !is_codeword(&symbols) {
            return Err(CodeError::Typo);
        }
        let hint = place
            .map(|place| parse_host_port(place, None).map_err(CodeError::Place))
            .transpose()?;
        Ok(Self {
            secret: from_symbols(&symbols[..DATA_SYMBOLS]),
            hint,
        })
    }
}

/// Where a join code sits in `text`, wherever it was pasted or typed: the byte range of each
/// run of [`CODE_SYMBOLS`] code characters (either case, `o`/`i`/`l` read as the parser reads
/// them) that the check characters accept, with `-` or white space between them, including
/// its `atf1-` when one leads it and its `@<host>:<port>` when one follows.
#[must_use]
pub fn code_spans(text: &str) -> Vec<std::ops::Range<usize>> {
    struct Word {
        start: usize,
        end: usize,
        symbols: Vec<u8>,
    }
    let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
    let mut run: Vec<Word> = Vec::new();
    let mut word: Option<Word> = None;
    let judge = |run: &[Word], spans: &mut Vec<std::ops::Range<usize>>| {
        let mut from = 0;
        while from < run.len() {
            let mut count = 0;
            let mut found = None;
            for (to, w) in run.iter().enumerate().skip(from) {
                count += w.symbols.len();
                if count >= CODE_SYMBOLS {
                    found = (count == CODE_SYMBOLS).then_some(to);
                    break;
                }
            }
            let Some(to) = found.filter(|&to| {
                let symbols: Vec<u8> = run[from..=to]
                    .iter()
                    .flat_map(|w| w.symbols.iter().copied())
                    .collect();
                is_codeword(&symbols)
            }) else {
                from += 1;
                continue;
            };
            let prefixed = from > 0
                && text[run[from - 1].start..run[from - 1].end]
                    .eq_ignore_ascii_case(CODE_PREFIX.trim_end_matches('-'));
            let start = if prefixed {
                run[from - 1].start
            } else {
                run[from].start
            };
            let mut end = run[to].end;
            if text[end..].starts_with('@') {
                end += text[end..]
                    .find(|c: char| {
                        !(c.is_ascii_alphanumeric()
                            || matches!(c, '@' | '.' | '-' | '_' | ':' | '[' | ']'))
                    })
                    .unwrap_or(text.len() - end);
            }
            spans.push(start..end);
            from = to + 1;
        }
    };
    for (at, c) in text.char_indices() {
        if let Some(symbol) = symbol_of(c) {
            word.get_or_insert_with(|| Word {
                start: at,
                end: at,
                symbols: Vec::new(),
            })
            .symbols
            .push(symbol);
            continue;
        }
        if let Some(mut w) = word.take() {
            w.end = at;
            run.push(w);
        }
        if c != '-' && !c.is_whitespace() {
            judge(&run, &mut spans);
            run.clear();
        }
    }
    if let Some(mut w) = word.take() {
        w.end = text.len();
        run.push(w);
    }
    judge(&run, &mut spans);
    spans
}

#[inline]
fn symbol_of(c: char) -> Option<u8> {
    let c = match c.to_ascii_lowercase() {
        'o' => '0',
        'i' | 'l' => '1',
        c => c,
    };
    let byte = u8::try_from(c).ok()?;
    ALPHABET
        .iter()
        .position(|a| *a == byte)
        .and_then(|at| u8::try_from(at).ok())
}

fn to_symbols(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(CODE_SYMBOLS);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &byte in bytes {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(((acc >> bits) & 31) as u8);
        }
        acc &= (1 << bits) - 1;
    }
    out
}

fn from_symbols(symbols: &[u8]) -> [u8; SECRET_LEN] {
    let mut out = [0u8; SECRET_LEN];
    let (mut acc, mut bits, mut at) = (0u32, 0u32, 0usize);
    for &symbol in symbols {
        acc = (acc << 5) | u32::from(symbol & 31);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            if let Some(slot) = out.get_mut(at) {
                *slot = ((acc >> bits) & 0xff) as u8;
            }
            at += 1;
        }
        acc &= (1 << bits) - 1;
    }
    out
}

#[inline]
const fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0;
    while b != 0 {
        if b & 1 != 0 {
            product ^= a;
        }
        b >>= 1;
        a <<= 1;
        if a & 0x20 != 0 {
            a ^= GF_POLY;
        }
    }
    product
}

#[inline]
const fn generator() -> [u8; CHECK_SYMBOLS + 1] {
    let mut g = [0u8; CHECK_SYMBOLS + 1];
    g[0] = 1;
    let mut root = 1u8;
    let mut degree = 0;
    while degree < CHECK_SYMBOLS {
        let mut i = degree + 1;
        while i >= 1 {
            g[i] ^= gf_mul(g[i - 1], root);
            i -= 1;
        }
        root = gf_mul(root, 2);
        degree += 1;
    }
    g
}

fn check_symbols(data: &[u8]) -> [u8; CHECK_SYMBOLS] {
    let g = generator();
    let mut rem = [0u8; CHECK_SYMBOLS];
    for &symbol in data {
        let factor = symbol ^ rem[0];
        rem.copy_within(1.., 0);
        rem[CHECK_SYMBOLS - 1] = 0;
        for (slot, coefficient) in rem.iter_mut().zip(&g[1..]) {
            *slot ^= gf_mul(*coefficient, factor);
        }
    }
    rem
}

fn is_codeword(symbols: &[u8]) -> bool {
    let mut root = 1u8;
    for _ in 0..CHECK_SYMBOLS {
        let syndrome = symbols
            .iter()
            .fold(0u8, |acc, symbol| gf_mul(acc, root) ^ symbol);
        if syndrome != 0 {
            return false;
        }
        root = gf_mul(root, 2);
    }
    true
}

/// Whether `host` is a host a fleet names: an IP address, or a name of letters, digits, `.`,
/// `-` and `_` (at most 253, not starting with `.` or `-`).
#[inline]
#[must_use]
pub fn is_host(host: &str) -> bool {
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    (1..=253).contains(&host.len())
        && !host.starts_with(['.', '-'])
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// `host` and `port` as one dialable word: `[v6]:port` for an IPv6 address.
#[must_use]
pub fn host_port(host: &str, port: u16) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Read `host[:port]` (`[v6]:port` for an IPv6 address with a port, a bare IPv6 address
/// without one); the port is `default` when none is written.
pub fn parse_host_port(text: &str, default: Option<u16>) -> Result<(String, u16), &'static str> {
    let text = text.trim();
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or("an unclosed `[`")?;
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err("only an IPv6 address goes in `[…]`");
        }
        match after {
            "" => (host, None),
            _ => (
                host,
                Some(
                    after
                        .strip_prefix(':')
                        .ok_or("`]` is followed by no `:port`")?,
                ),
            ),
        }
    } else if text.parse::<std::net::Ipv6Addr>().is_ok() {
        (text, None)
    } else {
        match text.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (text, None),
        }
    };
    if !is_host(host) {
        return Err("a host is an address or a name of letters, digits, `.`, `-` and `_`");
    }
    let port = match port {
        Some(port) => port
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| port.parse::<u16>().ok())
            .flatten()
            .filter(|port| *port != 0)
            .ok_or("a port is 1 to 65535")?,
        None => default.ok_or("no port")?,
    };
    Ok((host.to_owned(), port))
}

/// How a peer came into the book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// It registered with this machine, the fleet's issuer (`aterm fleet join` there).
    Joined,
    /// This machine joined the fleet it issues.
    Issuer,
    /// A person named it (`aterm fleet peers add`).
    Manual,
    /// A member's copy of an entry of its issuer's book ([`copies_of`]): kept in
    /// [`ISSUER_BOOK_FILE`], never in [`PEERS_FILE`], written only by a read of that book,
    /// replaced whole at each answer, and never passed on.
    IssuerBook,
}

impl Via {
    /// The word the book spells it with.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Issuer => "issuer",
            Self::Manual => "manual",
            Self::IssuerBook => "issuer-book",
        }
    }

    #[inline]
    fn from_word(word: &str) -> Option<Self> {
        [Self::Joined, Self::Issuer, Self::Manual, Self::IssuerBook]
            .into_iter()
            .find(|via| via.word() == word)
    }

    #[inline]
    const fn first_hand(self) -> bool {
        matches!(self, Self::Joined | Self::Manual)
    }
}

/// One peer in the book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Its node id; `None` for a peer a person named (or a copy of one).
    pub node: Option<String>,
    /// The node key the book binds that node id to; `None` for a node an older aterm
    /// recorded (bound at its next registration) and for a peer a person named.
    pub key: Option<NodeKey>,
    /// Where a node bound to no key may bind one, when that is not [`Peer::addr`]: the
    /// address its entry was recorded from before a keyless registration moved it ([`record`]).
    pub key_from: Option<IpAddr>,
    /// Its host name ([`is_host`]).
    pub host: String,
    /// The address it was last reached at, or registered from.
    pub addr: Option<IpAddr>,
    /// Its fleet port.
    pub port: u16,
    /// The aterm version it registered with.
    pub version: Option<String>,
    /// When it registered (Unix milliseconds): the last this book heard of it.
    pub seen_ms: Option<u64>,
    /// How it came into the book.
    pub via: Via,
    /// The fleet whose key this machine held when its join recorded the entry, read only
    /// while that key is held ([`read_peers`]); `None` reads as whatever fleet is held.
    pub fleet: Option<FleetMark>,
    /// Its node's own signature of it ([`Peer::signature_holds`]); `None` for an entry an older
    /// aterm recorded, and for a peer a person named.
    pub signed: Option<Signed>,
}

impl Peer {
    /// Attach a primary name to this node's signed entry. Invalid optional metadata leaves
    /// the ordinary entry intact, so it cannot prevent fleet membership.
    #[must_use]
    pub fn with_primary_name(mut self, me: &NodeIdentity, name: Option<&str>) -> Self {
        if let Some(signed) = self.signed
            && self.node.as_deref() == Some(me.node())
            && let Some(version) = self.version.as_deref()
        {
            self.signed = Some(
                Signed::named_by(me, (&self.host, self.port, version), signed.seq, name)
                    .map_or(signed, |named| named.with_record(signed.record)),
            );
        }
        self
    }
    /// `me`'s own entry at `host`, `port` and `version`, signed at self-seq `seq`: what a member
    /// serves of itself in its book ([`serve_book_at`]).
    #[must_use]
    pub fn own(me: &NodeIdentity, (host, port, version): (&str, u16, &str), seq: u64) -> Self {
        Self {
            node: Some(me.node().to_owned()),
            key: Some(me.key()),
            key_from: None,
            host: host.to_owned(),
            addr: None,
            port,
            version: Some(version.to_owned()),
            seen_ms: Some(seq),
            via: Via::Joined,
            fleet: None,
            signed: Some(Signed::by(me, (host, port, version), seq)),
        }
    }

    /// Whether its node signed it, under the key it names, as it stands ([`Signed::holds`]).
    #[must_use]
    pub fn signature_holds(&self) -> bool {
        match (&self.node, &self.key, &self.version, &self.signed) {
            (Some(node), Some(key), Some(version), Some(signed)) => {
                signed.holds(((node, key), (&self.host, self.port, version)))
            }
            _ => false,
        }
    }

    /// True when `self.seen_ms` is within [`LIVE_WITHIN_MS`] (`9 min`) of `now_ms`.
    #[inline]
    #[must_use]
    pub const fn is_live_at(&self, now_ms: u64) -> bool {
        match self.seen_ms {
            Some(seen) => now_ms.saturating_sub(seen) <= LIVE_WITHIN_MS,
            None => false,
        }
    }

    /// True when a non-manual peer has not been heard from for at least [`DORMANT_AFTER_MS`] (`14d`).
    #[inline]
    #[must_use]
    pub const fn is_dormant_at(&self, now_ms: u64) -> bool {
        match (self.via, self.seen_ms) {
            (Via::Manual, _) | (_, None) => false,
            (_, Some(seen)) => now_ms.saturating_sub(seen) >= DORMANT_AFTER_MS,
        }
    }

    /// True when a `joined` (or copied) peer has been silent for at least [`EXPIRE_AFTER_MS`] (`90d`).
    #[inline]
    #[must_use]
    pub const fn is_expired_at(&self, now_ms: u64) -> bool {
        match (self.via, self.seen_ms) {
            (Via::Joined | Via::IssuerBook, Some(seen)) => {
                now_ms.saturating_sub(seen) >= EXPIRE_AFTER_MS
            }
            _ => false,
        }
    }

    /// The peer as one line of the book: `key_from=`, `fleet=` and the signature's `seq=` and
    /// `sig=` only where there is one, which an older aterm passes over.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "peer node={} key={}{} host={} addr={} port={} version={} seen_ms={} via={}{}{}",
            self.node.as_deref().unwrap_or("-"),
            self.key.map_or_else(|| "-".to_owned(), |key| key.hex()),
            self.key_from
                .map_or_else(String::new, |from| format!(" key_from={from}")),
            aterm_codec::pct::encode(&self.host),
            self.addr.map_or_else(|| "-".to_owned(), |a| a.to_string()),
            self.port,
            self.version
                .as_deref()
                .map_or_else(|| "-".to_owned(), aterm_codec::pct::encode),
            self.seen_ms
                .map_or_else(|| "-".to_owned(), |ms| ms.to_string()),
            self.via.word(),
            self.fleet
                .map_or_else(String::new, |mark| format!(" fleet={}", mark.hex())),
            self.signed.as_ref().map_or_else(String::new, Signed::words),
        )
    }

    /// One line of the book as a peer; `None` when it is not one.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let fields = fields(line.strip_prefix("peer ")?);
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| *v)
                .filter(|v| *v != "-")
        };
        let node = match get("node") {
            Some(node) if is_node_id(node) => Some(node.to_owned()),
            Some(_) => return None,
            None => None,
        };
        let key = match get("key") {
            Some(key) if node.is_some() => Some(NodeKey::from_hex(key)?),
            Some(_) => return None,
            None => None,
        };
        let key_from = match get("key_from") {
            Some(from) => Some(from.parse::<IpAddr>().ok()?),
            None => None,
        }
        .filter(|_| node.is_some() && key.is_none());
        let host = aterm_codec::pct::decode(get("host")?);
        if !is_host(&host) {
            return None;
        }
        let addr = match get("addr") {
            Some(addr) => Some(addr.parse().ok()?),
            None => None,
        };
        let port = get("port")
            .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))?
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)?;
        let version = match get("version") {
            Some(v) => Some(version_word(&aterm_codec::pct::decode(v))?),
            None => None,
        };
        let seen_ms = match get("seen_ms") {
            Some(ms) => {
                if ms.is_empty() || !ms.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                Some(ms.parse().ok()?)
            }
            None => None,
        };
        let via = Via::from_word(get("via")?)?;
        if !matches!(via, Via::Manual | Via::IssuerBook) && node.is_none() {
            return None;
        }
        let fleet = match get("fleet") {
            Some(mark) => Some(FleetMark::from_hex(mark)?),
            None => None,
        };
        let signed = Signed::of_words(
            (get("seq"), get("sig")),
            (get("name"), get("name_sig")),
            (get("mname"), get("mname_sig")),
        )?
        .filter(|_| key.is_some());
        Some(Self {
            node,
            key,
            key_from,
            host,
            addr,
            port,
            version,
            seen_ms,
            via,
            fleet,
            signed,
        })
    }

    /// Where to dial it, as a person reads it: the address when there is one.
    #[must_use]
    pub fn target(&self) -> String {
        match self.addr {
            Some(addr) => host_port(&addr.to_string(), self.port),
            None => host_port(&self.host, self.port),
        }
    }

    /// Whether `other` is the same place: one port, and one address or one host name
    /// (`lab-mini` and `lab-mini.local` are one).
    #[must_use]
    pub fn same_place(&self, other: &Self) -> bool {
        self.port == other.port
            && ((self.addr.is_some() && self.addr == other.addr)
                || same_host(&self.host, &other.host))
    }

    /// Ranked match tier of `what` under the `--host` resolution order, without roster metadata.
    #[must_use]
    pub fn host_rank(&self, what: &str) -> Option<HostRank> {
        self.host_rank_with(what, None)
    }

    /// Ranked match tier of `what` under the `--host` resolution order:
    /// 1. signed primary name or roster `label`, 2. `alias`, 3. leading `m\d+` of `host`, 4. `vault_name`,
    /// 5. first DNS label (or full DNS host / `host:port`), 6. IP address (or `ip:port`),
    /// 7. node id or ≥8-hex-digit node id prefix. An explicit `node-` alias bypasses names.
    #[must_use]
    pub fn host_rank_with(&self, what: &str, identity: Option<&PeerIdentity>) -> Option<HostRank> {
        let what = what.trim();
        if what.is_empty() {
            return None;
        }
        if is_node_word(what)
            && what
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("node-"))
        {
            return self
                .node
                .as_deref()
                .filter(|node| names_node(node, what))
                .map(|_| HostRank::NodeId);
        }
        if self
            .primary_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(what))
        {
            return Some(HostRank::Label);
        }
        if identity
            .and_then(|id| id.label.as_deref())
            .is_some_and(|label| label.trim().eq_ignore_ascii_case(what))
        {
            return Some(HostRank::Label);
        }
        if identity.is_some_and(|id| {
            id.aliases
                .iter()
                .any(|alias| alias.trim().eq_ignore_ascii_case(what))
        }) {
            return Some(HostRank::Alias);
        }
        if leading_machine_label(&self.host)
            .is_some_and(|lead| lead.to_string().eq_ignore_ascii_case(what))
        {
            return Some(HostRank::LeadingMachine);
        }
        if identity
            .and_then(|id| id.vault_name.as_deref())
            .is_some_and(|vault_name| vault_name.trim().eq_ignore_ascii_case(what))
        {
            return Some(HostRank::VaultName);
        }
        let host_ip = self.host.parse::<IpAddr>().ok();
        if host_ip.is_none() {
            let short = self.host.split('.').next().unwrap_or(&self.host);
            if short.eq_ignore_ascii_case(what)
                || self.host.eq_ignore_ascii_case(what)
                || (self.addr.is_none()
                    && host_port(&self.host, self.port).eq_ignore_ascii_case(what))
            {
                return Some(HostRank::FirstDnsLabel);
            }
        }
        let matches_ip = |ip: IpAddr| {
            let ip_str = ip.to_string();
            ip_str.eq_ignore_ascii_case(what)
                || host_port(&ip_str, self.port).eq_ignore_ascii_case(what)
        };
        if self.addr.is_some_and(matches_ip) || host_ip.is_some_and(matches_ip) {
            return Some(HostRank::IpAddress);
        }
        if self
            .node
            .as_deref()
            .is_some_and(|node| names_node(node, what))
        {
            return Some(HostRank::NodeId);
        }
        None
    }

    /// The primary name proved by this entry's node key, independent of its network address.
    #[must_use]
    pub fn primary_name(&self) -> Option<&str> {
        self.signature_holds().then_some(())?;
        self.signed.as_ref()?.name.as_ref().map(Named::as_str)
    }

    /// Whether this peer matches `what` at any [`HostRank`] tier.
    #[must_use]
    pub fn answers_to(&self, what: &str) -> bool {
        self.host_rank(what).is_some()
    }

    /// Whether `what` is a short name of its host of `tier` ([`ShortName`]), in any case.
    fn answers_to_short(&self, what: &str, tier: ShortName) -> bool {
        let what = what.trim();
        !what.is_empty()
            && short_names(&self.host, tier)
                .iter()
                .any(|name| name.eq_ignore_ascii_case(what))
    }
}

/// Which tier of the `--host` resolution order a peer matched on (lowest variant wins).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HostRank {
    /// 1. Signed primary name or canonical swarm-roster label (`m<N>`).
    Label,
    /// 2. Legacy or folded swarm-roster alias (`m<N>`).
    Alias,
    /// 3. Leading `m\d+` token that the peer's host name starts with.
    LeadingMachine,
    /// 4. Machine name recorded in the vault.
    VaultName,
    /// 5. First DNS label (or full DNS host / `host:port`).
    FirstDnsLabel,
    /// 6. IP address (or `ip:port`).
    IpAddress,
    /// 7. Node id, ≥8-hex-digit node id prefix, or its explicit `node-` alias.
    NodeId,
}

/// Optional swarm-roster and vault names bound to a peer, consulted by [`Peer::host_rank_with`],
/// [`find_with`], [`named_with`] and [`all_named_with`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerIdentity {
    /// Node id this identity belongs to, when bound by node id.
    pub node: Option<String>,
    /// DNS host name fallback when matching an entry before node-id binding.
    pub host: Option<String>,
    /// Tier 1: canonical swarm label (`m<N>`).
    pub label: Option<String>,
    /// Tier 2: aliases (`m<N>`) that also resolve to this peer.
    pub aliases: Vec<String>,
    /// Tier 4: machine name recorded in the vault.
    pub vault_name: Option<String>,
}

/// The machine number `host` starts with ([`Number::leading`], the one rule): `m22` of
/// `m22-Macbook-M5-Max.local`; none for `Mac-mini-M4.local`, `m3a.local` or a zero-led tag.
#[must_use]
pub fn leading_machine_label(host: &str) -> Option<Number> {
    Number::leading(host)
}

fn identity_for<'a>(peer: &Peer, identities: &'a [PeerIdentity]) -> Option<&'a PeerIdentity> {
    if let Some(node) = peer.node.as_deref()
        && let Some(found) = identities.iter().find(|id| {
            id.node
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(node))
        })
    {
        return Some(found);
    }
    identities.iter().find(|id| {
        id.node.is_none()
            && id
                .host
                .as_deref()
                .is_some_and(|host| same_host(host, &peer.host))
    })
}

/// Which short names of a peer's host a word is read against ([`short_names`]), in the order
/// [`all_short_named`] reads them: a tier that names a peer ends the read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortName {
    /// Its first label's leading word, up to the first `-` or `.` (`m102` of
    /// `m102-macbook-M4-Pro-24GB-500GB.local`, `MacbookAir` of `MacbookAir-m27-M5-32GB-4TB.local`).
    Lead,
    /// Each later `-`-separated word of that label that is a lowercase `m` and digits alone
    /// (`m27` of `MacbookAir-m27-M5-32GB-4TB.local`), so a chip's `M2` or `M5` names no Mac.
    Number,
}

/// The short names of `tier` a peer's host answers to where no peer answers to the word
/// otherwise ([`all_short_named`], `aterm ctl --host m102`). An address has none.
#[must_use]
pub fn short_names(host: &str, tier: ShortName) -> Vec<&str> {
    if host.parse::<IpAddr>().is_ok() {
        return Vec::new();
    }
    let label = host.split('.').next().unwrap_or(host);
    let mut words = label.split('-');
    let lead = words.next().filter(|lead| !lead.is_empty());
    match tier {
        ShortName::Lead => lead.into_iter().collect(),
        ShortName::Number => {
            let mut names: Vec<&str> = Vec::new();
            for word in words {
                if is_machine_word(word) && !names.contains(&word) {
                    names.push(word);
                }
            }
            names
        }
    }
}

/// Whether `word` is a machine's number as the fleet names its Macs: a lowercase `m` and one
/// digit or more, nothing else (`m27`, never a chip's `M5`).
fn is_machine_word(word: &str) -> bool {
    word.strip_prefix('m')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `what` is shaped as a node id or the head of one ([`names_node`]): 8 to 32 hex digits,
/// optionally prefixed with `node-`, which no verb hands the system resolver as a host name.
#[must_use]
pub fn is_node_word(what: &str) -> bool {
    let what = what.trim();
    let what = if what
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("node-"))
    {
        &what[5..]
    } else {
        what
    };
    let what = what
        .strip_prefix("n-")
        .or_else(|| what.strip_prefix("N-"))
        .unwrap_or(what);
    (8..=32).contains(&what.len()) && what.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether `what` names the node `node`: its whole id, or 8 characters or more of its head, in any
/// case, optionally prefixed with `node-`.
#[must_use]
pub fn names_node(node: &str, what: &str) -> bool {
    let what = what.trim().to_ascii_lowercase();
    let what = what.strip_prefix("node-").unwrap_or(&what);
    let node = node.strip_prefix("n-").unwrap_or(node);
    let what = what.strip_prefix("n-").unwrap_or(what);
    node == what || (what.len() >= 8 && node.starts_with(what))
}

fn same_host(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    if a.parse::<IpAddr>().is_ok() || b.parse::<IpAddr>().is_ok() {
        return false;
    }
    let bare_of = |bare: &str, dotted: &str| {
        !bare.contains('.')
            && dotted
                .split_once('.')
                .is_some_and(|(label, _)| label.eq_ignore_ascii_case(bare))
    };
    bare_of(a, b) || bare_of(b, a)
}

fn version_word(version: &str) -> Option<String> {
    ((1..=64).contains(&version.len())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-' | b'_')))
    .then(|| version.to_owned())
}

fn fields(rest: &str) -> Vec<(&str, &str)> {
    rest.split(' ')
        .filter_map(|field| field.split_once('='))
        .collect()
}

/// The domain a node signs its book entry under ([`crate::fleet::record_message`]).
pub const ENTRY_DOMAIN: &str = "book entry";

/// A book entry's self-signature (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17): the node's
/// own key signed its node id, key, host, port and version at this self-seq, so any member passes
/// the entry on, and a reader trusts it as far as its membership log admits that key (with no log
/// yet, as far as its own book binds that node to no other key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signed {
    /// Its self-seq: a durable version for this node and key, initially at least the signer's
    /// clock in Unix milliseconds. A changed descriptor advances it even if the clock steps
    /// back; an unchanged descriptor can reuse it. It is not an observation timestamp.
    pub seq: u64,
    /// The node key's signature of the entry.
    pub sig: [u8; SIG_LEN],
    /// Optional primary name, independently signed for this exact entry. Older readers
    /// can still authenticate the original entry signature.
    name: Option<Named>,
    /// The node's name record as the entry carries it ([`name_record::Carried`]), not yet
    /// verified: its signature does not cover the entry, nor the entry's the record
    /// (`docs/DESIGN-machine-names-2026-11-04.md` §3.2), so a malformed one drops only itself.
    record: Option<name_record::Carried>,
}

/// A primary name's bounded text and the signature binding it to an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Named {
    bytes: [u8; 63],
    len: u8,
    sig: [u8; SIG_LEN],
}

impl Named {
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }

    fn parse(name: &str, sig: [u8; SIG_LEN]) -> Option<Self> {
        if !valid_primary_name(name) {
            return None;
        }
        let mut bytes = [0; 63];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Some(Self {
            bytes,
            len: u8::try_from(name.len()).ok()?,
            sig,
        })
    }
}

pub use aterm_types::fleet_name::{temporary_name, valid_primary_name};

/// The LAN IPv4 address to advertise for a wildcard fleet listener: the LAN route
/// before the default route. Public, loopback and IPv6 routes are not fleet endpoints.
#[must_use]
pub fn automatic_host(routes: [Option<IpAddr>; 3]) -> Option<String> {
    [routes[1], routes[0]].into_iter().flatten().find(|ip| {
        matches!(ip, IpAddr::V4(v4) if !v4.is_loopback() && (v4.is_private() || v4.is_link_local()))
    }).map(|ip| ip.to_string())
}

/// The private state file holding this node's chosen primary name.
pub const NAME_FILE: &str = "fleet-name";

/// The most [`NAME_FILE`] holds; a longer one holds no name.
pub const NAME_FILE_MAX: usize = 64;

/// Read this node's optional primary name from private local state.
/// # Errors
/// The file cannot be read privately, is not UTF-8, or contains an invalid name.
pub fn read_primary_name(state: &Path) -> Result<Option<String>, String> {
    let bytes = match read_private(state, NAME_FILE, NAME_FILE_MAX) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let name = std::str::from_utf8(&bytes)
        .map_err(|error| error.to_string())?
        .trim();
    if !valid_primary_name(name) {
        return Err("invalid primary fleet name".into());
    }
    Ok(Some(name.to_owned()))
}

/// What an entry's node, key, host, port and version are, as [`Signed`] covers them.
pub type Entry<'a> = ((&'a str, &'a NodeKey), (&'a str, u16, &'a str));

impl Signed {
    /// `me`'s signature of its own entry at `host`, `port` and `version`, at self-seq `seq`.
    #[must_use]
    pub fn by(me: &NodeIdentity, (host, port, version): (&str, u16, &str), seq: u64) -> Self {
        let key = me.key();
        Self {
            seq,
            sig: me.sign_record(
                ENTRY_DOMAIN,
                &entry_body(((me.node(), &key), (host, port, version)), seq),
            ),
            name: None,
            record: None,
        }
    }

    /// This signature carrying `record`, the node's name record for this entry, or none.
    #[must_use]
    pub const fn with_record(self, record: Option<name_record::Carried>) -> Self {
        Self { record, ..self }
    }

    /// The name record the entry carries, not yet verified ([`name_record::accept`]).
    #[must_use]
    pub const fn record(&self) -> Option<&name_record::Carried> {
        self.record.as_ref()
    }

    /// Sign an entry and its optional primary name. The temporary node alias always remains.
    /// # Errors
    /// The supplied primary name is invalid or reserved.
    pub fn named_by(
        me: &NodeIdentity,
        endpoint: (&str, u16, &str),
        seq: u64,
        name: Option<&str>,
    ) -> Result<Self, String> {
        let mut signed = Self::by(me, endpoint, seq);
        if let Some(name) = name {
            let key = me.key();
            let entry = ((me.node(), &key), endpoint);
            let sig = me.sign_record("book name", &name_body(entry, seq, name));
            signed.name = Some(Named::parse(name, sig).ok_or("invalid primary fleet name")?);
        }
        Ok(signed)
    }

    /// Whether it is the signature of `entry` by the entry's own key.
    #[must_use]
    pub fn holds(&self, entry: Entry<'_>) -> bool {
        crate::fleet::verifies_record(
            entry.0.1,
            ENTRY_DOMAIN,
            &entry_body(entry, self.seq),
            &self.sig,
        ) && self.name.as_ref().is_none_or(|name| {
            crate::fleet::verifies_record(
                entry.0.1,
                "book name",
                &name_body(entry, self.seq, name.as_str()),
                &name.sig,
            )
        })
    }

    fn words(&self) -> String {
        let mut words = format!(
            " seq={} sig={}",
            self.seq,
            aterm_codec::hex::encode_lower(&self.sig)
        );
        if let Some(name) = &self.name {
            words.push_str(&format!(
                " name={} name_sig={}",
                name.as_str(),
                aterm_codec::hex::encode_lower(&name.sig)
            ));
        }
        if let Some(record) = &self.record {
            words.push_str(&record.words());
        }
        words
    }

    /// The signature `seq=` and `sig=` spell: `Some(None)` when neither is there, `None` when
    /// they are malformed or one comes without the other. A primary name also requires its
    /// companion signature and the ordinary entry signature. A name record (`mname=` and
    /// `mname_sig=`) rides only on a signed entry, and one that is malformed, or half there,
    /// drops alone: the entry stands without it.
    fn of_words(
        (seq, sig): (Option<&str>, Option<&str>),
        (name, name_sig): (Option<&str>, Option<&str>),
        (mname, mname_sig): (Option<&str>, Option<&str>),
    ) -> Option<Option<Self>> {
        let record = name_record::Carried::of_words(mname, mname_sig);
        let name = match (name, name_sig) {
            (None, None) => None,
            (Some(name), Some(sig)) if aterm_codec::hex::is_lower(sig, 2 * SIG_LEN) => Some(
                Named::parse(name, crate::hex_decode(sig)?.try_into().ok()?)?,
            ),
            _ => return None,
        };
        match (seq, sig) {
            (None, None) if name.is_none() => Some(None),
            (Some(seq), Some(sig)) => {
                if seq.is_empty() || !seq.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let seq = seq.parse().ok()?;
                if !aterm_codec::hex::is_lower(sig, 2 * SIG_LEN) {
                    return None;
                }
                let sig = crate::hex_decode(sig)?.try_into().ok()?;
                Some(Some(Self {
                    seq,
                    sig,
                    name,
                    record,
                }))
            }
            _ => None,
        }
    }
}

fn name_body(entry: Entry<'_>, seq: u64, name: &str) -> Vec<u8> {
    let mut body = entry_body(entry, seq);
    body.push(0);
    body.extend_from_slice(name.as_bytes());
    body
}

fn entry_body(((node, key), (host, port, version)): Entry<'_>, seq: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(node.len() + host.len() + version.len() + 48);
    body.extend_from_slice(node.as_bytes());
    body.push(0);
    body.extend_from_slice(key.bytes());
    body.extend_from_slice(host.as_bytes());
    body.push(0);
    body.extend_from_slice(&port.to_be_bytes());
    body.extend_from_slice(version.as_bytes());
    body.push(0);
    body.extend_from_slice(&seq.to_be_bytes());
    body
}

/// Why the book could not be read.
#[derive(Debug)]
pub enum PeersError {
    /// The file breaks a private file's rules, or could not be read.
    File(PrivateFileError),
    /// This line (1-based) is no peer: the file was edited by hand.
    Line(usize),
}

impl fmt::Display for PeersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(e) => write!(f, "{PEERS_FILE}: {e}"),
            Self::Line(n) => write!(
                f,
                "{PEERS_FILE} line {n} is no peer record; fix it or remove the line"
            ),
        }
    }
}

impl std::error::Error for PeersError {}

/// The book of peers in the state directory `dir` as the fleet whose key `dir` holds reads
/// it: empty when there is none; entries marked with another fleet ([`Peer::fleet`]), or any
/// marked one when no key is held, and [`Via::IssuerBook`] lines are left out.
pub fn read_peers(dir: &Path) -> Result<Vec<Peer>, PeersError> {
    Ok(read_book(dir, held_mark(dir))?.0)
}

/// The book in `dir` as the fleet `fleet` reads it, whatever key `dir` holds: its entries of
/// that fleet, and those marked with none.
pub fn read_peers_as(dir: &Path, fleet: FleetMark) -> Result<Vec<Peer>, PeersError> {
    Ok(read_book(dir, Some(fleet))?.0)
}

/// Whether the book in `dir` names an issuer other than `own`, of whichever fleet it marks: a machine whose book may list this one still.
#[must_use]
pub fn names_an_issuer(dir: &Path, own: Option<&str>) -> bool {
    read_book(dir, held_mark(dir)).is_ok_and(|(held, other)| {
        held.iter().chain(&other).any(|peer| {
            peer.via == Via::Issuer && peer.node.is_some() && peer.node.as_deref() != own
        })
    })
}

/// The mark of the fleet whose key `dir` holds, when it holds one it can read.
fn held_mark(dir: &Path) -> Option<FleetMark> {
    crate::fleet::read_key_file(dir)
        .ok()
        .map(|secret| secret.mark())
}

/// The book in `dir` as the fleet `held` marks it: its entries of that fleet
/// ([`read_peers`]), then those of another fleet.
fn read_book(dir: &Path, held: Option<FleetMark>) -> Result<(Vec<Peer>, Vec<Peer>), PeersError> {
    let bytes = match read_private(dir, PEERS_FILE, PEERS_FILE_MAX) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok((Vec::new(), Vec::new())),
        Err(e) => return Err(PeersError::File(e)),
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut book = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let peer = Peer::parse(line).ok_or(PeersError::Line(n + 1))?;
        if peer.via != Via::IssuerBook {
            book.push(peer);
        }
    }
    Ok(book
        .into_iter()
        .partition(|peer| peer.fleet.is_none_or(|mark| Some(mark) == held)))
}

const ISSUER_BOOK_HEAD: &str = "issuer node=";

/// Whether `dir` holds a copy of the book `issuer` shared ([`ISSUER_BOOK_FILE`] whose head
/// names that node), even an empty one: the last whole read got that issuer's book. An issuer
/// that shares none (an older aterm), or a member that never read one, leaves none.
#[must_use]
pub fn holds_issuer_book(dir: &Path, issuer: &str) -> bool {
    read_private(dir, ISSUER_BOOK_FILE, PEERS_FILE_MAX).is_ok_and(|bytes| {
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .and_then(|head| head.strip_prefix(ISSUER_BOOK_HEAD))
            == Some(issuer)
    })
}

/// The member's copy of its issuer's book in `dir` ([`ISSUER_BOOK_FILE`]), at most
/// [`PEERS_MAX`]: empty unless the file is private, names `issuer` (the node this machine's
/// book names as its issuer) at its head, and holds only copies.
#[must_use]
pub fn read_issuer_book(dir: &Path, issuer: &str) -> Vec<Peer> {
    let Ok(bytes) = read_private(dir, ISSUER_BOOK_FILE, PEERS_FILE_MAX) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    if lines
        .next()
        .and_then(|head| head.strip_prefix(ISSUER_BOOK_HEAD))
        != Some(issuer)
    {
        return Vec::new();
    }
    let copies: Option<Vec<Peer>> = lines
        .map(|line| Peer::parse(line).filter(|peer| peer.via == Via::IssuerBook))
        .collect();
    let mut copies = copies.unwrap_or_default();
    copies.truncate(PEERS_MAX);
    copies
}

/// The word [`ANSWERED_FILE`] keys `peer`'s answers by: `node=<id>`, or `place=<host:port>` for
/// a peer with no node id (one a person named), so a write and a lookup of one entry meet.
#[must_use]
pub fn answer_key(peer: &Peer) -> String {
    match &peer.node {
        Some(node) => node_answer_key(node),
        None => format!("place={}", peer.target()),
    }
}

/// The [`answer_key`] of the member whose node id is `node`.
fn node_answer_key(node: &str) -> String {
    format!("node={node}")
}

/// The first word of a line of [`ANSWERED_FILE`] that says a member answered.
const ANSWERED: &str = "answered";

/// Times [`HeldBook::note_stamps`] notes, each a member's key ([`answer_key`]) and a time (Unix
/// milliseconds).
pub type Stamped = [(String, u64)];

/// Each member's key ([`answer_key`]) with one kind of time [`ANSWERED_FILE`] holds for it, as
/// [`read_answered`], [`read_missed`] and [`read_missed_heard`] read them.
type Stamps = std::collections::BTreeMap<String, u64>;

/// The first word of a line of [`ANSWERED_FILE`] that says a heartbeat's dial of a member missed.
const MISSED: &str = "missed";

/// The first word of a line of [`ANSWERED_FILE`] that says a heartbeat's dial of a member marked
/// missed missed again while it was heard from since its miss before ([`read_missed_heard`]).
const MISSED_HEARD: &str = "missed-heard";

/// One line of [`ANSWERED_FILE`] whose first word is `word`: its key ([`answer_key`]) and time,
/// `None` for any other line.
fn stamp_line<'a>(word: &str, line: &'a str) -> Option<(&'a str, u64)> {
    let mut words = line.split_ascii_whitespace();
    if words.next()? != word {
        return None;
    }
    let key = words.next()?;
    let ms = words
        .next()?
        .strip_prefix("ms=")
        .filter(|ms| !ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()?;
    let keyed = match key.split_once('=') {
        Some(("node", node)) => is_node_id(node),
        Some(("place", place)) => !place.is_empty() && place.len() <= RECORD_MAX,
        _ => false,
    };
    (keyed && words.next().is_none()).then_some((key, ms))
}

/// When each member last answered this machine, as [`ANSWERED_FILE`] in `dir` holds it: each key
/// ([`answer_key`]) with its newest time (Unix milliseconds). Empty where the file is missing,
/// not private, or holds no such line; a line it cannot read is passed over, and so is a time
/// more than [`ANSWER_AHEAD_MAX_MS`] past this machine's clock (none is where the clock reads
/// before 1970).
#[must_use]
pub fn read_answered(dir: &Path) -> std::collections::BTreeMap<String, u64> {
    read_stamps(dir, ANSWERED)
}

/// When a heartbeat round last dialled each member that did not answer it, as [`ANSWERED_FILE`]
/// in `dir` holds it, read as [`read_answered`] reads the answers.
#[must_use]
pub fn read_missed(dir: &Path) -> std::collections::BTreeMap<String, u64> {
    read_stamps(dir, MISSED)
}

/// When a heartbeat round's dial of each member marked missed last missed while the member was
/// heard from since its miss before (its entry's `seen_ms` was later), as [`ANSWERED_FILE`] in
/// `dir` holds it, read as [`read_answered`] reads the answers: such a member reaches some Mac
/// and not this one's dial, and being heard from puts it first among the members marked missed
/// no more ([`round_order`]) until a miss of it comes while nobody heard from it.
#[must_use]
pub fn read_missed_heard(dir: &Path) -> std::collections::BTreeMap<String, u64> {
    read_stamps(dir, MISSED_HEARD)
}

/// The lines of [`ANSWERED_FILE`] in `dir` whose first word is `word`, as [`read_answered`] reads
/// them.
fn read_stamps(dir: &Path, word: &str) -> std::collections::BTreeMap<String, u64> {
    let mut stamps = std::collections::BTreeMap::new();
    let Ok(bytes) = read_private(dir, ANSWERED_FILE, PEERS_FILE_MAX) else {
        return stamps;
    };
    let latest = unix_ms_now().map(|now| now.saturating_add(ANSWER_AHEAD_MAX_MS));
    for (key, ms) in String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| stamp_line(word, line))
        .filter(|(_, ms)| latest.is_none_or(|latest| *ms <= latest))
    {
        let at = stamps.entry(key.to_owned()).or_insert(ms);
        *at = (*at).max(ms);
    }
    stamps
}

/// Keep what a read of the issuer's book under `secret` made of it as the copy in `dir` ([`HeldBook::keep_issuer_book`]) by [`write_held`], while that fleet is held: whether it kept it.
/// # Errors
/// The book's lock cannot be taken, or the copy cannot be written or removed.
pub fn keep_issuer_book(
    dir: &Path,
    secret: &FleetSecret,
    copy: Option<(&str, &[Peer])>,
) -> Result<bool, String> {
    match write_held(dir, Began::Under(&Held::Key(secret.clone())), |book| {
        book.keep_issuer_book(copy)
    }) {
        Ok(kept) => kept
            .map(|()| true)
            .map_err(|e| format!("{ISSUER_BOOK_FILE}: {e}")),
        Err(Unwritten::Moved) => Ok(false),
        Err(Unwritten::Lock(why)) => Err(why),
    }
}

fn book_text(book: &[Peer]) -> String {
    let mut text = String::from(
        "# The fleet's peers, one per line: written by aterm (`aterm fleet peers`).\n",
    );
    for peer in book {
        text.push_str(&peer.line());
        text.push('\n');
    }
    text
}

/// Rewrite under the identity/book lock. Persist aliases before either book so a crash
/// cannot lose the binding retained mail needs. No grant or membership-log record is edited.
fn rewrite_mesh_book(dir: &Path) -> io::Result<usize> {
    const ALIASES: &str = "mesh-node-aliases";
    let judged = aterm_uds::private_file::JudgedDir::judge(dir)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let mut aliases = match judged.read(ALIASES, PEERS_FILE_MAX) {
        Ok(bytes) => String::from_utf8(bytes).map_err(io::Error::other)?,
        Err(PrivateFileError::Missing) => String::new(),
        Err(e) => return Err(io::Error::other(e.to_string())),
    };
    let mut files = Vec::new();
    let mut changed = 0;
    for name in [PEERS_FILE, ISSUER_BOOK_FILE] {
        let bytes = match judged.read(name, PEERS_FILE_MAX) {
            Ok(bytes) => bytes,
            Err(PrivateFileError::Missing) => continue,
            Err(e) => return Err(io::Error::other(e.to_string())),
        };
        let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
        let mut lines = Vec::new();
        let mut count = 0;
        for line in text.lines() {
            if let Some(mut peer) = Peer::parse(line)
                && let (Some(old), Some(key)) = (&peer.node, peer.key)
            {
                let new = node_principal(key);
                if old != &new {
                    let alias = format!("{old} {new}\n");
                    if !aliases.lines().any(|line| line == alias.trim_end()) {
                        aliases.push_str(&alias);
                    }
                    peer.node = Some(new);
                    // The signature covers the old id. Only that peer can refresh it.
                    peer.signed = None;
                    lines.push(peer.line());
                    count += 1;
                    continue;
                }
            }
            lines.push(line.to_owned());
        }
        files.push((name, lines, count));
        changed += count;
    }
    if changed > 0 {
        write_private(dir, ALIASES, aliases.as_bytes())?;
    }
    for (name, mut lines, count) in files {
        // The issuer header is a reference too, resolved only by a key-bound alias.
        let mut header_changed = false;
        for line in &mut lines {
            if let Some(old) = line.strip_prefix(ISSUER_BOOK_HEAD)
                && let Some(new) = aliases
                    .lines()
                    .filter_map(|a| a.split_once(' '))
                    .find_map(|(from, to)| (from == old).then_some(to))
            {
                *line = format!("{ISSUER_BOOK_HEAD}{new}");
                header_changed = true;
            }
        }
        if count > 0 || header_changed {
            write_private(dir, name, format!("{}\n", lines.join("\n")).as_bytes())?;
        }
    }
    // A local journal contains counts only, never keys, ids or mail contents.
    if changed > 0 {
        write_private(
            dir,
            "mesh-node-rewrite",
            format!("book_entries={changed}\n").as_bytes(),
        )?;
    }
    Ok(changed)
}

/// Edit the book in `dir` ([`HeldBook::update`]) as a verb's first write ([`write_held`]).
///
/// # Errors
/// The lock cannot be taken, the book cannot be read or written, or `edit` refuses.
pub fn update_peers<R>(
    dir: &Path,
    edit: impl FnOnce(&mut Vec<Peer>) -> Result<R, String>,
) -> Result<R, String> {
    write_held(dir, Began::Here, |book| book.update(edit)).map_err(|e| e.to_string())?
}

/// The fleet a state directory holds, as [`write_held`] compares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    /// No key: no fleet.
    Nothing,
    /// The fleet of this secret.
    Key(FleetSecret),
    /// A key file that breaks the key file's rules.
    Unreadable,
}

impl Held {
    /// The fleet `dir` holds now.
    #[must_use]
    pub fn read(dir: &Path) -> Self {
        match crate::fleet::read_key_file(dir) {
            Ok(secret) => Self::Key(secret),
            Err(crate::fleet::KeyFileError::Missing) => Self::Nothing,
            Err(_) => Self::Unreadable,
        }
    }

    /// Its secret, when it is a fleet's.
    #[must_use]
    pub const fn secret(&self) -> Option<&FleetSecret> {
        match self {
            Self::Key(secret) => Some(secret),
            Self::Nothing | Self::Unreadable => None,
        }
    }
}

/// What a write of fleet state is made under ([`write_held`]).
#[derive(Clone, Copy, Debug)]
pub enum Began<'a> {
    /// A verb's first write: it reads again under the lock whatever it read before, and the
    /// fleet it finds there ([`HeldBook::held`]) is the one its later writes are made under.
    Here,
    /// A later write, or one a read or a served connection makes under a fleet's key: only
    /// while the state still holds that fleet.
    Under(&'a Held),
}

/// Why [`write_held`] wrote nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum Unwritten {
    /// The book's lock was not taken, for this reason.
    Lock(String),
    /// The state holds another fleet than the write was made under.
    Moved,
}

impl fmt::Display for Unwritten {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lock(why) => f.write_str(why),
            Self::Moved => f.write_str(
                "this machine holds another fleet than the one this began under: an `aterm fleet \
                 leave`, or a join of another code, came between",
            ),
        }
    }
}

impl std::error::Error for Unwritten {}

/// THE WRITE RULE (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5): every write of fleet state — [`KEY_FILE`], [`CODE_FILE`], [`PEERS_FILE`], [`ISSUER_BOOK_FILE`], [`ANSWERED_FILE`], [`fleet_seed::FILE`], [`fleet_seed::CONFLICT_FILE`], [`fleet_seed::UNHELD_FILE`], [`OFF_FILE`], [`VAULT_FILE`], `fleet sync`'s two records and aterm.toml's `[fleet]` — is made in `write`, under the book's lock in `dir` and only while `dir` holds the fleet `began` names, from what `write` reads under it; a leave's is [`leave_held`]'s, and a level's [`level_held`]'s. The files are written by the [`HeldBook`] `write` is handed, and sync's records and aterm.toml's `[fleet]` beside it, by convention. The lock is taken once per call, never inside another, waited for at most two seconds ([`LOCK_WAIT`]; a debug build's test seams aside); aterm.toml's write lock, where `write` takes it, comes after it.
/// # Errors
/// [`Unwritten::Lock`]: another writer holds the lock past its wait, the lock cannot be
/// taken, or a leave's directory open to others cannot be narrowed; [`Unwritten::Moved`]:
/// `dir` holds another fleet than `began` names ([`Began::Under`]).
pub fn write_held<R>(
    dir: &Path,
    began: Began<'_>,
    write: impl FnOnce(&HeldBook<'_>) -> R,
) -> Result<R, Unwritten> {
    let lock =
        BookLock::take(dir, lock_wait()).map_err(|e| Unwritten::Lock(lock_refused(dir, &e)))?;
    let book = HeldBook {
        dir,
        held: Held::read(dir),
        _lock: lock,
    };
    if let Began::Under(began) = began
        && *began != book.held
    {
        return Err(Unwritten::Moved);
    }
    Ok(write(&book))
}

/// A leave's write, a first write of its own ([`write_held`]): the directory made private first, as an init or a join makes it (made where there is none, narrowed where it is open to others: [`Leaving::narrowed`]), then the book's lock taken there (one that is there but could not be made private decided by its lock, as any directory is), and gone on without the lock only where no writer could make the directory or take the lock either ([`stops_every_edit`]), so no odd state directory keeps a machine in a fleet. `remove` is handed a [`Leaving`], which removes the fleet's files and writes only [`OFF_FILE`].
/// # Errors
/// [`Unwritten::Lock`]: the directory cannot be made or narrowed, or the lock taken, for a reason that does not stop every writer ([`stops_every_edit`]): another writer holding the lock past its wait, the directory removed under it, no descriptor or space free among them.
pub fn leave_held<R>(dir: &Path, remove: impl FnOnce(&Leaving<'_>) -> R) -> Result<R, Unwritten> {
    let (narrowed, lock) = unnamed_held(dir)?;
    Ok(remove(&Leaving {
        dir,
        narrowed,
        _lock: lock,
    }))
}

/// A level's write (`allow facts|content`), which names no fleet either: held as a leave's is ([`leave_held`]), so no odd state directory blocks it. `write` is handed a [`Leveling`], which writes none of the fleet's files.
/// # Errors
/// As [`leave_held`]'s.
pub fn level_held<R>(dir: &Path, write: impl FnOnce(&Leveling) -> R) -> Result<R, Unwritten> {
    let (narrowed, lock) = unnamed_held(dir)?;
    Ok(write(&Leveling {
        narrowed,
        _lock: lock,
    }))
}

/// [`leave_held`]'s and [`level_held`]'s hold: the mode the directory had where it was narrowed, and the book's lock where any writer could take it.
/// # Errors
/// As [`leave_held`]'s.
fn unnamed_held(dir: &Path) -> Result<(Option<u32>, Option<BookLock>), Unwritten> {
    let narrowed = match judge_private_dir(dir) {
        Ok(PrivateDir::Missing | PrivateDir::Open(_)) => match ensure_private_dir(dir) {
            Ok(narrowed) => narrowed,
            // None to make, by any writer: none to lock either.
            Err(e)
                if stops_every_edit(&e)
                    && matches!(judge_private_dir(dir), Ok(PrivateDir::Missing)) =>
            {
                return Ok((None, None));
            }
            // There but not made private (an access list it inherits, a volume that ignores modes): its lock decides, as any directory's does.
            Err(e) if stops_every_edit(&e) => None,
            Err(e) => return Err(Unwritten::Lock(e.to_string())),
        },
        _ => None,
    };
    let lock = match BookLock::take(dir, lock_wait()) {
        Ok(lock) => Some(lock),
        Err(e) if stops_every_edit(&e) => None,
        Err(e) => {
            let why = lock_refused(dir, &e);
            return Err(Unwritten::Lock(match narrowed {
                Some(was) => format!(
                    "{why}; the directory was open to other users ({was:o}), and is 0700 now"
                ),
                None => why,
            }));
        }
    };
    Ok((narrowed, lock))
}

/// The book's lock in a state directory, held while this lives ([`write_held`]): no other writer, an older aterm's edit of the book included, comes between the writes made under it. Through this API it alone writes the fleet's files ([`KEY_FILE`], [`CODE_FILE`], [`PEERS_FILE`], [`ISSUER_BOOK_FILE`], [`ANSWERED_FILE`], [`fleet_seed::FILE`], [`fleet_seed::CONFLICT_FILE`], [`fleet_seed::UNHELD_FILE`], [`VAULT_FILE`]), so none is written without the lock; a leave removes them by a [`Leaving`].
pub struct HeldBook<'a> {
    dir: &'a Path,
    held: Held,
    _lock: BookLock,
}

/// A leave's hold on a state directory ([`leave_held`]): the book's lock, or none where no writer could take it either. It removes the fleet's files and writes only [`OFF_FILE`] and [`fleet_seed::DIGEST_FILE`].
pub struct Leaving<'a> {
    dir: &'a Path,
    narrowed: Option<u32>,
    _lock: Option<BookLock>,
}

/// A level's hold on a state directory ([`level_held`]): the book's lock, or none where no writer could take it either. It writes none of the fleet's files.
pub struct Leveling {
    narrowed: Option<u32>,
    _lock: Option<BookLock>,
}

impl Leveling {
    /// The mode the state directory had before it was narrowed, where it was open to others.
    #[must_use]
    pub const fn narrowed(&self) -> Option<u32> {
        self.narrowed
    }
}

impl Leaving<'_> {
    /// The mode the state directory had before it was narrowed, where it was open to others.
    #[must_use]
    pub const fn narrowed(&self) -> Option<u32> {
        self.narrowed
    }

    /// Whether it holds the book's lock: without it, no writer can make the directory or take the
    /// lock either ([`leave_held`]), so none writes a fleet there.
    #[must_use]
    pub const fn locked(&self) -> bool {
        self._lock.is_some()
    }

    /// Remove the state directory's file `name`: whether it was there.
    /// # Errors
    /// `name` is no one file name, or the file is there and cannot be removed.
    pub fn remove(&self, name: &str) -> io::Result<bool> {
        match remove_private(self.dir, name) {
            Err(e)
                if !self.locked() && stops_every_edit(&e) && check_owned_dir(self.dir).is_err() =>
            {
                Ok(false)
            }
            other => other,
        }
    }

    /// Keep the digest of the fleet seed this node holds in [`fleet_seed::DIGEST_FILE`], beside
    /// the node key, before the seed goes with the fleet's files, so Floor verifies this node's
    /// own records of that fleet in a book it reads later or its own `name-revs` row
    /// (`docs/DESIGN-machine-names-2026-11-04.md` §3.2): whether there was a seed. A seed file
    /// that cannot be read keeps no digest and stops no leave.
    /// # Errors
    /// The digest cannot be written, or `dir` is not an owned real directory.
    pub fn keep_seed_digest(&self) -> io::Result<bool> {
        let Ok(Some(seed)) = fleet_seed::read(self.dir) else {
            return Ok(false);
        };
        check_owned_dir(self.dir)?;
        aterm_uds::atomic_file::write(
            &self.dir.join(fleet_seed::DIGEST_FILE),
            fleet_seed::digest_line(seed.digest()).as_bytes(),
            0o600,
        )?;
        Ok(true)
    }

    /// Write [`OFF_FILE`], empty, in whatever directory the leave holds.
    /// # Errors
    /// It cannot be written, or `dir` is not an owned real directory.
    pub fn write_off(&self) -> io::Result<()> {
        #[cfg(any(test, debug_assertions))]
        if let Some(kind) = OFF_REFUSED.get() {
            return Err(io::Error::from(kind));
        }
        check_owned_dir(self.dir)?;
        aterm_uds::atomic_file::write(&self.dir.join(OFF_FILE), b"", 0o600)
    }
}

/// The book's lock alone, as another aterm's edit holds it: a test's stand-in for that edit and its writes, which no verb takes.
/// # Errors
/// Another edit holds it past the lock's wait, or it cannot be taken.
#[cfg(any(test, debug_assertions))]
pub fn hold_book(dir: &Path) -> Result<HeldBook<'_>, String> {
    let lock = BookLock::take(dir, lock_wait()).map_err(|e| lock_refused(dir, &e))?;
    Ok(HeldBook {
        dir,
        held: Held::read(dir),
        _lock: lock,
    })
}

/// Whether another take holds the book's lock in `dir` now (a test seam).
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn book_held(dir: &Path) -> bool {
    matches!(BookLock::take(dir, Duration::ZERO), Err(e) if e.kind() == io::ErrorKind::WouldBlock)
}

impl HeldBook<'_> {
    /// The fleet the state held as the lock was taken.
    #[must_use]
    pub const fn held(&self) -> &Held {
        &self.held
    }

    /// Edit the book: read it ([`read_peers`]), run `edit`, and write it back whole when `edit` changed it or a key is held beside entries of another fleet, those entries kept while no key is held and dropped while one is (by an `edit` that changes nothing too), so an older aterm, which reads no [`Peer::fleet`], never reads the old issuer as the new key's.
    /// # Errors
    /// The book cannot be read or written, or `edit` refuses.
    pub fn update<R>(
        &self,
        edit: impl FnOnce(&mut Vec<Peer>) -> Result<R, String>,
    ) -> Result<R, String> {
        let held = held_mark(self.dir);
        let (mut book, mut others) = read_book(self.dir, held).map_err(|e| e.to_string())?;
        let before = book.clone();
        let out = edit(&mut book)?;
        let drops = held.is_some() && !others.is_empty();
        if book != before || drops {
            if held.is_none() {
                book.append(&mut others);
            }
            write_private(self.dir, PEERS_FILE, book_text(&book).as_bytes())
                .map_err(|e| format!("{PEERS_FILE}: {e}"))?;
        }
        Ok(out)
    }

    /// Keep `copy` as the copy of the issuer's book ([`ISSUER_BOOK_FILE`]), whole: the copies [`copies_of`] made of the answer of the issuer `issuer` (a node id), or none for `None`.
    /// # Errors
    /// The copy cannot be written or removed.
    pub fn keep_issuer_book(&self, copy: Option<(&str, &[Peer])>) -> io::Result<()> {
        let Some((issuer, copies)) = copy else {
            return self.remove(ISSUER_BOOK_FILE).map(|_| ());
        };
        let mut text = format!(
            "# A copy of this machine's fleet issuer's book: written by aterm at each read of \
             that book (`aterm fleet peers`, `aterm ctl survey --fleet`, a heartbeat that reaches \
             it), never by hand.\n\
             {ISSUER_BOOK_HEAD}{issuer}\n"
        );
        for copy in copies.iter().filter(|copy| copy.via == Via::IssuerBook) {
            text.push_str(&copy.line());
            text.push('\n');
        }
        write_private(self.dir, ISSUER_BOOK_FILE, text.as_bytes())
    }

    /// Note `answers`, each a member's key ([`answer_key`]) and the time it answered this machine
    /// (Unix milliseconds), in [`ANSWERED_FILE`]: the later time stands for each key, over any
    /// time [`read_answered`] reads as none (one past this machine's clock), and past
    /// [`PEERS_MAX`] keys the oldest answers go. Written only when it changed; a key that is no
    /// [`answer_key`] is passed over. The misses the file holds ([`read_missed`]) are kept.
    /// # Errors
    /// The file cannot be written.
    pub fn note_answered(&self, answers: &[(String, u64)]) -> io::Result<()> {
        self.note_stamps(answers, (&[], &[]))
    }

    /// [`Self::note_answered`] for `answers`, and the same for `misses`, each a member a
    /// heartbeat round dialled and the time it did not answer ([`read_missed`]), and for
    /// `heard`, each of those misses that came while the member was marked missed and heard from
    /// since ([`read_missed_heard`]), in one write.
    /// # Errors
    /// The file cannot be written.
    pub fn note_stamps(
        &self,
        answers: &Stamped,
        (misses, heard): (&Stamped, &Stamped),
    ) -> io::Result<()> {
        let noted = |word: &str,
                     stamps: &mut std::collections::BTreeMap<String, u64>,
                     new: &[(String, u64)]| {
            for (key, ms) in new {
                if stamp_line(word, &format!("{word} {key} ms={ms}")).is_none() {
                    continue;
                }
                let at = stamps.entry(key.clone()).or_insert(*ms);
                *at = (*at).max(*ms);
            }
        };
        let (mut answered, mut missed, mut missed_heard) = (
            read_answered(self.dir),
            read_missed(self.dir),
            read_missed_heard(self.dir),
        );
        let before = (answered.clone(), missed.clone(), missed_heard.clone());
        noted(ANSWERED, &mut answered, answers);
        noted(MISSED, &mut missed, misses);
        noted(MISSED_HEARD, &mut missed_heard, heard);
        if (&answered, &missed, &missed_heard) == (&before.0, &before.1, &before.2) {
            return Ok(());
        }
        let rows = |stamps: std::collections::BTreeMap<String, u64>| {
            let mut rows: Vec<(String, u64)> = stamps.into_iter().collect();
            if rows.len() > PEERS_MAX {
                rows.sort_by(|(a_key, a_ms), (b_key, b_ms)| b_ms.cmp(a_ms).then(a_key.cmp(b_key)));
                rows.truncate(PEERS_MAX);
                rows.sort();
            }
            rows
        };
        let mut text = String::from(
            "# When each fleet member last answered this machine, when a heartbeat's dial of one \
             last missed, and when one missed while it was heard from elsewhere: written by aterm \
             at each answer (`aterm fleet peers`, `aterm ctl survey --fleet`, a heartbeat) and \
             each heartbeat, never by hand.\n",
        );
        for (word, stamps) in [
            (ANSWERED, answered),
            (MISSED, missed),
            (MISSED_HEARD, missed_heard),
        ] {
            for (key, ms) in rows(stamps) {
                text.push_str(&format!("{word} {key} ms={ms}\n"));
            }
        }
        write_private(self.dir, ANSWERED_FILE, text.as_bytes())
    }

    /// Write `secret` as this machine's [`KEY_FILE`], in the one spelling `fleet::read_key_file` reads (lowercase hex and a newline), whole: the listener reads the key every 2 s, and never sees half of one.
    /// # Errors
    /// `secret` is no fleet secret, or the key cannot be written.
    pub fn write_key(&self, secret: &[u8]) -> io::Result<()> {
        FleetSecret::new(secret)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let mut text = aterm_codec::hex::encode_lower(secret);
        text.push('\n');
        write_private(self.dir, KEY_FILE, text.as_bytes())
    }

    /// Write `code` as the join code this machine issues ([`CODE_FILE`]), whole.
    /// # Errors
    /// The code cannot be written.
    pub fn write_code(&self, code: &JoinCode) -> io::Result<()> {
        write_private(
            self.dir,
            CODE_FILE,
            format!("{}\n", code.written()).as_bytes(),
        )
    }

    /// Keep `digest`, the seed's of the fleet this node moves away from, in
    /// [`fleet_seed::DIGEST_FILE`], as a leave keeps it ([`Leaving::keep_seed_digest`]).
    /// # Errors
    /// It cannot be written.
    pub fn keep_seed_digest(&self, digest: [u8; 8]) -> io::Result<()> {
        write_private(
            self.dir,
            fleet_seed::DIGEST_FILE,
            fleet_seed::digest_line(digest).as_bytes(),
        )
    }

    /// Keep `seed` as the fleet seed this node holds ([`fleet_seed::FILE`]), whole, or none for
    /// `None`, which also drops the record of a seed refused against it
    /// ([`fleet_seed::CONFLICT_FILE`]), a diagnostic whose removal fails nothing.
    /// # Errors
    /// It cannot be written or removed.
    pub fn keep_seed(&self, seed: Option<&fleet_seed::FleetSeed>) -> io::Result<()> {
        match seed {
            Some(seed) => write_private(
                self.dir,
                fleet_seed::FILE,
                format!("{}\n", seed.words()).as_bytes(),
            ),
            None => {
                self.remove(fleet_seed::FILE)?;
                let _ = self.remove(fleet_seed::CONFLICT_FILE);
                Ok(())
            }
        }
    }

    /// Keep `text` as the record of the vault this machine's fleet state was taken under
    /// ([`VAULT_FILE`]), whole, or none for `None`.
    /// # Errors
    /// It is past [`VAULT_FILE_MAX`], or it cannot be written or removed.
    pub fn keep_vault(&self, text: Option<&str>) -> io::Result<()> {
        match text {
            Some(text) if text.len() > VAULT_FILE_MAX => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{VAULT_FILE} would hold more than {VAULT_FILE_MAX} bytes"),
            )),
            Some(text) => write_private(self.dir, VAULT_FILE, text.as_bytes()),
            None => self.remove(VAULT_FILE).map(|_| ()),
        }
    }

    /// Adopt, of `offered`, the fleet seeds members' book replies relayed in their trailers
    /// (`docs/DESIGN-machine-names-2026-11-04.md` §3.3), each one [`fleet_seed::judge`] takes
    /// over the seed held then, once it verifies under `mark`, the mark of the secret held here,
    /// and the key this node binds to its minter: its own, else its book's or its copy of its
    /// issuer's book's ([`Keyring::read`]). A seed whose minter no key here binds waits for a later
    /// read. A root (it issues the join code, which only `init` and its own rekey write, and its
    /// book names no issuer) adopts only a seed it minted, which another member can hold only
    /// when this root lost its file: another minter's is a conflict to it, held seed or none.
    /// Another minter's seed is refused, the last one recorded in [`fleet_seed::CONFLICT_FILE`]
    /// at `now_ms`, once. A seed file that cannot be read stands; one that holds no seed is replaced,
    /// and so is a held seed that no longer verifies under `mark` and the key bound to its minter
    /// (a rekey whose root could not sign it again): it is no seed of this fleet's, so a member
    /// takes the one its root holds now rather than keeping one nobody can sign for this secret.
    /// Returns the seed it adopted, if any.
    /// # Errors
    /// The seed or the record cannot be written.
    pub fn adopt_relayed_seed(
        &self,
        me: &NodeIdentity,
        mark: FleetMark,
        offered: impl IntoIterator<Item = fleet_seed::FleetSeed>,
        now_ms: u64,
    ) -> io::Result<Option<fleet_seed::FleetSeed>> {
        use fleet_seed::Judged;
        let mut offered = offered.into_iter().peekable();
        if offered.peek().is_none() {
            return Ok(None);
        }
        let mut held = match fleet_seed::read(self.dir) {
            Ok(held) => held,
            Err(e) if e.kind() == io::ErrorKind::InvalidData => None,
            Err(_) => return Ok(None),
        };
        let own = name_record::node_of(me.node());
        let root = self.is_root(me);
        let keys = Keyring::read(self.dir);
        let key_of = |seed: &fleet_seed::FleetSeed| {
            if Some(seed.minter()) == own {
                Some(me.key())
            } else {
                keys.bound(&name_record::node_word(seed.minter()))
            }
        };
        // A minter whose key is unbound here leaves its seed held: nothing shows it stale.
        if held.is_some_and(|held| key_of(&held).is_some_and(|key| !held.verifies(&key, mark))) {
            held = None;
        }
        let (mut adopted, mut refused) = (None, None);
        for seed in offered {
            let mine = Some(seed.minter()) == own;
            if !key_of(&seed).is_some_and(|key| seed.verifies(&key, mark)) {
                continue;
            }
            let judged = if root && !mine {
                Judged::Conflict
            } else {
                fleet_seed::judge(held.as_ref(), &seed)
            };
            match judged {
                Judged::Keep => {}
                Judged::Conflict => refused = Some(seed),
                Judged::Take | Judged::Earlier | Judged::Resigned => {
                    held = Some(seed);
                    adopted = Some(seed);
                }
            }
        }
        if let Some(seed) = &adopted {
            self.keep_seed(Some(seed))?;
        }
        if let Some(seed) = &refused
            && !fleet_seed::conflict_recorded(self.dir, seed)
        {
            write_private(
                self.dir,
                fleet_seed::CONFLICT_FILE,
                fleet_seed::conflict_line(seed, now_ms).as_bytes(),
            )?;
        }
        Ok(adopted)
    }

    /// On a root holding no fleet seed (`docs/DESIGN-machine-names-2026-11-04.md` §3.3: a fleet
    /// founded before seeds, or a root whose file was lost), mint one for the fleet whose
    /// secret's mark is `mark` once [`fleet_seed::UNHELD_WAIT_MS`] has passed since it first
    /// found it held none, as [`fleet_seed::UNHELD_FILE`] records at `now_ms`. The wait runs only
    /// in rounds that `read` a member's book, each of which relays the seed it minted before if
    /// that member holds it ([`HeldBook::adopt_relayed_seed`]), so a root off its network, whose
    /// dials all miss, neither starts the wait nor ends it; a second mint settles on the first by
    /// [`fleet_seed::judge`]'s earlier rule. A held seed that no longer verifies under its own key
    /// and `mark` is none. A member mints nothing, and a held seed drops the record. A seed file
    /// it cannot read stands. Returns the seed it minted.
    /// # Errors
    /// The record or the seed cannot be written, or the CSPRNG cannot be read.
    pub fn mint_unheld_seed(
        &self,
        me: &NodeIdentity,
        mark: FleetMark,
        (now_ms, read): (u64, bool),
    ) -> io::Result<Option<fleet_seed::FleetSeed>> {
        if !self.is_root(me) {
            return Ok(None);
        }
        match fleet_seed::read(self.dir) {
            Ok(Some(held)) if held.verifies(&me.key(), mark) => {
                let _ = self.remove(fleet_seed::UNHELD_FILE);
                return Ok(None);
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {}
            Err(_) => return Ok(None),
        }
        if !read {
            return Ok(None);
        }
        // A record from a later clock than this one's restarts the wait, so a clock set back
        // never holds the mint off past one wait.
        match fleet_seed::read_unheld(self.dir).filter(|since| *since <= now_ms) {
            Some(since) if now_ms - since >= fleet_seed::UNHELD_WAIT_MS => {}
            Some(_) => return Ok(None),
            None => {
                write_private(
                    self.dir,
                    fleet_seed::UNHELD_FILE,
                    format!("{now_ms}\n").as_bytes(),
                )?;
                return Ok(None);
            }
        }
        let seed = fleet_seed::FleetSeed::mint(me, mark, now_ms)?;
        self.keep_seed(Some(&seed))?;
        let _ = self.remove(fleet_seed::UNHELD_FILE);
        Ok(Some(seed))
    }

    /// Whether `me` is the fleet's root here: it holds the join code, which only `init` and its
    /// own rekey write, and its book names no issuer (§3.3).
    fn is_root(&self, me: &NodeIdentity) -> bool {
        read_join_code(self.dir).is_ok() && !names_an_issuer(self.dir, Some(me.node()))
    }

    /// Remove the state directory's file `name`: whether it was there.
    /// # Errors
    /// `name` is no one file name, or the file is there and cannot be removed.
    pub fn remove(&self, name: &str) -> io::Result<bool> {
        remove_private(self.dir, name)
    }
}

/// Whether `e`, why the state directory was not made or the book's lock not taken, stops every edit of the book too: the directory is one no edit narrows (a symlink, a file, another user's, one an access list opens) or makes (beneath a file or a link to nothing, in a directory or on a volume this user cannot write), or the lock is no file this user can lock — never another edit holding it, nor a directory gone (an init or a join makes one, and takes the lock there), nor a failure of the moment (no descriptor or space free, an interrupted call).
fn stops_every_edit(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied
            | io::ErrorKind::AlreadyExists
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::ReadOnlyFilesystem
            | io::ErrorKind::InvalidData
            | io::ErrorKind::Unsupported
    ) || matches!(e.raw_os_error(), Some(libc::ELOOP | libc::EISDIR))
}

/// What [`record`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// A peer the book did not hold.
    New,
    /// A peer it held under that node id, brought up to date.
    Updated,
    /// A person's peer the book already holds at that place, or a self-signed entry no later
    /// than the one it holds of that node.
    Already,
}

fn unix_ms_now() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
}

/// Drop peers from `book` whose node is on `denied_nodes` or (when `now_ms` is given) whose
/// `joined` registration has expired (`>= 90` days silent). Returns `true` when `book` shrank.
pub fn prune_book_at(book: &mut Vec<Peer>, now_ms: Option<u64>, denied_nodes: &[String]) -> bool {
    let before = book.len();
    book.retain(|known| {
        let denied = known
            .node
            .as_deref()
            .is_some_and(|node| crate::fleet::is_denied_node(node, denied_nodes))
            || known
                .key
                .as_ref()
                .is_some_and(|key| crate::fleet::is_denied_key(key, denied_nodes));
        let expired = now_ms.is_some_and(|now| known.is_expired_at(now));
        !denied && !expired
    });
    book.len() != before
}

/// Put `peer` in `book`: one with a node id replaces the entry with that id, and a person's
/// entry for the place it stands for (for a member's registration the address and port it
/// registered from, never the host name it gives itself; for an issuer its place); one a
/// person named is added unless the book holds that place already ([`Peer::same_place`]).
/// A valid stored self-signed descriptor keeps its signed fields when an unsigned contact
/// under the same key refreshes its observations or role. Two valid signed descriptors keep the
/// higher self-sequence; an equal sequence may add a valid signed name and refresh newer
/// contact observations.
/// It refuses a copy of the issuer's book ([`Via::IssuerBook`]), a registration whose key
/// does not fit the book's entry for its node or whose address already has
/// [`JOINED_PER_ADDRESS`] other live members ([`joined_per_address`]), and a new entry once the
/// book is full.
pub fn record(book: &mut Vec<Peer>, peer: Peer) -> Result<Recorded, String> {
    record_with_denied(book, peer, &[])
}

/// [`record`], additionally pruning `denied_nodes` and 90-day-expired entries and refusing a
/// peer whose node id is on `denied_nodes`.
pub fn record_with_denied(
    book: &mut Vec<Peer>,
    peer: Peer,
    denied_nodes: &[String],
) -> Result<Recorded, String> {
    let full = || {
        format!("the book holds {PEERS_MAX} peers already: remove one with `aterm fleet peers rm`")
    };
    if peer.via == Via::IssuerBook {
        return Err(
            "a copy of the issuer's book is written by a read of that book, never recorded"
                .to_owned(),
        );
    }
    if peer
        .node
        .as_deref()
        .is_some_and(|node| crate::fleet::is_denied_node(node, denied_nodes))
        || peer
            .key
            .as_ref()
            .is_some_and(|key| crate::fleet::is_denied_key(key, denied_nodes))
    {
        return Err("that node is on the swarm roster's deny-list".to_owned());
    }
    prune_book_at(book, peer.seen_ms, denied_nodes);
    if peer.via == Via::Manual {
        if book.iter().any(|known| known.same_place(&peer)) {
            return Ok(Recorded::Already);
        }
        if book.len() >= PEERS_MAX {
            return Err(full());
        }
        book.push(peer);
        return Ok(Recorded::New);
    }
    let joined = peer.via == Via::Joined;
    if joined
        && let Some(known) = book
            .iter()
            .find(|known| known.node.is_some() && known.node == peer.node)
    {
        bound_as(known, &peer)?;
    }
    let at_addr = |known: &Peer| known.addr.is_some() && known.addr == peer.addr;
    let allowance = joined_per_address();
    if joined
        && book
            .iter()
            .filter(|known| known.via == Via::Joined && at_addr(known) && known.node != peer.node)
            .count()
            >= allowance
    {
        let now = peer.seen_ms.or_else(unix_ms_now).unwrap_or(0);
        let oldest_silent = book
            .iter()
            .enumerate()
            .filter(|(_, known)| {
                known.via == Via::Joined
                    && at_addr(known)
                    && known.node != peer.node
                    && !known.is_live_at(now)
            })
            .min_by_key(|(_, known)| known.seen_ms.unwrap_or(0))
            .map(|(idx, _)| idx);
        match oldest_silent {
            Some(evict_idx) => {
                book.remove(evict_idx);
            }
            None => {
                return Err(format!(
                    "{} registered {allowance} members already: remove one with `aterm fleet peers \
                     rm`",
                    peer.addr
                        .map_or_else(|| "its address".to_owned(), |a| a.to_string())
                ));
            }
        }
    }
    book.retain(|known| {
        let stands_for = if joined {
            known.port == peer.port && at_addr(known)
        } else {
            known.same_place(&peer)
        };
        !(known.via == Via::Manual && stands_for)
    });
    if let Some(known) = book
        .iter_mut()
        .find(|known| known.node.is_some() && known.node == peer.node)
    {
        // Of two self-signed entries of one node under one key, the higher self-seq stands: a
        // lower one changes nothing. An equal one can add a valid independently signed
        // name stripped by an older relay, but cannot remove or replace a known name, nor the
        // name record the held copy carries (which copy's record stands at one seq is
        // Currency's choice, `docs/DESIGN-machine-names-2026-11-04.md` §3.2).
        // Other signed fields stay fixed; only where it was last heard can advance.
        // A damaged held copy has no authenticated ordering and can be repaired by a valid one.
        if let (Some(held), Some(new)) = (known.signed, peer.signed)
            && known.key == peer.key
            && known.signature_holds()
            && new.seq <= held.seq
        {
            let enriched = new.seq == held.seq
                && held.name.is_none()
                && new.name.is_some()
                && new.sig == held.sig
                && peer.signature_holds();
            if enriched {
                known.signed = Some(new.with_record(held.record.or(new.record)));
            }
            if new.seq == held.seq && peer.seen_ms > known.seen_ms {
                known.addr = peer.addr.or(known.addr);
                known.seen_ms = peer.seen_ms;
                return Ok(Recorded::Updated);
            }
            return Ok(if enriched {
                Recorded::Updated
            } else {
                Recorded::Already
            });
        }
        // A proved contact can refresh observations and this book's role, but its unsigned
        // endpoint cannot replace the node's own authenticated descriptor.
        let peer = if peer.signed.is_none() && known.key == peer.key && known.signature_holds() {
            Peer {
                host: known.host.clone(),
                port: known.port,
                version: known.version.clone(),
                signed: known.signed,
                addr: if peer.seen_ms >= known.seen_ms {
                    peer.addr
                } else {
                    known.addr.or(peer.addr)
                },
                seen_ms: peer.seen_ms.max(known.seen_ms),
                ..peer
            }
        } else {
            peer
        };
        let key_from = (joined && known.key.is_none() && peer.key.is_none())
            .then(|| known.key_from.or(known.addr))
            .flatten()
            .filter(|from| Some(*from) != peer.addr);
        // The member this Mac joined through stays that, under the fleet it joined.
        let (via, fleet) = if known.via == Via::Issuer {
            (Via::Issuer, known.fleet)
        } else {
            (peer.via, peer.fleet)
        };
        let addr = peer.addr.or(known.addr);
        *known = Peer {
            addr,
            key_from,
            via,
            fleet,
            ..peer
        };
        return Ok(Recorded::Updated);
    }
    if book.len() >= PEERS_MAX {
        return Err(full());
    }
    book.push(peer);
    Ok(Recorded::New)
}

fn bound_as(known: &Peer, peer: &Peer) -> Result<(), String> {
    let whole = peer.node.as_deref().unwrap_or("-");
    let node = whole.get(..8).unwrap_or(whole);
    let rm = format!("`aterm fleet peers rm {whole}` {REBIND}");
    match (known.key, peer.key) {
        (Some(bound), Some(proved)) if bound == proved => Ok(()),
        (Some(_), Some(_)) => Err(format!(
            "node {node} is bound to another key in the issuer's book: not that node, or one \
             that lost its node key — then {rm}"
        )),
        (Some(_), None) => Err(format!(
            "node {node} is bound to a key in the issuer's book, and this registration proves \
             none (an older aterm?): update aterm there, or {rm}"
        )),
        (None, Some(_)) => match known.key_from.or(known.addr) {
            Some(from) if Some(from) != peer.addr => Err(format!(
                "node {node} was recorded from {from} {BEFORE_KEYS}, and its key is bound only \
                 from there: {rm}"
            )),
            _ => Ok(()),
        },
        (None, None) => Ok(()),
    }
}

/// How each refusal of a registration by its node's key ends ([`record`]), after the
/// `aterm fleet peers rm <node>` that lets it register afresh; a member's `join` matches it.
pub const REBIND: &str = "on the issuer, and join again";

/// Words only one refusal says ([`bound_as`]): a key proved from another address than the one
/// a keyless entry was recorded from. A member's `join` reads it as no key bound yet, not as a
/// node bound to another key.
pub const BEFORE_KEYS: &str = "before keys came";

/// Where in `peers` the one peer `what` names is ([`Peer::answers_to`]): `None` when no peer
/// answers to it, an error when several do.
pub fn find(peers: &[Peer], what: &str) -> Result<Option<usize>, String> {
    find_with(peers, &[], what)
}

/// Ranked [`find`] consulting optional swarm-roster and vault `identities`: the highest matching
/// [`HostRank`] wins, and ties within that rank return [`several`].
/// # Errors
/// `what` matches more than one peer at the winning [`HostRank`].
pub fn find_with(
    peers: &[Peer],
    identities: &[PeerIdentity],
    what: &str,
) -> Result<Option<usize>, String> {
    let ranked: Vec<(usize, HostRank)> = peers
        .iter()
        .enumerate()
        .filter_map(|(at, peer)| {
            peer.host_rank_with(what, identity_for(peer, identities))
                .map(|rank| (at, rank))
        })
        .collect();
    let Some(best) = ranked.iter().map(|(_, rank)| *rank).min() else {
        return Ok(None);
    };
    let matches: Vec<usize> = ranked
        .into_iter()
        .filter(|(_, rank)| *rank == best)
        .map(|(at, _)| at)
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [at] => Ok(Some(*at)),
        many => Err(several(
            what,
            None,
            &many
                .iter()
                .filter_map(|at| peers.get(*at))
                .collect::<Vec<_>>(),
        )),
    }
}

/// The one peer `what` names in `book` and `others` (its copy of the issuer's book, and the
/// `[[fleet.peers]]` entries) read together: each node once (the book's entry first), then each
/// place an entry with no node holds once, where no node the word names stands. Every verb that
/// takes a peer's word reads it here.
/// # Errors
/// `what` names more than one peer.
pub fn named<'a>(
    book: &'a [Peer],
    others: &'a [Peer],
    what: &str,
) -> Result<Option<&'a Peer>, String> {
    named_with(book, others, &[], what)
}

/// Ranked [`named`] consulting optional swarm-roster and vault `identities`.
/// # Errors
/// `what` names more than one peer at the winning [`HostRank`].
pub fn named_with<'a>(
    book: &'a [Peer],
    others: &'a [Peer],
    identities: &[PeerIdentity],
    what: &str,
) -> Result<Option<&'a Peer>, String> {
    match all_named_with(book, others, identities, what).as_slice() {
        [] => Ok(None),
        [peer] => Ok(Some(peer)),
        many => Err(several(what, None, many)),
    }
}

/// Every peer `what` names in `book` and `others` read together, as [`named`] reads them.
#[must_use]
pub fn all_named<'a>(book: &'a [Peer], others: &'a [Peer], what: &str) -> Vec<&'a Peer> {
    all_named_with(book, others, &[], what)
}

/// Every peer `what` names at the winning [`HostRank`] across `book` and `others`, consulting
/// `identities` and deduplicating as [`all_named`] does.
#[must_use]
pub fn all_named_with<'a>(
    book: &'a [Peer],
    others: &'a [Peer],
    identities: &[PeerIdentity],
    what: &str,
) -> Vec<&'a Peer> {
    let ranked: Vec<(&Peer, HostRank)> = book
        .iter()
        .chain(others)
        .filter_map(|peer| {
            peer.host_rank_with(what, identity_for(peer, identities))
                .map(|rank| (peer, rank))
        })
        .collect();
    let Some(best) = ranked.iter().map(|(_, rank)| *rank).min() else {
        return Vec::new();
    };
    distinct(
        ranked
            .into_iter()
            .filter_map(|(peer, rank)| (rank == best).then_some(peer)),
    )
}

/// Every peer whose host `what` is a short name of ([`short_names`]) in `book` and `others`,
/// read together as [`all_named`] reads them: what `aterm ctl --host` reads a word as where
/// [`all_named`] finds no peer. The lead words are read first ([`ShortName::Lead`]), and the
/// machine numbers only where no lead word is `what`, so `m27` names `m27-mini` before
/// `MacbookAir-m27-…`. More than one peer of one tier is refused with [`several_short`], never
/// picked.
#[must_use]
pub fn all_short_named<'a>(book: &'a [Peer], others: &'a [Peer], what: &str) -> Vec<&'a Peer> {
    let lead = all_answering(book, others, |peer| {
        peer.answers_to_short(what, ShortName::Lead)
    });
    if !lead.is_empty() {
        return lead;
    }
    all_answering(book, others, |peer| {
        peer.answers_to_short(what, ShortName::Number)
    })
}

/// The refusal of a short name ([`all_short_named`]) `many` peers answer to: each by its host
/// and the address it was last reached at, and how to name one.
#[must_use]
pub fn several_short(what: &str, many: &[&Peer]) -> String {
    format!(
        "`{}` is a short name of {} peers ({}): name one by its host, node id or address \
         (`aterm fleet peers`)",
        aterm_codec::tty::one_line(what, 80),
        many.len(),
        many.iter()
            .map(|peer| format!(
                "{} at {}",
                aterm_codec::tty::one_line(&peer.host, 80),
                peer.target()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Every peer of `book` and `others` read together that `answers` ([`distinct`]).
fn all_answering<'a>(
    book: &'a [Peer],
    others: &'a [Peer],
    answers: impl Fn(&Peer) -> bool,
) -> Vec<&'a Peer> {
    distinct(book.iter().chain(others).filter(|peer| answers(peer)))
}

/// The peers `matched`, each node once (its first entry, the book's before a copy's), then each
/// place an entry with no node holds once, where no node answered.
fn distinct<'a>(matched: impl Iterator<Item = &'a Peer>) -> Vec<&'a Peer> {
    let (nodes, hands): (Vec<&Peer>, Vec<&Peer>) = matched.partition(|peer| peer.node.is_some());
    let mut peers: Vec<&Peer> = Vec::new();
    for node in &nodes {
        if !peers.iter().any(|seen| seen.node == node.node) {
            peers.push(node);
        }
    }
    for hand in hands {
        if !nodes.iter().chain(&peers).any(|seen| seen.same_place(hand)) {
            peers.push(hand);
        }
    }
    peers
}

/// The refusal of a word `many` peers answer to, and this machine's own node `own` where it names
/// that too, naming each by its whole node id, which tells them apart where a prefix may not, and
/// how to name one.
#[must_use]
pub fn several(what: &str, own: Option<&str>, many: &[&Peer]) -> String {
    let named = own
        .map(|own| format!("{own}, this machine itself"))
        .into_iter()
        .chain(many.iter().map(|peer| {
            format!(
                "{} at {}",
                peer.node.as_deref().unwrap_or("-"),
                peer.target()
            )
        }));
    format!(
        "`{}` names {} {} ({}): name one by its node id{}",
        aterm_codec::tty::one_line(what, 80),
        many.len() + usize::from(own.is_some()),
        if own.is_some() { "nodes" } else { "peers" },
        named.collect::<Vec<_>>().join(", "),
        // A place two of them share names neither.
        if many
            .iter()
            .enumerate()
            .all(|(i, a)| { many[i + 1..].iter().all(|b| a.target() != b.target()) })
        {
            " or its address and port"
        } else {
            ""
        }
    )
}

/// Take the entry of `book` of the one peer `what` names ([`named`] with `others`) out of it:
/// its node's, or the entry with no node at its place.
/// # Errors
/// No peer or several are named, two entries of `book` answer to it, or the peer is only in
/// `others`.
pub fn remove(book: &mut Vec<Peer>, others: &[Peer], what: &str) -> Result<Peer, String> {
    remove_with(book, others, &[], what)
}

/// Ranked [`remove`] consulting optional swarm-roster and vault `identities`.
/// # Errors
/// No peer or several are named at the winning [`HostRank`], two entries of `book` answer to it
/// for that peer, or the named peer is only in `others`.
pub fn remove_with(
    book: &mut Vec<Peer>,
    others: &[Peer],
    identities: &[PeerIdentity],
    what: &str,
) -> Result<Peer, String> {
    let shown = aterm_codec::tty::one_line(what, 80);
    let Some(peer) = named_with(book, others, identities, what)?.cloned() else {
        return Err(format!("no peer is `{shown}`"));
    };
    let matches: Vec<usize> = book
        .iter()
        .enumerate()
        .filter_map(|(at, entry)| {
            let same_peer = match (&peer.node, &entry.node) {
                (Some(want), Some(have)) => want == have,
                (Some(_), None) | (None, None) => entry.same_place(&peer),
                (None, Some(_)) => false,
            };
            (same_peer
                && entry
                    .host_rank_with(what, identity_for(entry, identities))
                    .is_some())
            .then_some(at)
        })
        .collect();
    match matches.as_slice() {
        [at] => Ok(book.remove(*at)),
        [] if peer.via == Via::IssuerBook => Err(format!(
            "`{shown}` is in the fleet issuer's book, which this machine copies at each read: \
             `aterm fleet peers rm` on the issuer takes it out, and this copy follows"
        )),
        [] => Err(format!(
            "`{shown}` is no entry of this machine's book, which `peers rm` edits"
        )),
        many => Err(several(
            what,
            None,
            &many
                .iter()
                .filter_map(|at| book.get(*at))
                .collect::<Vec<_>>(),
        )),
    }
}

/// The node keys this machine binds, node id to key: its book's (on the issuer, every member
/// that registered with a key; on a member, its issuer), then its copy of the issuer's book's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keyring {
    own: Vec<(String, NodeKey)>,
    copied: Vec<(String, NodeKey)>,
    issuer: Option<String>,
}

impl Keyring {
    /// The keys `peers` bind, a copy's ([`Via::IssuerBook`]) ranked under every other entry's
    /// wherever it stands, and the issuer they name.
    #[must_use]
    pub fn of<'a>(peers: impl IntoIterator<Item = &'a Peer>) -> Self {
        let mut ring = Self::default();
        for peer in peers {
            if peer.via == Via::Issuer && ring.issuer.is_none() {
                ring.issuer.clone_from(&peer.node);
            }
            if let (Some(node), Some(key)) = (&peer.node, peer.key) {
                let tier = if peer.via == Via::IssuerBook {
                    &mut ring.copied
                } else {
                    &mut ring.own
                };
                if !tier.iter().any(|(bound, _)| bound == node) {
                    tier.push((node.clone(), key));
                }
            }
        }
        ring
    }

    /// The keys this machine binds, as the state directory `dir` holds them: its book, then
    /// its copy of its issuer's book ([`read_issuer_book`]).
    #[must_use]
    pub fn read(dir: &Path) -> Self {
        let (peers, copies) = book_and_copies(dir);
        Self::of(peers.iter().chain(&copies))
    }

    /// Whether the copy of the issuer's book binds `node`, which is not the issuer: a `peers rm`
    /// of it here leaves it bound, as only the issuer's own `peers rm` takes it out of that book.
    #[must_use]
    pub fn copy_binds(&self, node: &str) -> bool {
        !self.is_issuer(node) && self.copied.iter().any(|(bound, _)| bound == node)
    }

    /// These keys once `book`, a fresh read of the issuer's book, is the copy: the copy's keys
    /// are replaced whole by the book's ([`copies_of`]; none from a book whose issuer proved
    /// no key), so a member the issuer took out and bound afresh is held to its new key at once.
    #[must_use]
    pub fn with_book(&self, book: &Book, own: Option<&str>) -> Self {
        Self {
            copied: Self::of(&copies_of(book, own)).copied,
            ..self.clone()
        }
    }

    /// The key `node` is bound to, if any.
    #[must_use]
    pub fn bound(&self, node: &str) -> Option<NodeKey> {
        self.own
            .iter()
            .chain(&self.copied)
            .find(|(bound, _)| bound == node)
            .map(|(_, key)| *key)
    }

    fn is_issuer(&self, node: &str) -> bool {
        self.issuer.as_deref() == Some(node)
    }

    /// The proven node behind `proof`, what a connection's peer proved
    /// ([`Served::peer_node`], [`Reached::peer`]): the node id it named, when this keyring
    /// binds that id to the key it proved.
    pub fn proven(&self, proof: Option<&NodeProof>) -> Result<String, Unproven> {
        let proof = proof.ok_or(Unproven::NoProof)?;
        match self.bound(&proof.node) {
            Some(key) if key == proof.key => Ok(proof.node.clone()),
            Some(_) => Err(Unproven::OtherKey(proof.node.clone())),
            None => Err(Unproven::Unbound(proof.node.clone())),
        }
    }
}

/// Why a peer is no node this machine can name ([`Keyring::proven`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unproven {
    /// It proved no node: an older aterm, or one that holds no node key.
    NoProof,
    /// It proved a key for this node id, which this machine binds no key to: an entry an
    /// older aterm recorded, a node this machine never heard of, or a peer named by hand.
    Unbound(String),
    /// It claimed this node id and proved another key than the one this machine binds it
    /// to: not that node.
    OtherKey(String),
}

impl Unproven {
    /// Its words, the node named by a head no other node of `knows` shares ([`node_head`]).
    #[must_use]
    pub fn said<S: AsRef<str>>(&self, knows: &[S]) -> String {
        match self {
            Self::NoProof => {
                "the peer proved no node (an older aterm, or one with no node key)".to_owned()
            }
            Self::Unbound(node) => format!(
                "node {} is bound to no key on the dialled Mac, nor named by its membership log: \
                 it is bound once it joins the fleet again",
                node_head(node, knows)
            ),
            Self::OtherKey(node) => format!(
                "the peer claims node {} and proved another key than the dialled Mac binds to it",
                node_head(node, knows)
            ),
        }
    }
}

impl fmt::Display for Unproven {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.said::<&str>(&[]))
    }
}

impl std::error::Error for Unproven {}

/// The proven node of the peer of a fleet connection this listener serves, for an
/// authorization that names nodes (a remote step-in): [`Keyring::proven`] over the keys in
/// `state_dir`.
pub fn proven_peer(served: &Served, state_dir: &Path) -> Result<String, Unproven> {
    Keyring::read(state_dir).proven(served.peer_node())
}

/// Where a node key stands in this Mac's membership log (`docs/DESIGN-fluid-swarm-2026-10-28.md`
/// §3.3, F17): a member's, or one the log took out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The log admits a member under it.
    Member,
    /// The log took it out, with why in a few words (removed, retired, contested).
    Out(String),
}

/// The node keys this Mac's membership log names, each with its [`Standing`]: what admits a
/// drive, a join and a member's own book entry once the Mac holds a log, with no other Mac read.
/// A key it names neither way is unknown to it, and is judged as with no log (the fleet secret
/// admits its join, the book's keys its drive, [`Keyring::driver`]): no record binds a founder's
/// node key yet (Genesis v2 names none), so only a key the log took out is refused for the log's
/// sake.
///
/// A Mac whose log exists but cannot be read now, with no verdict read from it before, has the
/// [`MemberKeys::unreadable`] verdict: it names no key a member's, and every key stands out
/// under it, so nothing the book alone would admit gets in until the log reads again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemberKeys {
    keys: std::collections::BTreeMap<NodeKey, Standing>,
    /// Where every key stands while the log cannot be read: out, with why.
    unread: Option<Standing>,
}

impl MemberKeys {
    /// The verdict of a membership log that exists but cannot be read now (`why`), with none
    /// read from it before to keep: every key stands out, so every admission refuses, a join, a
    /// drive, a merged book entry and a heartbeat target alike. It takes nothing away: no stored
    /// facts, book entry or grant goes for its sake ([`MemberKeys::unread`]).
    #[must_use]
    pub fn unreadable(why: impl Into<String>) -> Self {
        Self {
            keys: std::collections::BTreeMap::new(),
            unread: Some(Standing::Out(why.into())),
        }
    }

    /// Why the log cannot be read now, for the verdict [`MemberKeys::unreadable`] made; `None`
    /// for a verdict read from the log.
    #[must_use]
    pub fn unread(&self) -> Option<&str> {
        match &self.unread {
            Some(Standing::Out(why)) => Some(why),
            _ => None,
        }
    }

    /// Where `key` stands, if the log names it; out for every key while the log cannot be read.
    #[must_use]
    pub fn standing(&self, key: &NodeKey) -> Option<&Standing> {
        self.unread.as_ref().or_else(|| self.keys.get(key))
    }

    /// Whether the log admits a member under `key`.
    #[must_use]
    pub fn admits(&self, key: &NodeKey) -> bool {
        self.standing(key) == Some(&Standing::Member)
    }

    /// Whether the log took `key` out (its Mac removed, its member bound another since, or two
    /// members claim it).
    #[must_use]
    pub fn holds_out(&self, key: &NodeKey) -> bool {
        matches!(self.standing(key), Some(Standing::Out(_)))
    }

    /// The keys the log took out, which every listener refuses before any op
    /// ([`crate::fleet::Denied`]). An unread log names none: the listener refuses every proved
    /// dialer under it instead ([`crate::fleet::Denied::under`]).
    #[must_use]
    pub fn out(&self) -> Vec<NodeKey> {
        self.keys
            .iter()
            .filter(|(_, standing)| matches!(standing, Standing::Out(_)))
            .map(|(key, _)| *key)
            .collect()
    }

    /// The keys the log seats a current member under: what [`crate::fleet::ALPN_MESH`] admits
    /// ([`crate::fleet::Denied::under`]). An unread log names none.
    #[must_use]
    pub fn members(&self) -> Vec<NodeKey> {
        if self.unread.is_some() {
            return Vec::new();
        }
        self.keys
            .iter()
            .filter(|(_, standing)| **standing == Standing::Member)
            .map(|(key, _)| *key)
            .collect()
    }

    /// Whether the log names no key and was read.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.unread.is_none()
    }
}

impl FromIterator<(NodeKey, Standing)> for MemberKeys {
    /// A key named twice stands out if either names it so.
    fn from_iter<I: IntoIterator<Item = (NodeKey, Standing)>>(iter: I) -> Self {
        let mut keys = std::collections::BTreeMap::new();
        for (key, standing) in iter {
            match keys.get(&key) {
                Some(Standing::Out(_)) => {}
                _ => {
                    keys.insert(key, standing);
                }
            }
        }
        Self { keys, unread: None }
    }
}

/// Why a peer may not drive this machine ([`proven_driver`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Undriven {
    /// It is no node this machine can name ([`Keyring::proven`]).
    Unproven(Unproven),
    /// No grant names its node.
    Unlisted(String),
    /// A grant names its node under another key than the one it proved: the node id was
    /// bound afresh since the person here let it drive, or another machine claims it. No read
    /// of a book makes it the node the grant was made for.
    Rekeyed(String),
    /// This machine's membership log took the key it proved out.
    Removed {
        /// The node it claimed.
        node: String,
        /// Why, as the log says it.
        why: String,
    },
}

impl Undriven {
    /// Its words, the node named by a head no other node of `knows` shares ([`node_head`]): the
    /// dialled Mac's, so its refusal never names the dialer as another node it knows.
    #[must_use]
    pub fn said<S: AsRef<str>>(&self, knows: &[S]) -> String {
        match self {
            Self::Unproven(why) => why.said(knows),
            Self::Unlisted(node) => format!(
                "node {} is not one the dialled Mac lets drive it (aterm fleet allow drive --from \
                 <node> or --all, there)",
                node_head(node, knows)
            ),
            Self::Rekeyed(node) => format!(
                "node {} proved another key than the one it held when the dialled Mac's person \
                 let it drive: it drives again only once they run aterm fleet allow drive --from \
                 <node> (or --all), there",
                node_head(node, knows)
            ),
            Self::Removed { node, why } => format!(
                "node {} proved a key the dialled Mac's membership log took out ({}): it drives \
                 again only once the fleet admits it again",
                node_head(node, knows),
                aterm_codec::tty::one_line(why, 160)
            ),
        }
    }
}

impl fmt::Display for Undriven {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.said::<&str>(&[]))
    }
}

impl std::error::Error for Undriven {}

impl Undriven {
    /// No grant admits the node as this machine's grants, log and book stand now: none names
    /// it, its grant holds another key, its log took it out, or no book binds it.
    #[must_use]
    pub const fn ungranted(&self) -> bool {
        matches!(
            self,
            Self::Unlisted(_)
                | Self::Rekeyed(_)
                | Self::Removed { .. }
                | Self::Unproven(Unproven::Unbound(_) | Unproven::OtherKey(_))
        )
    }
}

impl Keyring {
    /// The driver behind `proof`: the node it proved under the key this keyring binds it to
    /// ([`Self::proven`]), when `grants` names that node with that same key or every member.
    /// A node no grant names is refused as that before its key is judged, and one that proved
    /// another key than its grant holds as [`Undriven::Rekeyed`] wherever this keyring binds it
    /// no other key. An all-member grant uses the same issuer-book admission and freshness
    /// checks as a named grant; it never admits a peer merely for knowing the fleet secret.
    pub fn driver(
        &self,
        proof: Option<&NodeProof>,
        grants: &[DriveGrant],
    ) -> Result<String, Undriven> {
        let proof = proof.ok_or(Undriven::Unproven(Unproven::NoProof))?;
        if DriveGrant::admitting(grants, &proof.node, proof.key).is_some() {
            return self.proven(Some(proof)).map_err(Undriven::Unproven);
        }
        if !grants.iter().any(|grant| grant.node == proof.node) {
            return Err(Undriven::Unlisted(proof.node.clone()));
        }
        match self.proven(Some(proof)) {
            Ok(node) | Err(Unproven::Unbound(node)) => Err(Undriven::Rekeyed(node)),
            proven => proven.map_err(Undriven::Unproven),
        }
    }
}

/// The driver of a fleet connection this machine's listener serves, judged on this Mac alone
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17): by `log`, its membership log's keys, when
/// it holds one that names the key the peer proved, else by [`Keyring::driver`] over the book in
/// `state_dir`, read fresh on each call. No other Mac is read, so with every other member dark a
/// member the log admits drives, and one it took out drives nowhere at once.
pub fn proven_driver(
    served: &Served,
    state_dir: &Path,
    grants: &[DriveGrant],
    log: Option<&MemberKeys>,
) -> Result<String, Undriven> {
    driver_as_of(served.peer_node(), (state_dir, grants), log)
}

/// Authenticate a signed administrative record, then apply the same current grants,
/// node bindings and membership revocations as a live fleet drive connection.
/// # Errors
/// The signature is invalid or the current fleet policy refuses this node.
pub fn signed_record_driver(
    proof: &NodeProof,
    domain: &str,
    body: &[u8],
    signature: &[u8; 64],
    state_dir: &Path,
    grants: &[DriveGrant],
    log: Option<&MemberKeys>,
) -> Result<String, String> {
    if !crate::fleet::verifies_record(&proof.key, domain, body, signature) {
        return Err("fleet administrative record signature does not verify".into());
    }
    driver_as_of(Some(proof), (state_dir, grants), log).map_err(|e| e.to_string())
}

/// Recheck a transport-authenticated node against current Drive grants and revocations.
/// The caller retains the exact node key proved by the connection handshake.
/// # Errors
/// The current node binding, membership or grant no longer admits this driver.
pub fn recheck_driver(
    proof: &NodeProof,
    state_dir: &Path,
    grants: &[DriveGrant],
    log: Option<&MemberKeys>,
) -> Result<String, Undriven> {
    driver_as_of(Some(proof), (state_dir, grants), log)
}

fn driver_as_of(
    proof: Option<&NodeProof>,
    (state_dir, grants): (&Path, &[DriveGrant]),
    log: Option<&MemberKeys>,
) -> Result<String, Undriven> {
    let proved = proof.ok_or(Undriven::Unproven(Unproven::NoProof))?;
    let own_key_mismatch = match read_node_identity(state_dir).ok().flatten() {
        Some(own) => own.node() == proved.node && own.key() != proved.key,
        None => read_node_id(state_dir)
            .ok()
            .flatten()
            .is_some_and(|own_node| own_node == proved.node),
    };
    if own_key_mismatch {
        return Err(Undriven::Unproven(Unproven::OtherKey(proved.node.clone())));
    }
    match log.and_then(|log| log.standing(&proved.key)) {
        Some(Standing::Out(why)) => Err(Undriven::Removed {
            node: proved.node.clone(),
            why: why.clone(),
        }),
        Some(Standing::Member) => {
            let keys = Keyring::read(state_dir);
            // A node id the book binds to another key the log did not take out is that member's.
            if let Some(bound) = keys.bound(&proved.node)
                && bound != proved.key
                && !log.is_some_and(|log| log.holds_out(&bound))
            {
                return Err(Undriven::Unproven(Unproven::OtherKey(proved.node.clone())));
            }
            if DriveGrant::admitting(grants, &proved.node, proved.key).is_some() {
                Ok(proved.node.clone())
            } else if grants.iter().any(|grant| grant.node == proved.node) {
                Err(Undriven::Rekeyed(proved.node.clone()))
            } else {
                Err(Undriven::Unlisted(proved.node.clone()))
            }
        }
        None => Keyring::read(state_dir).driver(proof, grants),
    }
}

fn issuer_of(book: &[Peer]) -> Option<&Peer> {
    book.iter().find(|peer| peer.via == Via::Issuer)
}

fn book_and_copies(dir: &Path) -> (Vec<Peer>, Vec<Peer>) {
    let peers = read_peers(dir).unwrap_or_default();
    let copies = issuer_of(&peers)
        .and_then(|issuer| issuer.node.as_deref())
        .map(|issuer| read_issuer_book(dir, issuer))
        .unwrap_or_default();
    (peers, copies)
}

/// The shortest head of `node`, eight characters at least, that none of `others` but `node`
/// itself begins with, so two nodes in one message never print alike: the whole id where every
/// shorter head is shared.
#[must_use]
pub fn node_head<'a, S: AsRef<str>>(node: &'a str, others: &[S]) -> &'a str {
    let shared = |head: &str| {
        others.iter().map(AsRef::as_ref).any(|other| {
            !other.eq_ignore_ascii_case(node)
                && other
                    .get(..head.len())
                    .is_some_and(|start| start.eq_ignore_ascii_case(head))
        })
    };
    (8..node.len())
        .filter_map(|len| node.get(..len))
        .find(|head| !shared(head))
        .unwrap_or(node)
}

/// Every node this machine in `state_dir` knows: its own, its book's and its copy's of its
/// issuer's book, against which [`node_head`] names one.
#[must_use]
pub fn known_nodes(state_dir: &Path) -> Vec<String> {
    let (peers, copies) = book_and_copies(state_dir);
    read_node_id(state_dir)
        .ok()
        .flatten()
        .into_iter()
        .chain(peers.into_iter().chain(copies).filter_map(|peer| peer.node))
        .collect()
}

/// The entry this machine's book in `state_dir`, or its copy of its issuer's book, holds for
/// `node` ([`Keyring::read`]'s order): how a node a connection proved is named to a person.
#[must_use]
pub fn peer_of(state_dir: &Path, node: &str) -> Option<Peer> {
    let (mut peers, copies) = book_and_copies(state_dir);
    with_copies(&mut peers, copies);
    peers
        .into_iter()
        .find(|peer| peer.node.as_deref() == Some(node))
}

/// The peers a key binds, gathered as [`Keyring::read`] gathers keys: each entry of the book
/// in `state_dir`, then of the whole copy of the issuer's book, that names a node with the key
/// the keyring binds it to, each node once and the book's entry first.
#[must_use]
pub fn bound_peers(state_dir: &Path) -> Vec<Peer> {
    let (peers, copies) = book_and_copies(state_dir);
    let keys = Keyring::of(peers.iter().chain(&copies));
    let mut bound: Vec<Peer> = Vec::new();
    for peer in peers.into_iter().chain(copies) {
        let Some(node) = peer.node.as_deref() else {
            continue;
        };
        if peer.key.is_some()
            && keys.bound(node) == peer.key
            && !bound.iter().any(|held| held.node.as_deref() == Some(node))
        {
            bound.push(peer);
        }
    }
    bound
}

/// The LAN addresses of this machine's fleet members, which share toolset downloads with it
/// (`atpkg::peers`): while `state_dir` holds a fleet key, each member a key binds
/// ([`bound_peers`]) other than this node, at the address its entry last reached it at when
/// that is a private or link-local one ([`crate::fleet::is_fleet_source`] less loopback), the
/// most recently heard first, each address once.
#[must_use]
pub fn member_addrs(state_dir: &Path) -> Vec<IpAddr> {
    member_addrs_with_denied(state_dir, &[], None)
}

/// [`member_addrs`], omitting any peer whose `node` or `key` is in `denied_nodes` or whose key
/// `log` holds out.
#[must_use]
pub fn member_addrs_with_denied(
    state_dir: &Path,
    denied_nodes: &[String],
    log: Option<&MemberKeys>,
) -> Vec<IpAddr> {
    if crate::fleet::read_key_file(state_dir).is_err() {
        return Vec::new();
    }
    let own = read_node_id(state_dir).ok().flatten();
    let mut members: Vec<Peer> = bound_peers(state_dir)
        .into_iter()
        .filter(|peer| {
            peer.node.is_some()
                && peer.node != own
                && !peer
                    .node
                    .as_deref()
                    .is_some_and(|node| crate::fleet::is_denied_node(node, denied_nodes))
                && !peer.key.is_some_and(|key| {
                    crate::fleet::is_denied_key(&key, denied_nodes)
                        || log.is_some_and(|log| log.holds_out(&key))
                })
        })
        .collect();
    members.sort_by_key(|peer| std::cmp::Reverse(peer.seen_ms));
    let mut addrs: Vec<IpAddr> = Vec::new();
    for addr in members.into_iter().filter_map(|peer| peer.addr) {
        let addr = addr.to_canonical();
        if crate::fleet::is_fleet_source(addr) && !addr.is_loopback() && !addrs.contains(&addr) {
            addrs.push(addr);
        }
    }
    addrs
}

/// Why the book's lock in `dir` was not taken, naming `dir` once: [`BookLock::take`]'s own refusals name it, a bare OS error does not.
fn lock_refused(dir: &Path, e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(_) => format!("{PEERS_FILE}'s lock in {}: {e}", dir.display()),
        None => format!("{PEERS_FILE}'s lock: {e}"),
    }
}

/// The membership write lock, `.fleet-peers.lock` beside the book
/// ([`aterm_uds::private_file::PrivateLock`]): its refusals name `dir`, each time it is found
/// held the test seam is told ([`found_held`]), and on macOS its taker runs at the default class
/// at least from before it takes it until it lets it go ([`ClassLift`]).
struct BookLock {
    /// Held for the guard's life: `PrivateLock`'s own `Drop` releases it.
    _held: aterm_uds::private_file::PrivateLock,
    /// The taker's thread lifted before it took the lock; dropped after `_held`, so its class
    /// is put back once the lock is let go.
    #[cfg(target_os = "macos")]
    _lift: ClassLift,
}

/// The thread that takes the book's lock, lifted to the default class where it had asked for less
/// (utility, background: [`aterm_uds::qos::lift`]) from before its take until it lets the lock
/// go, and put back as the lock is let go on that thread (one let go on another thread leaves the
/// class as it is) or as its take is refused. Each holder's read-modify-write is milliseconds of
/// work, and a person's verb waits [`LOCK_WAIT`] at most for it: a holder at a class a loaded Mac
/// starves of a core (an aterm window's member heartbeat, at utility) kept it for seconds and the
/// verb was refused, the inversion `aterm-gui`'s fork lift prevents for a fork. The lift comes
/// first, so no holder holds the lock at its own class for the moment between the take and the
/// lift, where a thread at utility may wait seconds for a core and one at background minutes; the
/// take's wait runs at the default class with it. The node job (`aterm node serve`) holds its own
/// threads at utility one by one rather than in a launchd band, where none is lifted (a
/// `Background` job runs every thread at priority 4, a `Standard` one clamps every thread at
/// utility), so its heartbeat holds the lock at the default class too; and the converge pass the
/// vault subscriber's `Background` job starts leaves that job's band as it starts
/// (`aterm_secrets::converge::runner`) and holds its threads at the background class one by
/// one, so its join does.
#[cfg(target_os = "macos")]
struct ClassLift(Option<(std::thread::ThreadId, aterm_uds::qos::Lifted)>);

#[cfg(target_os = "macos")]
impl ClassLift {
    fn up() -> Self {
        Self(aterm_uds::qos::lift().map(|lifted| (std::thread::current().id(), lifted)))
    }
}

#[cfg(target_os = "macos")]
impl Drop for ClassLift {
    fn drop(&mut self) {
        if let Some((thread, lifted)) = self.0.take()
            && thread == std::thread::current().id()
        {
            aterm_uds::qos::restore(lifted);
        }
    }
}

impl BookLock {
    fn take(dir: &Path, within: Duration) -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        let lift = ClassLift::up();
        let mut first = true;
        aterm_uds::private_file::PrivateLock::take_with(
            dir,
            PEERS_LOCK,
            within,
            &format!(
                "another aterm is writing the book in {}; run this again",
                dir.display()
            ),
            || found_held(std::mem::take(&mut first)),
        )
        .map(|held| Self {
            _held: held,
            #[cfg(target_os = "macos")]
            _lift: lift,
        })
    }
}

/// What a member tells the member it joins or heartbeats ([`Registration::line`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    /// The member's node id ([`node_id`]).
    pub node: String,
    /// The member's host name.
    pub host: String,
    /// The member's fleet port.
    pub port: u16,
    /// The member's aterm version.
    pub version: String,
    /// The member's node key ([`node_identity`]), which the connection it is sent on proves
    /// it holds; `None` from an older aterm.
    pub key: Option<NodeKey>,
    /// The member's own signature of the entry it registers ([`Signed`]); `None` from an older
    /// aterm, and with no key.
    pub signed: Option<Signed>,
}

impl Registration {
    /// Attach a primary name without making optional naming metadata a condition of joining.
    #[must_use]
    pub fn with_primary_name(mut self, me: &NodeIdentity, name: Option<&str>) -> Self {
        if let Some(signed) = self.signed
            && self.node == me.node()
        {
            self.signed = Some(
                Signed::named_by(me, (&self.host, self.port, &self.version), signed.seq, name)
                    .map_or(signed, |named| named.with_record(signed.record)),
            );
        }
        self
    }
    /// This registration signed by `me` at self-seq `seq` ([`Signed::by`]), naming `me`'s key.
    #[must_use]
    pub fn signed_by(self, me: &NodeIdentity, seq: u64) -> Self {
        let signed = Signed::by(me, (&self.host, self.port, &self.version), seq);
        Self {
            key: Some(me.key()),
            signed: Some(signed),
            ..self
        }
    }

    /// The registration as its line, newline included: `key=`, then the signature's `seq=` and
    /// `sig=` and optional signed name fields, which an older member passes over. It carries no
    /// name record: a member before 0.147 reads at most 512 bytes of it
    /// (`docs/DESIGN-machine-names-2026-11-04.md` §3.2 Registrations).
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "register node={} host={} port={} version={}{}{}\n",
            self.node,
            aterm_codec::pct::encode(&self.host),
            self.port,
            aterm_codec::pct::encode(&self.version),
            self.key
                .map_or_else(String::new, |key| format!(" key={}", key.hex())),
            self.signed
                .filter(|_| self.key.is_some())
                .map_or_else(String::new, |signed| signed.with_record(None).words()),
        )
    }

    /// A registration line, its newline dropped.
    pub fn parse(line: &str) -> Result<Self, &'static str> {
        let rest = line
            .strip_prefix("register ")
            .ok_or("a join sends `register node= host= port= version=`")?;
        let fields = fields(rest);
        let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let node = get("node")
            .filter(|node| is_node_id(node))
            .ok_or("node= is 32 lowercase hex digits")?;
        let host = aterm_codec::pct::decode(get("host").ok_or("no host=")?);
        if !is_host(&host) {
            return Err("host= is an address or a name of letters, digits, `.`, `-` and `_`");
        }
        let port = get("port")
            .filter(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|port| port.parse::<u16>().ok())
            .filter(|port| *port != 0)
            .ok_or("port= is 1 to 65535")?;
        let version = version_word(&aterm_codec::pct::decode(
            get("version").ok_or("no version=")?,
        ))
        .ok_or("version= is a version word")?;
        let key = get("key")
            .map(|key| NodeKey::from_hex(key).ok_or("key= is 64 lowercase hex digits"))
            .transpose()?;
        let signed = Signed::of_words(
            (get("seq"), get("sig")),
            (get("name"), get("name_sig")),
            (get("mname"), get("mname_sig")),
        )
        .ok_or("seq= is a number and sig= 128 lowercase hex digits, the two together")?
        .filter(|_| key.is_some());
        Ok(Self {
            node: node.to_owned(),
            host,
            port,
            version,
            key,
            signed,
        })
    }

    /// Whether its signature, if any, holds over what it names ([`Signed::holds`]).
    fn signature_fits(&self) -> bool {
        match (&self.signed, &self.key) {
            (None, _) => true,
            (Some(signed), Some(key)) => {
                signed.holds(((&self.node, key), (&self.host, self.port, &self.version)))
            }
            (Some(_), None) => false,
        }
    }
}

/// The member that recorded a join (once the fleet's issuer, now any member), as it names itself
/// in its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issuer {
    /// Its node id.
    pub node: String,
    /// Its host name.
    pub host: String,
    /// Its fleet port.
    pub port: u16,
    /// Its aterm version.
    pub version: String,
}

/// What a join's registration came to: the member that recorded it, and how many peers its book
/// holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Joined {
    /// The member that recorded it.
    pub issuer: Issuer,
    /// Its book's size, this member included.
    pub peers: usize,
    /// The fleet seed it holds, as its `seed=` words spell it (machine names §3.3), its
    /// signature not yet checked ([`fleet_seed::FleetSeed::verifies`]); `None` from a member
    /// holding none, and from an older one.
    pub seed: Option<fleet_seed::FleetSeed>,
}

fn read_record<S: Read>(stream: &mut S) -> io::Result<String> {
    let mut line = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the line ended before its newline",
                ));
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if line.len() >= RECORD_MAX {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "a line longer than a record",
                    ));
                }
                line.push(byte[0]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    String::from_utf8(line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a line that is not UTF-8"))
}

/// How the refusal an aterm before 0.147 that holds no join code answers a `join` with ends, in
/// the joining machine's words: a member's `aterm fleet join` reads it as no member that records
/// joins. Every member records joins now ([`serve_join`]).
pub const NOT_THE_ISSUER: &str = "it is no fleet's issuer: it holds no join code, so it records \
                                  no members (a join code comes from the fleet's issuer, in the \
                                  file its `aterm fleet init` names)";

/// How the refusal of a registration that names the answering member's own node starts
/// ([`serve_join`]): a member's `join` reads it as this machine carrying that member's node.
pub const OWN_NODE: &str = "the registration names the answering member's own node";

/// The refusal of a join proved under a fleet left or rotated before it was recorded, in the
/// joining machine's words.
pub const MOVED_ON: &str = "the answering member left the fleet, or rotated its secret, while \
                            the join was served, so it recorded nothing: join with the fleet's \
                            code as it is now";

/// How the refusal of a join whose node key the answering member's membership log does not admit
/// starts ([`serve_join`]).
pub const NOT_ADMITTED: &str = "the answering member's membership log holds this Mac's node key out \
                                of the fleet";

/// A member's end of a join, on a stream the handshake proved for op `join` under `secret`: read
/// the joining member's [`Registration`] and, while that fleet is held ([`write_held`]), record
/// it in the book in `state_dir` with the address `from` it came from; a key it names must be
/// the one `proof` ([`Served::peer_node`]) proved for that node, and a signature it carries must
/// hold. Any member serves it (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17). It answers
/// `OK` with the member `member` gives and the book's size, or `ERR <why>`: a malformed record, an
/// unproved key or a signature that does not hold, a refusal of [`record`], an unreadable book,
/// the member's own node id, a fleet left or rotated meanwhile ([`MOVED_ON`]), or a member that
/// cannot name itself (no node id, or a host that is no [`is_host`], which the joiner's
/// [`register`] would refuse after the book recorded it).
pub fn serve_join<S: Read + Write>(
    stream: &mut S,
    (from, proof): (IpAddr, Option<&NodeProof>),
    (state_dir, secret): (&Path, &FleetSecret),
    member: impl FnOnce() -> Result<Issuer, String>,
    now_ms: u64,
) -> io::Result<Result<(Recorded, Peer), String>> {
    serve_join_with_denied(
        stream,
        (from, proof),
        (state_dir, secret),
        member,
        now_ms,
        (&[], None),
    )
}

/// [`serve_join`], refusing any node on `denied_nodes` and pruning expired (`>= 90d`) or denied
/// entries from the book; with `log`, this Mac's membership log's keys, also refusing a joiner
/// whose proved key the log took out ([`NOT_ADMITTED`]), whatever secret it holds. A node this
/// Mac's copy of its issuer's book binds to another key is refused too ([`copy_rebinds`]).
pub fn serve_join_with_denied<S: Read + Write>(
    stream: &mut S,
    (from, proof): (IpAddr, Option<&NodeProof>),
    (state_dir, secret): (&Path, &FleetSecret),
    member: impl FnOnce() -> Result<Issuer, String>,
    now_ms: u64,
    (denied_nodes, log): (&[String], Option<&MemberKeys>),
) -> io::Result<Result<(Recorded, Peer), String>> {
    let line = read_record(stream)?;
    let outcome = match Registration::parse(&line) {
        Err(why) => Err(why.to_owned()),
        Ok(reg)
            if crate::fleet::is_denied_node(&reg.node, denied_nodes)
                || reg
                    .key
                    .as_ref()
                    .is_some_and(|key| crate::fleet::is_denied_key(key, denied_nodes))
                || proof.is_some_and(|proof| {
                    crate::fleet::is_denied_node(&proof.node, denied_nodes)
                        || crate::fleet::is_denied_key(&proof.key, denied_nodes)
                }) =>
        {
            Err("that node is on the swarm roster's deny-list".to_owned())
        }
        Ok(reg)
            if reg.key.is_some_and(|key| {
                proof.is_none_or(|proof| proof.node != reg.node || proof.key != key)
            }) || proof.is_some_and(|proof| proof.node != reg.node) =>
        {
            Err(
                "the registration names a node key this connection did not prove: a member \
                 proves its key as it dials (`aterm fleet join` does)"
                    .to_owned(),
            )
        }
        Ok(reg) if !reg.signature_fits() => Err(
            "the registration's signature does not hold over what it names under its key"
                .to_owned(),
        ),
        Ok(reg) => admitted(&reg, proof, log)
            .and_then(|()| {
                member()
                    .and_then(|member| {
                        if is_host(&member.host) {
                            Ok(member)
                        } else {
                            Err("its host name is no host a member can dial".to_owned())
                        }
                    })
                    .map_err(|why| format!("the answering member cannot name itself: {why}"))
            })
            .and_then(|member| {
                if reg.node == member.node {
                    return Err(format!(
                        "{OWN_NODE}, and a member does not join itself (a Mac cloned or restored \
                         from it carries its node)"
                    ));
                }
                let peer = Peer {
                    node: Some(reg.node),
                    key: reg.key,
                    key_from: None,
                    host: reg.host,
                    addr: Some(from.to_canonical()),
                    port: reg.port,
                    version: Some(reg.version),
                    seen_ms: Some(now_ms),
                    via: Via::Joined,
                    fleet: None,
                    signed: reg.signed,
                };
                let recorded_as = peer.clone();
                // Only while the fleet the connection was proved under is held.
                match write_held(
                    state_dir,
                    Began::Under(&Held::Key(secret.clone())),
                    |book| {
                        book.update(|book| {
                            copy_rebinds(&copied_keys(state_dir, book), &peer, log, denied_nodes)?;
                            unbind_replaced(book, &peer, log, denied_nodes)?;
                            record_with_denied(book, peer, denied_nodes)
                                .map(|recorded| (recorded, book.len()))
                        })
                    },
                ) {
                    Ok(recorded) => recorded,
                    Err(Unwritten::Moved) => Err(MOVED_ON.to_owned()),
                    Err(Unwritten::Lock(why)) => Err(why),
                }
                .map(|(recorded, peers)| (recorded, peers, member, recorded_as))
            }),
    };
    let (answer, outcome) = match outcome {
        Ok((recorded, peers, issuer, recorded_as)) => (
            join_answer(
                &issuer,
                peers,
                fleet_seed::read(state_dir).ok().flatten().as_ref(),
            ),
            Ok((recorded, recorded_as)),
        ),
        Err(why) => {
            let why = aterm_codec::tty::one_line(&why, 400);
            (format!("ERR {why}"), Err(why))
        }
    };
    stream.write_all(format!("{answer}\n").as_bytes())?;
    stream.flush()?;
    Ok(outcome)
}

/// Whether `log`, this Mac's membership log's keys, lets the joiner `reg` names join: unless it
/// took the key `reg` or `proof` proves out, whatever secret the joiner holds. A key it names no
/// way joins by the secret, as with no log ([`MemberKeys`]).
fn admitted(
    reg: &Registration,
    proof: Option<&NodeProof>,
    log: Option<&MemberKeys>,
) -> Result<(), String> {
    let key = reg.key.as_ref().or_else(|| proof.map(|proof| &proof.key));
    match key.and_then(|key| log.and_then(|log| log.standing(key))) {
        Some(Standing::Out(why)) => Err(format!(
            "{NOT_ADMITTED}: it took that key out ({})",
            aterm_codec::tty::one_line(why, 160)
        )),
        Some(Standing::Member) | None => Ok(()),
    }
}

/// Clear the way for `peer`, where the book binds its node id to another key the log `log` took
/// out or `denied_nodes` denies (the member rotated its node key): that binding goes, the entry of
/// the member this Mac joined through staying that entry. Another key the log admits is another
/// member's, and the join is refused; one it names no way stays bound, so [`record`] refuses a
/// rebinding of it.
fn unbind_replaced(
    book: &mut Vec<Peer>,
    peer: &Peer,
    log: Option<&MemberKeys>,
    denied_nodes: &[String],
) -> Result<(), String> {
    let Some(at) = book
        .iter()
        .position(|known| known.node.is_some() && known.node == peer.node)
    else {
        return Ok(());
    };
    match (book[at].key, peer.key) {
        (Some(bound), Some(proved))
            if bound != proved
                && !crate::fleet::is_denied_key(&bound, denied_nodes)
                && log.is_some_and(|log| log.admits(&bound)) =>
        {
            let whole = peer.node.as_deref().unwrap_or("-");
            Err(format!(
                "node {} is bound to another key the membership log admits: another member \
                 holds that node id (a Mac cloned or restored from it)",
                whole.get(..8).unwrap_or(whole)
            ))
        }
        (Some(bound), Some(proved))
            if bound != proved
                && (log.is_some_and(|log| log.holds_out(&bound))
                    || crate::fleet::is_denied_key(&bound, denied_nodes)) =>
        {
            if book[at].via == Via::Issuer {
                let known = &mut book[at];
                known.key = None;
                known.key_from = None;
                known.addr = None;
                known.signed = None;
            } else {
                book.remove(at);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Refuse `peer` where this Mac's copy of its issuer's book (`copies`, [`copied_keys`]) binds its
/// node to another key than `peer` proves, or to a key where `peer` proves none, unless the log
/// `log` took that key out or `denied_nodes` denies it: only the issuer's `peers rm` lets a node
/// it bound bind afresh, as a join recorded only there did.
fn copy_rebinds(
    copies: &Keyring,
    peer: &Peer,
    log: Option<&MemberKeys>,
    denied_nodes: &[String],
) -> Result<(), String> {
    let Some(node) = peer.node.as_deref() else {
        return Ok(());
    };
    match (copies.bound(node), peer.key) {
        (Some(bound), proved)
            if proved != Some(bound)
                && !log.is_some_and(|log| log.holds_out(&bound))
                && !(crate::fleet::is_denied_key(&bound, denied_nodes) && proved.is_some()) =>
        {
            Err(format!(
                "node {} is bound to another key in this Mac's copy of its issuer's book: not \
                 that node, or one that lost its node key — then `aterm fleet peers rm {node}` \
                 {REBIND}",
                node.get(..8).unwrap_or(node)
            ))
        }
        _ => Ok(()),
    }
}

/// The keys this Mac's copy of the issuer `book` names binds ([`read_issuer_book`] in
/// `state_dir`): none where the book names no issuer.
fn copied_keys(state_dir: &Path, book: &[Peer]) -> Keyring {
    issuer_of(book)
        .and_then(|issuer| issuer.node.as_deref())
        .map(|issuer| Keyring::of(&read_issuer_book(state_dir, issuer)))
        .unwrap_or_default()
}

/// Why a registration ([`register`]) recorded nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unregistered {
    /// The machine at the code's address answered and recorded nothing: its own words (a
    /// member's [`NOT_THE_ISSUER`] among them), or an answer that is no join's.
    Answered(String),
    /// No answer came: the registration was not sent, the exchange failed, or time ran out.
    Unanswered(String),
}

impl fmt::Display for Unregistered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Answered(why) | Self::Unanswered(why) => f.write_str(why),
        }
    }
}

/// The member's end of a join, on a stream proved for op `join`: send `reg`, read the answer.
pub fn register<S: Read + Write>(
    stream: &mut S,
    reg: &Registration,
) -> Result<Joined, Unregistered> {
    let unanswered = Unregistered::Unanswered;
    stream
        .write_all(reg.line().as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| unanswered(format!("the registration could not be sent: {e}")))?;
    let line =
        read_record(stream).map_err(|e| unanswered(format!("the issuer did not answer: {e}")))?;
    if let Some(why) = line.strip_prefix("ERR ") {
        return Err(Unregistered::Answered(aterm_codec::tty::one_line(why, 400)));
    }
    joined_of(&line).map_err(Unregistered::Answered)
}

/// A join's `OK` answer from `issuer`, whose book holds `peers` with the joiner, carrying the
/// fleet seed it holds, its root's or relayed, which the joiner verifies (machine names §3.3);
/// an older joiner reads no field it does not know ([`joined_of`]).
fn join_answer(issuer: &Issuer, peers: usize, seed: Option<&fleet_seed::FleetSeed>) -> String {
    format!(
        "OK node={} host={} port={} version={} peers={peers}{}",
        issuer.node,
        aterm_codec::pct::encode(&issuer.host),
        issuer.port,
        aterm_codec::pct::encode(&issuer.version),
        seed.map_or_else(String::new, |seed| format!(" {}", seed.words())),
    )
}

/// The issuer that a join's `OK` answer `line` names.
fn joined_of(line: &str) -> Result<Joined, String> {
    let not_an_answer = || "the issuer's answer is no join answer (an older aterm?)".to_owned();
    let fields = fields(line.strip_prefix("OK ").ok_or_else(not_an_answer)?);
    let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    let node = get("node")
        .filter(|node| is_node_id(node))
        .ok_or_else(not_an_answer)?;
    let host = aterm_codec::pct::decode(get("host").ok_or_else(not_an_answer)?);
    let port = get("port")
        .filter(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(not_an_answer)?;
    let version = version_word(&aterm_codec::pct::decode(
        get("version").ok_or_else(not_an_answer)?,
    ))
    .ok_or_else(not_an_answer)?;
    let peers = get("peers")
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse().ok())
        .ok_or_else(not_an_answer)?;
    if !is_host(&host) {
        return Err(not_an_answer());
    }
    Ok(Joined {
        issuer: Issuer {
            node: node.to_owned(),
            host,
            port,
            version,
        },
        peers,
        seed: fleet_seed::FleetSeed::of_line(line),
    })
}

/// [`register`] on a peer `reached` for op `join`, the whole exchange by `deadline`: socket
/// timeouts bound each read and write, and a [`HandshakeWatchdog`] shuts the socket at the
/// deadline, since rustls reads a whole TLS record per read and an issuer trickling bytes
/// could otherwise outlast any per-read timeout.
pub fn register_by(
    reached: &mut Reached,
    reg: &Registration,
    deadline: Instant,
) -> Result<Joined, Unregistered> {
    let left = deadline.saturating_duration_since(Instant::now());
    let late = || {
        Unregistered::Unanswered(format!(
            "the issuer did not answer within {:.1}s",
            left.as_secs_f64()
        ))
    };
    if left.is_zero() {
        return Err(late());
    }
    #[cfg(any(test, debug_assertions))]
    if held_at(reached.addr, true, left) {
        return Err(late());
    }
    let unbounded =
        |e: io::Error| Unregistered::Unanswered(format!("could not bound the registration: {e}"));
    let tcp = reached.transport.stream().get_ref();
    tcp.set_read_timeout(Some(left))
        .and_then(|()| tcp.set_write_timeout(Some(left)))
        .map_err(unbounded)?;
    let watchdog = HandshakeWatchdog::arm(tcp, left).map_err(unbounded)?;
    let joined = register(reached.transport.stream(), reg);
    match (watchdog.finish(), joined) {
        (_, Ok(joined)) => Ok(joined),
        (Claim::Authed, Err(why)) => Err(why),
        (_, Err(_)) => Err(late()),
    }
}

/// How many answers fill a heartbeat round ([`heartbeat_members`]): past them it asks only the
/// members marked missed (and the member it joined through, while that served no entry it
/// signed), until one of them misses again or the round holds [`HEARTBEAT_ANSWERS_MAX`].
pub const HEARTBEAT_MEMBERS: usize = 3;

/// The most answers one heartbeat round takes ([`heartbeat_members`]): its
/// [`HEARTBEAT_MEMBERS`], then as many again of the members marked missed, so a heal that
/// brings many members back costs each round at most twice the registrations, book reads and
/// facts pushes it makes otherwise, and the rest come back the rounds after, before members
/// nobody heard from (their books carry the rest's newer entries).
pub const HEARTBEAT_ANSWERS_MAX: usize = 2 * HEARTBEAT_MEMBERS;

/// The last time any member answered this Mac, as [`read_answered`] holds it.
#[must_use]
pub fn last_answered(dir: &Path) -> Option<u64> {
    read_answered(dir).into_values().max()
}

/// One member a heartbeat round reached ([`heartbeat_members`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answered {
    /// Its node id.
    pub node: String,
    /// Where it answered.
    pub at: SocketAddr,
    /// Its answer to the registration.
    pub joined: Joined,
    /// How many entries of its book changed this Mac's ([`merge_signed`]).
    pub merged: usize,
}

/// One heartbeat round (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17): register `reg`,
/// signed by `me` at its durable self-seq, with [`HEARTBEAT_MEMBERS`] members this Mac can reach,
/// and past them with members marked missed it reaches, until one of those misses again or the
/// round holds [`HEARTBEAT_ANSWERS_MAX`] answers, within `within`. The members are the book's
/// entries in `state_dir` that `log`, this Mac's membership log's keys, admits (with no log,
/// those its book binds a key to) ([`round_order`]: the one that answered last, then the rest
/// the round asked longest ago first, each marked missed ([`missed_since_answered`]) behind
/// them, those heard from since their last miss first, unless that miss too came while they
/// were heard from ([`read_missed_heard`]), then the one missed longest ago first). Once
/// [`HEARTBEAT_MEMBERS`] answered, the round passes over the rest not marked missed, save the
/// member it joined through while that runs an aterm before 0.147 (it served no entry it signed,
/// so this Mac's entry there goes stale with no registration of its own), and goes on through
/// those marked missed until the first dial that misses (a member marked missed that missed
/// earlier in the round ends it at its three answers): a heal is heard within a round, and
/// members gone dark for good cost a round one dial past its answers. Then, where they did not
/// fill the round, each place `hints` gives (`_alab._tcp` adverts a browse resolved, asked only
/// then) that proves such a member it has not reached. Each dial of a member takes at most a
/// [`HEARTBEAT_MEMBERS`]` + 1`th of `within` to connect and prove its node, one past the
/// round's [`HEARTBEAT_MEMBERS`] answers its registration and book read with them, and the
/// members take no more of it than leaves one such share for the hints, so members gone dark
/// cannot spend the round. Each answering member's entry is brought to where it answered, what of
/// its book `log` does not hold out is merged ([`merge_signed`]), the book of the member it
/// joined through becomes this Mac's copy of it ([`HeldBook::keep_issuer_book`]), and when it
/// answered is noted, as is when each member dialled did not answer, and which of those misses
/// came while the member was marked missed and heard from since ([`HeldBook::note_stamps`]).
/// # Errors
/// The book could not be read or updated, or the local signed descriptor could not be committed.
pub fn heartbeat_members(
    state_dir: &Path,
    (secret, me): (&FleetSecret, &NodeIdentity),
    reg: &Registration,
    within: Duration,
    (log, hints): (Option<&MemberKeys>, &dyn Fn() -> Vec<SocketAddr>),
) -> Result<Vec<Answered>, String> {
    heartbeat_members_with_denied(state_dir, (secret, me), reg, within, (log, &[], hints))
}

/// [`heartbeat_members`], refusing any node on `denied_nodes` as a heartbeat target, pruning
/// `denied_nodes` from `book` and from merged entries ([`merge_signed`]), and omitting denied or
/// log-held-out entries from the stored copy of the issuer's book.
/// # Errors
/// The book could not be read or updated, or the local signed descriptor could not be committed.
pub fn heartbeat_members_with_denied(
    state_dir: &Path,
    (secret, me): (&FleetSecret, &NodeIdentity),
    reg: &Registration,
    within: Duration,
    (log, denied_nodes, hints): (Option<&MemberKeys>, &[String], &dyn Fn() -> Vec<SocketAddr>),
) -> Result<Vec<Answered>, String> {
    heartbeat_members_while(
        state_dir,
        (secret, me),
        reg,
        within,
        (log, denied_nodes, hints),
        &AtomicBool::new(true),
    )
}

/// [`heartbeat_members_with_denied`] for a listener generation that remains `current`.
/// An ended generation stops at its next exchange or commit boundary, and cannot allocate a
/// descriptor version after a newer generation's publication. An exchange already sent can
/// finish remotely; its descriptor precedes a newer generation's. Under a log that cannot be read
/// now ([`MemberKeys::unreadable`]) the round dials no one and changes no book.
/// # Errors
/// The book could not be read or updated, or the local signed descriptor could not be committed.
pub fn heartbeat_members_while(
    state_dir: &Path,
    (secret, me): (&FleetSecret, &NodeIdentity),
    reg: &Registration,
    within: Duration,
    (log, denied_nodes, hints): (Option<&MemberKeys>, &[String], &dyn Fn() -> Vec<SocketAddr>),
    current: &AtomicBool,
) -> Result<Vec<Answered>, String> {
    // A log that cannot be read now admits no member to dial and judges no book entry: the
    // round dials no one and leaves the book as it is, as a listener with no deny list read.
    if !current.load(Ordering::Acquire) || log.is_some_and(|log| log.unread().is_some()) {
        return Ok(Vec::new());
    }
    let Some(reg) = reg
        .clone()
        .signed_in_while(state_dir, me, current)
        .map_err(|e| e.to_string())?
    else {
        return Ok(Vec::new());
    };
    let peers = read_peers(state_dir).map_err(|e| e.to_string())?;
    let issuer = peers
        .iter()
        .find(|peer| peer.via == Via::Issuer)
        .and_then(|peer| peer.node.clone());
    let issuer_copies = issuer
        .as_deref()
        .map(|node| read_issuer_book(state_dir, node))
        .unwrap_or_default();
    if !current.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    prune_pushed_facts(state_dir, &peers, denied_nodes, log);
    if !current.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    prune_pushed_facts(state_dir, &issuer_copies, denied_nodes, log);
    let keys = Keyring::read(state_dir);
    let own = me.node();
    // A key the log names no way is judged by the book, as with no log ([`MemberKeys`]).
    let admits = |node: &str, key: &NodeKey| {
        if crate::fleet::is_denied_node(node, denied_nodes)
            || crate::fleet::is_denied_key(key, denied_nodes)
        {
            return false;
        }
        if let Some(bound) = keys.bound(node)
            && bound != *key
            && !log.is_some_and(|log| log.holds_out(&bound))
            && !crate::fleet::is_denied_key(&bound, denied_nodes)
        {
            return false;
        }
        match log {
            Some(log) if log.admits(key) => true,
            Some(log) if log.holds_out(key) => false,
            _ => keys.bound(node) == Some(*key),
        }
    };
    let members: Vec<&Peer> = peers
        .iter()
        .filter(|peer| {
            peer.via != Via::Manual
                && peer.node.as_deref().is_some_and(|node| node != own)
                && peer
                    .node
                    .as_deref()
                    .zip(peer.key.as_ref())
                    .is_some_and(|(node, key)| admits(node, key))
        })
        .collect();
    let began_ms = unix_ms_now().unwrap_or_default();
    // The network this round begins on, read from the kernel now: an answer is booked on it only
    // where this Mac is still on it when the answer is noted (`aterm_netplace::Answer::network`).
    #[cfg(unix)]
    let began_on = aterm_netplace::facts::network_now();
    let book_members: Vec<(String, Option<IpAddr>)> = members
        .iter()
        .filter_map(|peer| Some((peer.node.clone()?, peer.addr)))
        .collect();
    if !current.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    // Away from the ALab LAN the round passes over the members behind it, save the first round
    // on each network and one every half hour (`aterm_netplace::routes::Round`).
    let mut pace = Pace::begin(state_dir);
    let members: Vec<&Peer> = members
        .into_iter()
        .filter(|peer| pace.dials(peer))
        .collect();
    let stamps = (
        read_answered(state_dir),
        read_missed(state_dir),
        read_missed_heard(state_dir),
    );
    let members = round_order(members, (&stamps.0, &stamps.1, &stamps.2));
    let mut dialled = 0_usize;
    let capped_within = within.min(MAX_WITHIN);
    let deadline = (deadline_after(capped_within), within);
    // Each member's dial gets a share of the round, and one share is left for the hints.
    let share = capped_within / u32::try_from(HEARTBEAT_MEMBERS + 1).unwrap_or(u32::MAX);
    let members_until = deadline.0.checked_sub(share).unwrap_or(deadline.0);
    let dial_by = || {
        let by = deadline_after(share).min(deadline.0);
        (by, by.saturating_duration_since(Instant::now()))
    };
    let mut answered: Vec<(Answered, Option<Book>, Option<NodeKey>)> = Vec::new();
    let mut missed: Vec<String> = Vec::new();
    // Of those, the members marked missed and heard from since that missed again.
    let mut missed_heard: Vec<String> = Vec::new();
    // Dials that met something other than silence (`dial_blocked`): a round of only those is no
    // evidence, for this Mac's network place, that the members are elsewhere.
    let mut blocked = 0_usize;
    // Past three answers the round passes over the members not marked missed, save the member it
    // joined through while that served no entry it signed, and goes on through those marked
    // missed (last in `round_order`) until a dial misses or it holds `HEARTBEAT_ANSWERS_MAX`
    // answers, each such dial whole within its share: a heal is heard within a round, and
    // members gone dark for good cost a round one dial past its three answers, by its share.
    let mut missed_again = false;
    for member in members {
        if !current.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }
        if Instant::now() >= members_until {
            break;
        }
        let marked = missed_since_answered(member, (&stamps.0, &stamps.1));
        let past = answered.len() >= HEARTBEAT_MEMBERS;
        if past {
            if missed_again || answered.len() >= HEARTBEAT_ANSWERS_MAX {
                break;
            }
            if !marked && !(member.via == Via::Issuer && member.signed.is_none()) {
                continue;
            }
        }
        let target = target_of(member);
        dialled += 1;
        let by = dial_by();
        // Within the round its registration may run a second past the round's end, so a member
        // reached at the end still records this Mac; past three answers, to its share's end.
        let bounds = if past {
            (by.0, (by.0, share))
        } else {
            (
                deadline.0.max(deadline_after(Duration::from_secs(1))),
                deadline,
            )
        };
        let beat = match reach_node(&target, &keys, (secret, me), FleetOp::Join, by) {
            Ok((reached, node)) => {
                let beat = beat(reached, &node, (secret, me), &reg, bounds, current);
                if beat.is_none() {
                    blocked += 1;
                }
                beat
            }
            Err(why) => {
                if dial_blocked(&why, |addr| pace.on_this_network(addr)) {
                    blocked += 1;
                }
                None
            }
        };
        match beat {
            Some(beat) => answered.push(beat),
            None => {
                missed_again |= marked || past;
                if marked && heard_since_missed(member, &stamps.1) {
                    missed_heard.extend(member.node.clone());
                }
                missed.extend(member.node.clone());
            }
        }
    }
    if !current.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    // The adverts are resolved only for a round the book's members did not fill.
    let hints = if answered.len() < HEARTBEAT_MEMBERS && Instant::now() < deadline.0 {
        hints()
    } else {
        Vec::new()
    };
    for hint in &hints {
        if !current.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }
        if answered.len() >= HEARTBEAT_MEMBERS || Instant::now() >= deadline.0 {
            break;
        }
        if answered.iter().any(|(done, _, _)| done.at == *hint) {
            continue;
        }
        let ip = hint.ip().to_canonical();
        dialled += 1;
        let reached = match dial(
            &ip.to_string(),
            Some(ip),
            hint.port(),
            (secret, Some(me)),
            FleetOp::Join,
            dial_by().1,
        ) {
            Ok(reached) => reached,
            Err(why) => {
                if dial_blocked(&why, |addr| pace.on_this_network(addr)) {
                    blocked += 1;
                }
                continue;
            }
        };
        let Some(proof) = reached.peer.clone() else {
            continue;
        };
        if proof.node == own
            || answered.iter().any(|(done, _, _)| done.node == proof.node)
            || !admits(&proof.node, &proof.key)
        {
            continue;
        }
        let bounds = (
            deadline.0.max(deadline_after(Duration::from_secs(1))),
            deadline,
        );
        if let Some(beat) = beat(reached, &proof.node, (secret, me), &reg, bounds, current) {
            answered.push(beat);
        }
    }
    if !current.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    // A member a hint reached answered, whatever its dial from the book came to.
    missed.retain(|node| !answered.iter().any(|(done, _, _)| done.node == *node));
    missed_heard.retain(|node| missed.contains(node));
    let network = pace.network();
    let is_barred = |peer: &Peer| {
        peer.node
            .as_deref()
            .is_some_and(|node| crate::fleet::is_denied_node(node, denied_nodes))
            || peer.key.is_some_and(|key| {
                crate::fleet::is_denied_key(&key, denied_nodes)
                    || log.is_some_and(|log| log.holds_out(&key))
            })
    };
    let has_barred = peers.iter().any(is_barred) || issuer_copies.iter().any(is_barred);
    if answered.is_empty() && missed.is_empty() && !has_barred {
        if current.load(Ordering::Acquire) {
            pace.end(state_dir);
        }
        return Ok(Vec::new());
    }
    let now = unix_ms_now().unwrap_or_default();
    let kept = write_held(
        state_dir,
        Began::Under(&Held::Key(secret.clone())),
        |book| -> Result<bool, String> {
            if !current.load(Ordering::Acquire) {
                return Ok(false);
            }
            book.observe_self_entries(
                me,
                answered.iter().flat_map(|(_, fetched, _)| {
                    fetched.iter().flat_map(|book| book.peers.iter().cloned())
                }),
            )
            .map_err(|e| e.to_string())?;
            book.update(|book| {
                // A member this Mac's book dropped while the round was out (`aterm fleet peers
                // rm`) stays dropped: a book fetched before its issuer dropped it too would bind
                // it here again, and no later round takes an entry of this book out.
                let dropped: Vec<String> = peers
                    .iter()
                    .filter_map(|peer| peer.node.clone())
                    .filter(|node| !book.iter().any(|peer| peer.node.as_ref() == Some(node)))
                    .collect();
                prune_book_at(book, Some(now), denied_nodes);
                book.retain(|peer| {
                    !peer
                        .key
                        .is_some_and(|key| log.is_some_and(|log| log.holds_out(&key)))
                });
                for (done, fetched, proved_key) in &mut answered {
                    let copies = copied_keys(state_dir, book);
                    let fetched = fetched
                        .as_ref()
                        .map_or(&[][..], |book| book.peers.as_slice());
                    prune_pushed_facts(state_dir, fetched, denied_nodes, log);
                    let fetched: Vec<Peer> = fetched
                        .iter()
                        .filter(|entry| {
                            !entry
                                .node
                                .as_ref()
                                .is_some_and(|node| dropped.contains(node))
                        })
                        .cloned()
                        .collect();
                    done.merged = merge_signed(
                        book,
                        &fetched,
                        (Some(own), now),
                        (log, denied_nodes, &copies),
                    );
                    if let Some(slot) = book.iter_mut().find(|peer| {
                        peer.node.as_deref() == Some(done.node.as_str())
                            && (peer.key.is_none() || peer.key == *proved_key)
                    }) {
                        slot.addr = Some(done.at.ip().to_canonical());
                        // The book can carry a newer descriptor than the earlier join reply.
                        // Only the node's signed entry may set its signed fields.
                        if slot.signed.is_none() {
                            slot.port = done.joined.issuer.port;
                            slot.host.clone_from(&done.joined.issuer.host);
                            slot.version = Some(done.joined.issuer.version.clone());
                        }
                        slot.seen_ms = Some(now);
                    }
                }
                Ok(())
            })?;
            // What `aterm fleet peers` says this Mac is away from every member since, and which
            // members the next round passes over until they answer again.
            let answers: Vec<(String, u64)> = answered
                .iter()
                .map(|(done, _, _)| (node_answer_key(&done.node), now))
                .collect();
            let stamped = |nodes: &[String]| -> Vec<(String, u64)> {
                nodes
                    .iter()
                    .map(|node| (node_answer_key(node), now))
                    .collect()
            };
            let _ = book.note_stamps(&answers, (&stamped(&missed), &stamped(&missed_heard)));
            // A copy another read kept while this round was out (`aterm fleet peers`, a survey)
            // can be newer than the book this round fetched, and its barred rewrite starts from
            // the copy this round began with: that copy stays until the next round reads the
            // issuer again.
            let kept_since = issuer
                .as_deref()
                .is_some_and(|node| read_issuer_book(state_dir, node) != issuer_copies);
            // The member this Mac joined through answered with its book, which is this Mac's copy
            // of it again: a member it recorded since binds here, and one it took out no more.
            match answered
                .iter()
                .find(|(done, _, _)| issuer.as_deref() == Some(done.node.as_str()))
            {
                _ if kept_since => Ok(()),
                Some((done, Some(fetched), _)) => {
                    let mut copies = copies_of(fetched, Some(own));
                    copies.retain(|copy| !is_barred(copy));
                    book.keep_issuer_book(Some((&done.node, &copies)))
                        .map_err(|e| format!("{ISSUER_BOOK_FILE}: {e}"))
                }
                _ if issuer_copies.iter().any(is_barred) => {
                    if let Some(issuer_node) = issuer.as_deref() {
                        let mut copies = issuer_copies.clone();
                        copies.retain(|copy| !is_barred(copy));
                        book.keep_issuer_book(Some((issuer_node, &copies)))
                            .map_err(|e| format!("{ISSUER_BOOK_FILE}: {e}"))
                    } else {
                        Ok(())
                    }
                }
                _ => Ok(()),
            }?;
            // The seed a member's reply relays in its trailer, once this book, merged, binds its
            // minter (machine names §3.3). One not written now is offered again by the next
            // round's reads, so its failure stops none of this round's writes.
            let _ = book.adopt_relayed_seed(
                me,
                secret.mark(),
                answered
                    .iter()
                    .filter_map(|(_, fetched, _)| fetched.as_ref()?.seed),
                now,
            );
            // A root holding none mints once its wait over rounds that read its members' books,
            // none of which relayed its seed, is over (machine names §3.3); the next round's
            // readers adopt it.
            let _ = book.mint_unheld_seed(
                me,
                secret.mark(),
                (
                    now,
                    answered.iter().any(|(_, fetched, _)| fetched.is_some()),
                ),
            );
            Ok(true)
        },
    );
    match kept {
        Ok(written) => {
            if !written? {
                return Ok(Vec::new());
            }
        }
        Err(Unwritten::Moved) => return Ok(Vec::new()),
        Err(why) => return Err(why.to_string()),
    }
    if !current.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    pace.end(state_dir);
    let answered: Vec<Answered> = answered.into_iter().map(|(done, _, _)| done).collect();
    #[cfg(unix)]
    note_place(
        state_dir,
        &answered,
        (&book_members, dialled, blocked),
        (began_ms, now, network.as_ref()),
        (began_on.as_ref(), log.is_some()),
    );
    #[cfg(not(unix))]
    let _ = (&book_members, began_ms, dialled, blocked, &network);
    Ok(answered)
}

/// WHETHER A DIAL THAT GOT NO ANSWER MET SOMETHING OTHER THAN SILENCE, for the network place's
/// silent-round rule (`aterm_netplace::Round::blocked`): a host at the address answered (a
/// refused connect, a connect that went on to a handshake, a proof or an exchange that failed or
/// ran out of time), or this Mac's own way onto this network refused the connect (no network or
/// not permitted, to an address `on_this_network` says lies on the network this Mac is on: a
/// sandbox, a firewall). Silence is the rest: a connect that timed out, a host down (no route to
/// it: no ARP answer on this network, or the home LAN's addresses from a phone hotspot), and no
/// address or lookup at all.
#[must_use]
pub fn dial_blocked(why: &Unreached, on_this_network: impl Fn(IpAddr) -> bool) -> bool {
    match why {
        Unreached::Connect(at, e) => match e.kind() {
            io::ErrorKind::ConnectionRefused => true,
            io::ErrorKind::NetworkUnreachable | io::ErrorKind::PermissionDenied => {
                on_this_network(at.ip())
            }
            _ => false,
        },
        Unreached::Deadline(stage, _) => *stage != Stage::Connect,
        Unreached::Handshake(..)
        | Unreached::Reply(..)
        | Unreached::OtherNode { .. }
        | Unreached::Impostor { .. }
        | Unreached::Misnamed { .. }
        | Unreached::Unproved { .. }
        | Unreached::Unbound { .. } => true,
        Unreached::Resolve(_)
        | Unreached::Lookup(..)
        | Unreached::NotFleet(_)
        | Unreached::Local(_) => false,
    }
}

/// A heartbeat round's pacing: on unix, away from the ALab LAN, it passes over the members behind
/// it, save the first round on each network and one every half hour
/// ([`aterm_netplace::routes::Round`]); elsewhere it dials every member.
struct Pace {
    #[cfg(unix)]
    round: aterm_netplace::routes::Round,
}

impl Pace {
    fn begin(state_dir: &Path) -> Self {
        #[cfg(not(unix))]
        let _ = state_dir;
        Self {
            #[cfg(unix)]
            round: aterm_netplace::routes::Round::begin(state_dir, "fleet"),
        }
    }

    /// Whether this round dials `peer`.
    fn dials(&mut self, peer: &Peer) -> bool {
        #[cfg(unix)]
        return self.round.dials(peer.node.as_deref(), peer.addr);
        #[cfg(not(unix))]
        {
            let _ = peer;
            true
        }
    }

    /// The network (strong id) this round began on, which the book's answers are booked on;
    /// `None` off unix, where no place is read.
    fn network(&self) -> Option<String> {
        #[cfg(unix)]
        return self
            .round
            .routes()
            .network()
            .filter(|network| network.is_strong())
            .map(|network| network.as_str().to_owned());
        #[cfg(not(unix))]
        None
    }

    /// Whether `addr` lies on the network this round began on (never off unix, where no place
    /// is read).
    fn on_this_network(&self, addr: IpAddr) -> bool {
        #[cfg(unix)]
        return self.round.on_this_network(addr);
        #[cfg(not(unix))]
        {
            let _ = addr;
            false
        }
    }

    /// Ends the round (the log says once per network what it passed over).
    fn end(self, state_dir: &Path) {
        #[cfg(unix)]
        self.round.end(state_dir);
        #[cfg(not(unix))]
        let _ = state_dir;
    }
}

/// Tells this Mac's network place (`aterm_netplace`, under `state_dir`) what a heartbeat round
/// heard: each member that answered (every one proved its key, [`reach_node`]) and where, the
/// round itself (how many of the members it dialled answered), and the book's admitted members
/// with where each was and when it last answered, which the place reads a home network from
/// before it has learned one. A round that dialled no member (`dialled`: none, or every one
/// passed over away from the ALab LAN) notes no round: its silence is no member's. `blocked`
/// counts its dials that met something other than silence ([`dial_blocked`]). Each answer is
/// booked on `began_on`, the network the round began on, where this Mac is still on it, and
/// marked `logged` where the round ran under this Mac's vault's membership log, which an
/// isolated job (no vault, no deny list) never has: only those answers are evidence for the
/// vault's automatic admission ([`aterm_netplace::heard::note_answer`]).
#[cfg(unix)]
fn note_place(
    state_dir: &Path,
    answered: &[Answered],
    (book_members, dialled, blocked): (&[(String, Option<IpAddr>)], usize, usize),
    (began_ms, now, network): (u64, u64, Option<&String>),
    heard: (Option<&aterm_netplace::NetworkId>, bool),
) {
    let dir = aterm_netplace::record::dir(state_dir);
    for done in answered {
        aterm_netplace::heard::note_answer(
            &dir,
            (aterm_netplace::Lane::Fleet, &done.node),
            Some(done.at.ip()),
            heard,
            (began_ms, now),
        );
    }
    if !book_members.is_empty() {
        if dialled > 0 {
            aterm_netplace::heard::note_round(
                &dir,
                aterm_netplace::Lane::Fleet,
                (answered.len(), blocked),
                (began_ms, now),
            );
        }
        let stamps = read_answered(state_dir);
        let network = network.and_then(|id| aterm_netplace::NetworkId::parse(id));
        let _ = aterm_netplace::heard::write_book(
            &dir,
            book_members.iter().map(|(node, addr)| {
                (
                    node.clone(),
                    *addr,
                    stamps.get(&node_answer_key(node)).copied(),
                )
            }),
            network.as_ref(),
        );
    }
}

/// When `peer` last answered this Mac, or a round's dial of it last missed, as `stamps` (one map
/// of [`read_answered`] or [`read_missed`]) holds it.
fn stamp_of(stamps: &Stamps, peer: &Peer) -> Option<u64> {
    peer.node
        .as_deref()
        .and_then(|node| stamps.get(&node_answer_key(node)).copied())
}

/// Whether `peer` is marked missed: a heartbeat round's dial of it missed since it last answered
/// this Mac (`stamps`: [`read_answered`], [`read_missed`]).
fn missed_since_answered(peer: &Peer, (answered, missed): (&Stamps, &Stamps)) -> bool {
    stamp_of(missed, peer) > stamp_of(answered, peer)
}

/// Whether `peer` was heard from since a heartbeat round's dial of it last missed (`missed`:
/// [`read_missed`]): its entry's `seen_ms` is later, from a registration it made here or a newer
/// entry it signed read from a member's book.
fn heard_since_missed(peer: &Peer, missed: &Stamps) -> bool {
    peer.seen_ms > stamp_of(missed, peer)
}

/// The order a heartbeat round asks `members` in, given when each last answered this Mac, when a
/// round's dial of it last missed, and when one missed while it was marked missed and heard from
/// since (`stamps`: [`read_answered`], [`read_missed`], [`read_missed_heard`]): first the one
/// that answered last, which keeps a link that holds; then the rest, the one the rounds asked
/// longest ago (never, first) first, so a round reads books it has not read and every member in
/// turn hears this Mac's registration; and behind them each marked missed
/// ([`missed_since_answered`]), so members gone dark never hold the round's first places. Of
/// those, each heard from since its last miss ([`heard_since_missed`]) goes first, unless that
/// miss too came while it was heard from, then the rest, each part the one missed longest ago
/// first, so one that misses again goes behind the rest of its part, and a member back from a
/// cut is not left behind members gone dark for good. Being heard from clears no mark: it shows
/// the member reaches some Mac, not that this Mac's dial reaches it, which only its answer
/// shows. And it puts a member first once per miss nobody heard it through: one heard from every
/// round that this Mac's dial never reaches (a path that works one way only) takes the first
/// place once, then waits its turn by its misses, behind members back from a cut that nobody
/// heard from.
fn round_order<'a>(
    mut members: Vec<&'a Peer>,
    (answered, missed, missed_heard): (&Stamps, &Stamps, &Stamps),
) -> Vec<&'a Peer> {
    let stamp = stamp_of;
    let dark = |peer: &Peer| missed_since_answered(peer, (answered, missed));
    let heard_first = |peer: &Peer| {
        dark(peer)
            && heard_since_missed(peer, missed)
            && stamp(missed_heard, peer) < stamp(missed, peer)
    };
    members.sort_by_key(|peer| {
        (
            dark(peer),
            !heard_first(peer),
            stamp(answered, peer).max(stamp(missed, peer)),
            std::cmp::Reverse(peer.seen_ms),
        )
    });
    let lead = members
        .iter()
        .enumerate()
        .filter(|(_, peer)| !dark(peer))
        .filter_map(|(at, peer)| stamp(answered, peer).map(|ms| (std::cmp::Reverse(ms), at)))
        .min()
        .map(|(_, at)| at);
    if let Some(at) = lead {
        members[..=at].rotate_right(1);
    }
    members
}

/// The registration `reg` with the member `node` that `reached` proved, by `register_until`, then
/// a read of its book by `deadline` (a member too old to share its book gives none): `None` when
/// it recorded nothing, or named another node than the one it proved.
fn beat(
    mut reached: Reached,
    node: &str,
    (secret, me): (&FleetSecret, &NodeIdentity),
    reg: &Registration,
    (register_until, deadline): (Instant, (Instant, Duration)),
    current: &AtomicBool,
) -> Option<(Answered, Option<Book>, Option<NodeKey>)> {
    if !current.load(Ordering::Acquire) {
        return None;
    }
    let at = reached.addr;
    let key = reached.peer.as_ref().map(|proof| proof.key);
    let joined = register_by(&mut reached, reg, register_until).ok()?;
    drop(reached);
    if joined.issuer.node != node {
        return None;
    }
    let mut fetched = None;
    if current.load(Ordering::Acquire) && Instant::now() < deadline.0 {
        let target = Target {
            host: at.ip().to_canonical().to_string(),
            addr: Some(at.ip().to_canonical()),
            port: at.port(),
            node: Some(node.to_owned()),
            key,
        };
        if let Ok(book) = fetch_book(&target, (secret, Some(me)), deadline)
            && book.proven
        {
            fetched = Some(book);
        }
    }
    Some((
        Answered {
            node: node.to_owned(),
            at,
            joined,
            merged: 0,
        },
        fetched,
        key,
    ))
}

/// How an aterm before 0.147 that holds no join code refuses a `book` read, in the reading
/// machine's words. Every member shares its book now ([`serve_book`]).
pub const NO_BOOK: &str = "it is no fleet's issuer: it holds no join code, so it keeps no book of \
                           the fleet's members to share";

/// The word of a book read's `ERR` from an aterm before 0.147 that holds no join code: it has no
/// book to share, so the member's copy goes when it is the issuer that says so ([`read_whole`]).
pub const BOOK_NONE: &str = "no-book";

/// The word of a book read's `ERR` from a member that cannot share its book this time (it
/// cannot read it, or cannot name itself): the reader keeps what it holds.
pub const BOOK_NOT_NOW: &str = "not-now";

/// A member's end of a book read, on a stream proved for op `book`: answered unasked, its peer
/// lines, then a trailer with the fleet seed it holds ([`book_reply`]). Returns how many peer
/// lines it sent.
pub fn serve_book<S: Write>(
    stream: &mut S,
    state_dir: &Path,
    node: impl FnOnce() -> Result<String, String>,
) -> io::Result<Result<usize, String>> {
    serve_book_at(stream, state_dir, node, None, (&[], None))
}

/// [`serve_book`], omitting peers whose node id is on `denied_nodes` or (when `now_ms` is given)
/// whose `joined` entry has expired (`>= 90` days silent), and putting `own`, this member's own
/// signed entry ([`Peer::own`]), first. Any member serves it
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1, F17): its first-hand entries, the member it
/// joined through among them, each as an entry it recorded.
pub fn serve_book_at<S: Write>(
    stream: &mut S,
    state_dir: &Path,
    node: impl FnOnce() -> Result<String, String>,
    now_ms: Option<u64>,
    (denied_nodes, own): (&[String], Option<&Peer>),
) -> io::Result<Result<usize, String>> {
    serve_book_with_log(stream, state_dir, node, now_ms, (denied_nodes, None, own))
}

/// [`serve_book_at`], also omitting peers whose node key `log` holds out.
pub fn serve_book_with_log<S: Write>(
    stream: &mut S,
    state_dir: &Path,
    node: impl FnOnce() -> Result<String, String>,
    now_ms: Option<u64>,
    (denied_nodes, log, own): (&[String], Option<&MemberKeys>, Option<&Peer>),
) -> io::Result<Result<usize, String>> {
    let node = node().and_then(|node| {
        if is_node_id(&node) {
            Ok(node)
        } else {
            Err("its node id is no node id".to_owned())
        }
    });
    let shared = match &node {
        Err(why) => Err((
            BOOK_NOT_NOW,
            format!("the member cannot name itself: {why}"),
        )),
        Ok(_) => read_peers(state_dir).map_err(|e| {
            (
                BOOK_NOT_NOW,
                format!("the member cannot read its book: {e}"),
            )
        }),
    };
    let named = node
        .as_ref()
        .map_or_else(|_| String::new(), |node| format!("node={node} "));
    let (answer, outcome) = match shared {
        Ok(book) => {
            let own = own.filter(|own| {
                node.as_ref().ok() == own.node.as_ref()
                    && !own
                        .node
                        .as_deref()
                        .is_some_and(|n| crate::fleet::is_denied_node(n, denied_nodes))
                    && !own.key.is_some_and(|key| {
                        crate::fleet::is_denied_key(&key, denied_nodes)
                            || log.is_some_and(|log| log.holds_out(&key))
                    })
            });
            let shared: Vec<Peer> =
                own.cloned()
                    .into_iter()
                    .chain(
                        book.into_iter()
                            .filter(|peer| {
                                (peer.via.first_hand() || peer.via == Via::Issuer)
                                    && peer.node.as_ref() != node.as_ref().ok()
                                    && !peer.node.as_deref().is_some_and(|n| {
                                        crate::fleet::is_denied_node(n, denied_nodes)
                                    })
                                    && !peer.key.is_some_and(|key| {
                                        crate::fleet::is_denied_key(&key, denied_nodes)
                                            || log.is_some_and(|log| log.holds_out(&key))
                                    })
                                    && !now_ms.is_some_and(|now| peer.is_expired_at(now))
                            })
                            .map(|peer| Peer {
                                via: if peer.via == Via::Issuer {
                                    Via::Joined
                                } else {
                                    peer.via
                                },
                                fleet: None,
                                ..peer
                            }),
                    )
                    .take(PEERS_MAX)
                    .collect();
            // The seed this member holds rides every book reply's trailer, as its join replies
            // carry it: a reader adopts it once its own book binds the minter's key (§3.3).
            let trailer: Vec<String> = fleet_seed::read(state_dir)
                .ok()
                .flatten()
                .map(|seed| seed.words())
                .into_iter()
                .collect();
            let (answer, served) =
                book_reply(named.trim_end(), shared.iter().map(Peer::line), &trailer);
            (answer, Ok(served))
        }
        Err((word, why)) => {
            let why = aterm_codec::tty::one_line(&why, 400);
            (format!("ERR {word} {named}{why}\n"), Err(why))
        }
    };
    stream.write_all(answer.as_bytes())?;
    stream.flush()?;
    Ok(outcome)
}

/// A book reply naming `named` (`node=<node>`): its head, the lines of `peers` in order while the
/// next leaves [`TRAILER_MAX`] of [`REPLY_MAX`] free, then `trailer`, its first lines while they
/// fit [`TRAILER_MAX`], at most [`TRAILER_LINES_MAX`], counted in the head as `more=<k>` when
/// there is one; with how many peer lines it holds. The head's first count is the peer lines', so a reader before the trailer
/// reads those, returns, and never reads the trailer.
fn book_reply(
    named: &str,
    peers: impl Iterator<Item = String>,
    trailer: &[String],
) -> (String, usize) {
    let mut lines = String::new();
    let mut served = 0;
    for line in peers {
        if BOOK_HEAD_MAX + lines.len() + line.len() + 1 + TRAILER_MAX > REPLY_MAX {
            break;
        }
        lines.push_str(&line);
        lines.push('\n');
        served += 1;
    }
    let mut kept = 0;
    let mut bytes = 0;
    for line in trailer.iter().take(TRAILER_LINES_MAX) {
        if bytes + line.len() + 1 > TRAILER_MAX {
            break;
        }
        bytes += line.len() + 1;
        kept += 1;
    }
    let trailer = &trailer[..kept];
    let more = if trailer.is_empty() {
        String::new()
    } else {
        format!(" more={}", trailer.len())
    };
    let mut answer = format!("OK {served} {named}{more}\n");
    answer.push_str(&lines);
    for line in trailer {
        answer.push_str(line);
        answer.push('\n');
    }
    (answer, served)
}

/// Put each entry of `fetched`, a member's book as this Mac read it at `now_ms` by its own clock,
/// into `book`: one its own node signed ([`Peer::signature_holds`]) under a key `log` did not take
/// out, not this Mac's own (`own`) nor a node `denied_nodes` names, as an entry this Mac holds
/// first-hand ([`record`]: of two of one node, the later self-seq stands). With a log or with
/// none (`None`), every Mac takes in each node a member's book names, so a Mac that joined at any
/// member reaches every book, member to member, with no Mac that gathers them
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.1): no issuer's book is the fleet's roster, and no
/// Mac's `peers rm` takes a Mac out of any other's. The member's book is
/// believed as an issuer's was, never over a binding of this Mac's: [`record`] binds no node
/// `book` holds under another key, and none that `copies`, this Mac's copy of its issuer's book,
/// binds so ([`copy_rebinds`]), but where the log took that key out. Neither `addr` nor when
/// the entry was last heard is covered by the signature: a merged entry carries no `addr` of
/// the serving book's (an existing entry here keeps the address it was reached at or registered
/// from), so a secret holder cannot name others' entries at one address to crowd its
/// [`JOINED_PER_ADDRESS`] allowance or move a held entry, and `seen_ms` is never later than
/// `now_ms`, so no entry ages this Mac's out. Returns how many changed `book`.
pub fn merge_signed(
    book: &mut Vec<Peer>,
    fetched: &[Peer],
    (own, now_ms): (Option<&str>, u64),
    (log, denied_nodes, copies): (Option<&MemberKeys>, &[String], &Keyring),
) -> usize {
    let mut merged = 0;
    for entry in fetched {
        let admitted = entry.key.as_ref().is_some_and(|key| {
            !log.is_some_and(|log| log.holds_out(key))
                && !crate::fleet::is_denied_key(key, denied_nodes)
        });
        if !admitted
            || !entry.signature_holds()
            || entry.node.is_none()
            || entry.node.as_deref() == own
        {
            continue;
        }
        let peer = Peer {
            addr: None,
            via: Via::Joined,
            fleet: None,
            key_from: None,
            seen_ms: entry.seen_ms.map(|seen| seen.min(now_ms)),
            ..entry.clone()
        };
        if copy_rebinds(copies, &peer, log, denied_nodes).is_err()
            || unbind_replaced(book, &peer, log, denied_nodes).is_err()
        {
            continue;
        }
        if matches!(
            record_with_denied(book, peer, denied_nodes),
            Ok(Recorded::New | Recorded::Updated)
        ) {
            merged += 1;
        }
    }
    merged
}

/// The issuer's book as a member read it ([`fetch_book`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Book {
    /// The node that answered: the issuer's, checked against the member's record of it.
    pub node: String,
    /// Its first-hand entries as it sent them; a line that is no such entry (a later aterm's,
    /// say) is passed over.
    pub peers: Vec<Peer>,
    /// The address the issuer answered at: an entry at a loopback address is on the issuer's
    /// machine ([`copies_of`]).
    pub at: IpAddr,
    /// Whether the issuer proved the key the member binds it to: only then are the keys its
    /// book binds copied ([`copies_of`]).
    pub proven: bool,
    /// The fleet seed its reply's trailer carried (a trailer with two carries none), as sent: a
    /// reader adopts it only once it verifies under the key the reader's own book binds to its
    /// minter and the mark of the secret the reader holds ([`fleet_seed::judge`]).
    pub seed: Option<fleet_seed::FleetSeed>,
}

/// Why a member's read of its issuer's book brought no book.
#[derive(Debug)]
pub enum BookUnread {
    /// A machine that proved the fleet's secret here serves no `book`: an older aterm (`NO op`).
    Older(SocketAddr),
    /// The machine at `at` holds no join code (`ERR no-book`): no issuer, no book.
    NoBook {
        /// Where it answered.
        at: SocketAddr,
        /// The node it named.
        node: Option<String>,
        /// Its words.
        why: String,
    },
    /// Any other `ERR` (`not-now`, or a later aterm's): no book this time.
    NotNow {
        /// Where it answered.
        at: SocketAddr,
        /// The node it named.
        node: Option<String>,
        /// Its words.
        why: String,
    },
    /// The address answered as another node than the issuer it was dialled as.
    OtherNode {
        /// Where it answered.
        at: SocketAddr,
        /// The node it answered as (its proof, or its book's `node=`).
        said: String,
        /// The issuer's node, which `said` is not.
        want: String,
    },
    /// Not reached, or its answer was no book.
    Unreached(Unreached),
}

impl fmt::Display for BookUnread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Older(at) => write!(
                f,
                "the issuer at {at} runs an older aterm, which shares no book"
            ),
            Self::NoBook { at, why, .. } => write!(f, "the machine at {at} shares no book: {why}"),
            Self::NotNow { at, why, .. } => {
                write!(f, "the issuer at {at} cannot share its book now: {why}")
            }
            // By a head the issuer's own id does not share: a node that chose an id beginning
            // as the issuer's reads as another.
            Self::OtherNode { at, said, want } => write!(
                f,
                "{at} answered as node {}, not this Mac's issuer",
                node_head(said, &[want])
            ),
            Self::Unreached(unreached) => write!(f, "{unreached}"),
        }
    }
}

/// The member's end of a book read: dial `issuer` for op `book` by `deadline` (and the
/// `within` it was set from, which a deadline's reason names), proving `me`, then read its
/// answer under that deadline and the socket-shutting watchdog a survey's reply is read under.
pub fn fetch_book(
    issuer: &Target,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    deadline: (Instant, Duration),
) -> Result<Book, BookUnread> {
    fetch_book_in(issuer, (secret, me), deadline, (None, None))
}

/// [`fetch_book`] as one dial of a read ([`HeldAddrs`]): it reads no other dial's exchange,
/// and makes its own at the issuer's recorded address in the one the read opened there first
/// (`first`), at any other address once no other dial of the read has one open there.
fn fetch_book_in<'a>(
    issuer: &Target,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    (deadline, within): (Instant, Duration),
    held: (Option<&'a HeldAddrs>, Option<(SocketAddr, Opened<'a>)>),
) -> Result<Book, BookUnread> {
    fetch_book_with(issuer, (secret, me), (deadline, within), held, resolve)
}

fn fetch_book_with<'a>(
    issuer: &Target,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    (deadline, within): (Instant, Duration),
    (held, mut first): (Option<&'a HeldAddrs>, Option<(SocketAddr, Opened<'a>)>),
    resolver: impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
) -> Result<Book, BookUnread> {
    let keys = Keyring {
        issuer: issuer.node.clone(),
        ..Keyring::default()
    };
    walk_target(issuer, &keys, (deadline, within), resolver, |to| {
        // Keep the address until read_book_reply has dropped its stream, including a
        // legacy reply whose header names another node and leaves this walk looking.
        let mine = match first.take() {
            Some((at, opened)) if at == to => Some(opened),
            _ => held.and_then(|held| held.enter_after(to, deadline)),
        };
        let reached = reach(
            to,
            (secret, me),
            FleetOp::Book,
            (deadline, within),
            &|_, _| None,
        )
        .and_then(|reached| {
            // Judge this address before keeping its exchange: a different member at a
            // stale address leaves the issuer's other addresses available. An unbound
            // older issuer still supplies the same unproved book it did before.
            answered_as(issuer, &keys, to, reached.peer.as_ref())
                .map(|answered| (reached, answered.is_some_and(|(_, proven)| proven)))
        })
        .and_then(|(reached, proven)| {
            match read_book_reply(reached, issuer, proven, (deadline, within)) {
                // An older peer proves no node in its handshake: its header supplies
                // the identity that this same target walk must judge before ending.
                Err(BookUnread::OtherNode { at, said, want }) => {
                    Err(Unreached::OtherNode { at, said, want })
                }
                result => Ok(result),
            }
        });
        if let Some(mut mine) = mine {
            match &reached {
                AtAddr::Passed(why @ Unreached::Connect(..)) => mine.close(Exchanged::Failed {
                    passed: true,
                    why: why.copied(),
                }),
                AtAddr::Reached(_) | AtAddr::Passed(_) | AtAddr::Ended(_) | AtAddr::Refused(_) => {
                    mine.close(Exchanged::Unasked);
                }
            }
        }
        reached
    })
    .map_err(|unreached| match unreached {
        Unreached::Handshake(at, FleetError::UnknownOp(_)) => BookUnread::Older(at),
        Unreached::OtherNode { at, said, want } => BookUnread::OtherNode { at, said, want },
        other => BookUnread::Unreached(other),
    })?
}

/// Read this address's Book response, retaining no connection after its result. Its
/// identity error is handled by the target walk; other Book errors end the read there.
fn read_book_reply(
    mut reached: Reached,
    issuer: &Target,
    proven: bool,
    (deadline, within): (Instant, Duration),
) -> Result<Book, BookUnread> {
    let at = reached.addr;
    let (head, lines) =
        ask_lines(&mut reached.transport, None, deadline, book_head).map_err(|why| {
            BookUnread::Unreached(match why {
                Asked::Deadline => Unreached::Deadline(Stage::Book, within),
                Asked::Failed(why) => Unreached::Reply(at, why),
                Asked::Local(why) => Unreached::Local(why),
            })
        })?;
    if let Some(rest) = head.strip_prefix("ERR ") {
        let (word, rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let (node, why) = match rest.split_once(' ') {
            Some((field, why)) => match field.strip_prefix("node=").filter(|n| is_node_id(n)) {
                Some(node) => (Some(node.to_owned()), why),
                None => (None, rest),
            },
            None => (None, rest),
        };
        if let Some(said) = node.as_ref()
            && let Some(want) = issuer.node.as_ref().filter(|want| *want != said)
        {
            return Err(BookUnread::OtherNode {
                at,
                said: said.clone(),
                want: want.clone(),
            });
        }
        let why = aterm_codec::tty::one_line(why, 400);
        return Err(if word == BOOK_NONE {
            BookUnread::NoBook { at, node, why }
        } else {
            BookUnread::NotNow { at, node, why }
        });
    }
    let node = head
        .split(' ')
        .find_map(|field| field.strip_prefix("node="))
        .filter(|node| is_node_id(node))
        .map(str::to_owned)
        .ok_or_else(|| {
            BookUnread::Unreached(Unreached::Reply(
                at,
                "answered a book naming no node".into(),
            ))
        })?;
    if let Some(want) = issuer.node.as_ref().filter(|want| **want != node) {
        return Err(BookUnread::OtherNode {
            at,
            said: node,
            want: want.clone(),
        });
    }
    let (n, _) =
        book_counts(&head).map_err(|why| BookUnread::Unreached(Unreached::Reply(at, why)))?;
    let (peers, trailer) = lines.split_at(n.min(lines.len()));
    let peers = peers
        .iter()
        .filter_map(|line| Peer::parse(line))
        .filter(|peer| peer.via.first_hand())
        .collect();
    // A trailer line a later aterm adds (a reservation, a tombstone) is passed over.
    let mut seeds = trailer
        .iter()
        .filter_map(|line| fleet_seed::FleetSeed::of_line(line));
    let seed = match (seeds.next(), seeds.next()) {
        (Some(seed), None) => Some(seed),
        _ => None,
    };
    Ok(Book {
        node,
        peers,
        at: at.ip().to_canonical(),
        proven,
        seed,
    })
}

/// How many lines a book reply's head promises: its peer lines and its trailer's
/// ([`book_counts`]), none after an `ERR`.
fn book_head(line: &str) -> Result<usize, String> {
    if line.starts_with("ERR ") {
        return Ok(0);
    }
    book_counts(line).map(|(n, more)| n + more)
}

/// A book reply's `OK <n> … [more=<k>]` head: `n` peer lines, at most [`PEERS_MAX`], then `k`
/// trailer lines, none without `more=`. A reader takes up to [`TRAILER_READ_MAX`] of them, more
/// than this aterm sends ([`TRAILER_LINES_MAX`]), so a later aterm's longer trailer, which
/// [`REPLY_MAX`] bounds whatever it holds, costs this reader only the lines it passes over.
fn book_counts(line: &str) -> Result<(usize, usize), String> {
    let shown = aterm_codec::tty::one_line(line, 120);
    let count = |n: &str| {
        (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .then(|| n.parse::<usize>().ok())
            .flatten()
    };
    let mut words = line.strip_prefix("OK ").unwrap_or_default().split(' ');
    let n = match words.next().and_then(count) {
        Some(n) if n <= PEERS_MAX => n,
        Some(n) => {
            return Err(format!(
                "promised {n} entries, past the {PEERS_MAX} a book holds: refused"
            ));
        }
        None => return Err(format!("answered no book: {shown}")),
    };
    let mut more = None;
    for word in words {
        if let Some(k) = word.strip_prefix("more=") {
            match count(k) {
                Some(k) if k <= TRAILER_READ_MAX && more.replace(k).is_none() => {}
                _ => return Err(format!("promised a trailer it cannot carry: {shown}")),
            }
        }
    }
    Ok((n, more.unwrap_or(0)))
}

fn names_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn copy_of(entry: &Peer, issuer_at: IpAddr) -> Peer {
    let mut copy = Peer {
        via: Via::IssuerBook,
        key_from: None,
        // Marks are what this machine's own joins recorded, never a copy's.
        fleet: None,
        ..entry.clone()
    };
    if issuer_at.is_loopback() {
        return copy;
    }
    let named_local = names_loopback(&copy.host);
    if copy.addr.is_some_and(|addr| addr.is_loopback()) || (copy.addr.is_none() && named_local) {
        copy.addr = Some(issuer_at);
    }
    if named_local {
        copy.host = issuer_at.to_string();
    }
    copy
}

/// The member's copy of `fetched`, its issuer's book: each first-hand entry in its order as a
/// copy, except this machine (`own`, its node id), the issuer itself, a node an earlier copy
/// holds, and a copy naming no node at a place one holds; at most [`PEERS_MAX`]. Keys are
/// copied only from a book whose issuer proved its key ([`Book::proven`]).
#[must_use]
pub fn copies_of(fetched: &Book, own: Option<&str>) -> Vec<Peer> {
    let mut copies = Vec::new();
    with_copies(
        &mut copies,
        fetched
            .peers
            .iter()
            .filter(|peer| peer.via.first_hand())
            .map(|entry| copy_of(entry, fetched.at))
            .map(|copy| Peer {
                key: copy.key.filter(|_| fetched.proven),
                ..copy
            })
            .filter(|copy| {
                copy.node
                    .as_deref()
                    .is_none_or(|node| Some(node) != own && node != fetched.node)
            }),
    );
    copies
}

/// `book` with `copies` (of its issuer's book) after its own entries, as a read reads them:
/// a copy that an entry of `book` or an earlier copy stands for (the same node, or the same
/// place for a copy with no node) is left out, though its key still binds its node
/// ([`Keyring::of`] of both), and `book` never grows past [`PEERS_MAX`].
pub fn with_copies(book: &mut Vec<Peer>, copies: impl IntoIterator<Item = Peer>) {
    for copy in copies {
        if book.len() >= PEERS_MAX {
            break;
        }
        if !book.iter().any(|known| stands_for(known, &copy)) {
            book.push(copy);
        }
    }
}

fn stands_for(known: &Peer, copy: &Peer) -> bool {
    match copy.node {
        Some(_) => known.node == copy.node,
        None => known.same_place(copy),
    }
}

/// A member's `book` with its copy of its issuer's book replaced whole by `fetched`: every
/// [`Via::IssuerBook`] entry dropped, then [`copies_of`] `fetched` added ([`with_copies`]).
pub fn merge_book(book: &mut Vec<Peer>, fetched: &Book, own: Option<&str>) {
    book.retain(|peer| peer.via != Via::IssuerBook);
    with_copies(book, copies_of(fetched, own));
}

/// The longest one connect may take: a LAN host answers a SYN in milliseconds, so a recorded
/// address gone dark leaves the name (and `<host>.local`) the rest of the deadline.
pub const CONNECT_WITHIN: Duration = Duration::from_secs(1);

/// The most peers a fleet read asks at once (`peers`, `survey --fleet`): each holds a socket
/// while it is asked, so a reader under a Dock-launched app's 256 descriptors keeps most of
/// them.
pub const AT_ONCE: usize = 32;

/// A peer reached and proved: the stream, ready for the op's request.
pub struct Reached {
    /// The proved stream, its socket deadlines cleared.
    pub transport: TlsTransport<ClientConnection>,
    /// The level the peer proved it serves.
    pub scope: FleetScope,
    /// The address it answered at.
    pub addr: SocketAddr,
    /// The node it proved it holds the key of, if any: whether that makes it the node is the
    /// book's to say ([`Keyring::proven`]).
    pub peer: Option<NodeProof>,
    /// It answered on [`crate::fleet::ALPN_MESH`]: this Mac's membership log seats a member under
    /// the key it proved.
    pub mesh: bool,
}

/// One exchange at an address per read ([`read_whole`], [`survey_all`]): a listener serves an
/// address only [`crate::fleet::per_source`] proved connections at once, sized for one per
/// reader, so the read's first dial to an address makes the one connection there, and every
/// other dial that reaches it while it is open reads that exchange as its own, judging the
/// node proved there by its own target and keys.
#[derive(Default)]
struct HeldAddrs {
    open: std::sync::Mutex<Vec<(SocketAddr, std::sync::Arc<Exchange>)>>,
}

/// One exchange of a read at an address ([`HeldAddrs`]): how far it has come, and the wake of
/// the dials that read it.
#[derive(Default)]
struct Exchange {
    now: std::sync::Mutex<Exchanged>,
    changed: std::sync::Condvar,
}

/// How far an [`Exchange`] has come.
#[derive(Default)]
enum Exchanged {
    /// Its dial is connecting there, or proving.
    #[default]
    Dialing,
    /// Failed before any proof: passed over (`passed`, the next address tried) or the dial's end.
    Failed { passed: bool, why: Unreached },
    /// The handshake refused the operation after proving its node.
    Refused(ProvedRefusal),
    /// Reached and proved: the level it serves, and the node it proved.
    Proved(FleetScope, Option<NodeProof>),
    /// Its request's answer, beside what it proved.
    Answered(FleetScope, Option<NodeProof>, Result<Vec<String>, Asked>),
    /// Closed with nothing asked (its dial judged the node there not its target's, or ended
    /// there): each reader makes its own.
    Unasked,
}

/// What one address of a read's dial came to ([`HeldAddrs::ask`]): what it proved, the dial's
/// judgement of that node ([`answered_as`]) and the request's answer.
struct Answer {
    scope: FleetScope,
    addr: SocketAddr,
    peer: Option<NodeProof>,
    answered: Option<(String, bool)>,
    lines: Result<Vec<String>, Asked>,
}

/// What a dial that reads another's [`Exchange`] has seen of it.
enum Seen {
    /// It failed there before any proof, for this reason ([`Exchanged::Failed`]).
    Failed(bool, Unreached),
    /// The refusing node's proof is judged separately by each reader.
    Refused(ProvedRefusal),
    /// It proved this level and node there.
    Proved(FleetScope, Option<NodeProof>),
    /// It is closed, nothing asked: this dial makes its own.
    Gone,
}

impl HeldAddrs {
    /// This dial's own exchange at `to`, else the one another dial of the read has open there.
    fn enter(&self, to: SocketAddr) -> Result<Opened<'_>, std::sync::Arc<Exchange>> {
        let mut open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, theirs)) = open.iter().find(|(at, _)| *at == to) {
            return Err(std::sync::Arc::clone(theirs));
        }
        let mine = std::sync::Arc::new(Exchange::default());
        open.push((to, std::sync::Arc::clone(&mine)));
        Ok(Opened {
            of: self,
            exchange: mine,
            closed: false,
        })
    }

    /// This dial's own exchange at `to` once no other dial of the read has one open there, or
    /// `None` when `by` comes first.
    fn enter_after(&self, to: SocketAddr, by: Instant) -> Option<Opened<'_>> {
        loop {
            match self.enter(to) {
                Ok(opened) => return Some(opened),
                Err(theirs) => theirs.read(by, |now| match now {
                    Exchanged::Dialing | Exchanged::Proved(..) => None,
                    _ => Some(()),
                })?,
            }
        }
    }

    /// `to`'s answer for one dial of the read by `deadline`: this dial's own exchange there
    /// (`dial`, `judge`, then `ask`) when none is open, else the open one read as its own, its
    /// node judged by this dial's `judge`.
    fn ask(
        &self,
        to: SocketAddr,
        (deadline, within): (Instant, Duration),
        dial: impl Fn() -> AtAddr<Reached>,
        judge: impl Fn(Option<&NodeProof>) -> Result<Option<(String, bool)>, Unreached>,
        ask: impl Fn(&mut TlsTransport<ClientConnection>) -> Result<Vec<String>, Asked>,
    ) -> AtAddr<Answer> {
        loop {
            let theirs = match self.enter(to) {
                Ok(opened) => return opened.ask(to, dial(), &judge, &ask),
                Err(theirs) => theirs,
            };
            let seen = theirs.read(deadline, |now| match now {
                Exchanged::Dialing => None,
                Exchanged::Failed { passed, why } => Some(Seen::Failed(*passed, why.copied())),
                Exchanged::Refused(refused) => Some(Seen::Refused(refused.copied())),
                Exchanged::Proved(scope, peer) | Exchanged::Answered(scope, peer, _) => {
                    Some(Seen::Proved(*scope, peer.clone()))
                }
                Exchanged::Unasked => Some(Seen::Gone),
            });
            let (scope, peer) = match seen {
                None => return AtAddr::Ended(Unreached::Deadline(Stage::Connect, within)),
                Some(Seen::Gone) => continue,
                // Its deadline came there, and this dial's has not. One passed over (a stalled
                // handshake, with the deadline still to come) passes this dial on as well, below:
                // another try at a silent address would spend what its next address needs.
                Some(Seen::Failed(false, Unreached::Deadline(..))) if Instant::now() < deadline => {
                    continue;
                }
                Some(Seen::Failed(true, why)) => return AtAddr::Passed(why),
                Some(Seen::Failed(false, why)) => return AtAddr::Ended(why),
                Some(Seen::Refused(refused)) => return AtAddr::Refused(refused),
                Some(Seen::Proved(scope, peer)) => (scope, peer),
            };
            let answered = match judge(peer.as_ref()) {
                Ok(answered) => answered,
                Err(why) => return AtAddr::Ended(why),
            };
            let lines = theirs.read(deadline, |now| match now {
                Exchanged::Answered(_, _, lines) => Some(Some(lines.clone())),
                Exchanged::Unasked => Some(None),
                _ => None,
            });
            let lines = match lines {
                None => Err(Asked::Deadline),
                // It asked nothing, or its deadline cut the answer short and this dial's has not.
                Some(None) => continue,
                Some(Some(Err(Asked::Deadline))) if Instant::now() < deadline => continue,
                Some(Some(lines)) => lines,
            };
            return AtAddr::Reached(Answer {
                scope,
                addr: to,
                peer,
                answered,
                lines,
            });
        }
    }
}

impl Exchange {
    /// What `seen` makes of this exchange once it says something, or `None` when `by` comes first.
    fn read<T>(&self, by: Instant, seen: impl Fn(&Exchanged) -> Option<T>) -> Option<T> {
        let mut now = self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(seen) = seen(&now) {
                return Some(seen);
            }
            let left = by.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            now = self
                .changed
                .wait_timeout(now, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Tell every dial that reads it how far it has come.
    fn tell(&self, now: Exchanged) {
        *self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = now;
        self.changed.notify_all();
    }
}

/// A dial's own exchange at an address ([`HeldAddrs`]): closed once its stream is gone, as
/// [`Exchanged::Unasked`] when it drops unclosed.
struct Opened<'a> {
    of: &'a HeldAddrs,
    exchange: std::sync::Arc<Exchange>,
    closed: bool,
}

impl Opened<'_> {
    /// The exchange at `to`: `reached`, its node judged by `judge`, and only then `ask`.
    fn ask(
        mut self,
        to: SocketAddr,
        reached: AtAddr<Reached>,
        judge: impl Fn(Option<&NodeProof>) -> Result<Option<(String, bool)>, Unreached>,
        ask: impl Fn(&mut TlsTransport<ClientConnection>) -> Result<Vec<String>, Asked>,
    ) -> AtAddr<Answer> {
        let mut reached = match reached {
            AtAddr::Reached(reached) => reached,
            AtAddr::Passed(why) => {
                self.close(Exchanged::Failed {
                    passed: true,
                    why: why.copied(),
                });
                return AtAddr::Passed(why);
            }
            AtAddr::Ended(why) => {
                self.close(Exchanged::Failed {
                    passed: false,
                    why: why.copied(),
                });
                return AtAddr::Ended(why);
            }
            AtAddr::Refused(refused) => {
                self.close(Exchanged::Refused(refused.copied()));
                return AtAddr::Refused(refused);
            }
        };
        let (scope, peer) = (reached.scope, reached.peer.clone());
        self.exchange.tell(Exchanged::Proved(scope, peer.clone()));
        let answered = match judge(peer.as_ref()) {
            Ok(answered) => answered,
            Err(why) => {
                drop(reached);
                self.close(Exchanged::Unasked);
                return AtAddr::Ended(why);
            }
        };
        let lines = ask(&mut reached.transport);
        // The stream is gone before another dial of the read may open one.
        drop(reached);
        self.close(Exchanged::Answered(scope, peer.clone(), lines.clone()));
        AtAddr::Reached(Answer {
            scope,
            addr: to,
            peer,
            answered,
            lines,
        })
    }

    /// Out of the read's open exchanges, then `now` told to its readers.
    fn close(&mut self, now: Exchanged) {
        self.closed = true;
        self.of
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(_, open)| !std::sync::Arc::ptr_eq(open, &self.exchange));
        self.exchange.tell(now);
    }
}

impl Drop for Opened<'_> {
    fn drop(&mut self) {
        if !self.closed {
            self.close(Exchanged::Unasked);
        }
    }
}

/// What a lookup that ran out says of its host ([`Unreached::Lookup`]): nothing answered for it.
/// A caller that knows more (a browse that just heard the name, as `aterm members join`'s does)
/// reads it to say why.
pub const LOOKUP_UNANSWERED: &str = "no DNS or multicast-DNS answer";

/// Why a peer was not reached, as a person can act on it. A survey or a drive reports a node
/// it judged wrong (`OtherNode`, `Impostor`, `Misnamed`, `Unproved`, `Unbound`) only once the
/// peer's host name did not answer as the target either.
#[derive(Debug)]
pub enum Unreached {
    /// Its host name resolves to no address.
    Resolve(String),
    /// No connection to this address.
    Connect(SocketAddr, io::Error),
    /// Connected; the handshake failed.
    Handshake(SocketAddr, FleetError),
    /// The one deadline came at this stage.
    Deadline(Stage, Duration),
    /// Its host name was still being looked up when the deadline came.
    Lookup(String, Duration),
    /// Proved; its answer to the request was none ([`survey_all`]).
    Reply(SocketAddr, String),
    /// The address is none a fleet listener serves a reader from
    /// ([`crate::fleet::is_fleet_source`]), so it was not dialled.
    NotFleet(SocketAddr),
    /// The address answered as another node than the book's (the one it proved, or its
    /// survey's `node=`): it passed to another member.
    OtherNode {
        /// The address that answered.
        at: SocketAddr,
        /// The node it answered as.
        said: String,
        /// The node the book holds there: what `aterm fleet peers rm` takes when that machine left.
        want: String,
    },
    /// The address claimed this node and proved another key than the one this machine binds
    /// it to ([`Keyring`]): not that node, so it was asked nothing.
    Impostor {
        /// The address that answered.
        at: SocketAddr,
        /// The node it claimed.
        node: String,
        /// Whether that node is this machine's issuer, whose old key is taken out here (its
        /// book never holds itself), never on the issuer.
        issuer: bool,
    },
    /// The address proved a node and its survey's `instance` header names another: its word
    /// contradicts its proof, so nothing it sent is read, neither its sessions nor which
    /// machine it is.
    Misnamed {
        /// The address that answered.
        at: SocketAddr,
        /// The node it proved.
        proved: String,
        /// The node its header named, made one printable line ([`aterm_codec::tty::one_line`]).
        claimed: String,
    },
    /// This machine binds the book's node a key, and the address proved no node (an older
    /// aterm, one with no node key, or a peer that will not say), so it is never read as that
    /// node.
    Unproved {
        /// The address that answered.
        at: SocketAddr,
        /// The node this machine binds a key to.
        node: String,
    },
    /// A drive ([`drive`]) reached a peer that proved no node this machine binds a key to (no
    /// proof, or a node bound to none): a read takes that as the peer's claim, a drive never.
    Unbound {
        /// The address that answered.
        at: SocketAddr,
        /// The node it claimed, when it named one.
        node: Option<String>,
    },
    /// This machine could not try.
    Local(String),
}

/// Where a peer's one deadline came ([`Unreached::Deadline`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Before any connection was made.
    Connect,
    /// Connected, and the fleet handshake unfinished: an aterm stopped or overloaded, or no
    /// aterm on that port.
    Handshake,
    /// Authentication completed, but the exchange did not finish before its deadline.
    Answer,
    /// Proved for its book, and the book not come: the listener answers a book itself, at once
    /// and unasked, so that aterm is stopped or overloaded.
    Book,
}

impl Unreached {
    /// The same failure again, for each dial of a read that meets the one exchange at an address
    /// ([`HeldAddrs`]).
    fn copied(&self) -> Self {
        match self {
            Self::Resolve(host) => Self::Resolve(host.clone()),
            Self::Connect(at, e) => Self::Connect(*at, crate::fleet::copied_io(e)),
            Self::Handshake(at, e) => Self::Handshake(*at, e.copied()),
            Self::Deadline(stage, within) => Self::Deadline(*stage, *within),
            Self::Lookup(host, within) => Self::Lookup(host.clone(), *within),
            Self::Reply(at, why) => Self::Reply(*at, why.clone()),
            Self::NotFleet(at) => Self::NotFleet(*at),
            Self::OtherNode { at, said, want } => Self::OtherNode {
                at: *at,
                said: said.clone(),
                want: want.clone(),
            },
            Self::Impostor { at, node, issuer } => Self::Impostor {
                at: *at,
                node: node.clone(),
                issuer: *issuer,
            },
            Self::Misnamed {
                at,
                proved,
                claimed,
            } => Self::Misnamed {
                at: *at,
                proved: proved.clone(),
                claimed: claimed.clone(),
            },
            Self::Unproved { at, node } => Self::Unproved {
                at: *at,
                node: node.clone(),
            },
            Self::Unbound { at, node } => Self::Unbound {
                at: *at,
                node: node.clone(),
            },
            Self::Local(why) => Self::Local(why.clone()),
        }
    }

    /// Whether it came before the peer proved itself (no name, no address, no connection, no
    /// handshake; a busy address aside), so a second dial of that peer within the same
    /// deadline would fail the same way.
    #[must_use]
    pub const fn before_proof(&self) -> bool {
        match self {
            Self::Handshake(_, FleetError::AddressBusy) => false,
            Self::Resolve(_)
            | Self::Connect(..)
            | Self::Handshake(..)
            | Self::Lookup(..)
            | Self::NotFleet(_)
            | Self::Local(_)
            | Self::Deadline(Stage::Connect | Stage::Handshake, _) => true,
            Self::Deadline(Stage::Answer | Stage::Book, _)
            | Self::Reply(..)
            | Self::OtherNode { .. }
            | Self::Impostor { .. }
            | Self::Misnamed { .. }
            | Self::Unproved { .. }
            | Self::Unbound { .. } => false,
        }
    }

    /// Whether the peer closed the connection during the handshake: what an aterm restarting
    /// its fleet listener (a change of `[fleet]` there) or stopping does to a dial in flight.
    #[must_use]
    pub fn hung_up(&self) -> bool {
        matches!(
            self,
            Self::Handshake(_, FleetError::Tls(e) | FleetError::Io(e)) if matches!(
                e.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::UnexpectedEof
            )
        )
    }

    /// Whether the listener took the connection and only could not serve this dial just then: it
    /// hung up mid-handshake ([`Self::hung_up`]), let the handshake's time pass, or was at a cap
    /// ([`FleetError::AddressBusy`]) or between reads of its deny list
    /// ([`FleetError::DenyListUnread`]). The same dial a moment later may be served ([`passes`]).
    #[must_use]
    pub fn served_later(&self) -> bool {
        self.hung_up()
            || matches!(
                self,
                Self::Handshake(_, FleetError::AddressBusy | FleetError::DenyListUnread)
                    | Self::Deadline(Stage::Handshake, _)
            )
    }

    /// Whether the failure matches an overloaded listener at its connection or per-address slot
    /// cap: `NO capacity` ([`FleetError::AddressBusy`]), an immediate mid-handshake drop when all
    /// listener slots are occupied ([`Self::hung_up`]), a TCP reset/timeout while connecting, a
    /// connect/handshake/reply deadline, or the local resolver permit cap.
    #[must_use]
    pub fn at_capacity(&self) -> bool {
        if self.hung_up() {
            return true;
        }
        match self {
            Self::Handshake(_, FleetError::AddressBusy) | Self::Deadline(..) => true,
            Self::Connect(_, e) => matches!(
                e.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::WouldBlock
            ),
            Self::Local(why) => why == "the fleet resolver is at capacity",
            Self::Resolve(_)
            | Self::Handshake(..)
            | Self::Lookup(..)
            | Self::Reply(..)
            | Self::NotFleet(_)
            | Self::OtherNode { .. }
            | Self::Impostor { .. }
            | Self::Misnamed { .. }
            | Self::Unproved { .. }
            | Self::Unbound { .. } => false,
        }
    }

    /// Whether the node's place said that node may drive this machine no more: it answered under
    /// another fleet's secret, refused this machine's, or proved that node under another key than
    /// this machine binds. Nothing else does: a node's drive is its own dial of this machine, so a
    /// place that does not answer, answers late or busy, or answers as another node says nothing
    /// of it.
    #[must_use]
    pub const fn disowns(&self) -> bool {
        matches!(
            self,
            Self::Handshake(
                _,
                FleetError::ListenerWrongSecret | FleetError::Refused | FleetError::DeniedNode
            ) | Self::Impostor { .. }
        )
    }

    /// Whether the connect failed with no route to the host (`EHOSTUNREACH`): a host off the LAN,
    /// asleep or off.
    #[must_use]
    pub fn no_route(&self) -> bool {
        matches!(
            self,
            Self::Connect(_, e) if e.kind() == io::ErrorKind::HostUnreachable
        )
    }

    /// Whether the connect failed with no route to the peer's network (`ENETUNREACH`): this
    /// machine has no interface on the LAN (offline, or a full-tunnel VPN holds the route),
    /// whatever the peer does.
    #[must_use]
    pub fn no_network(&self) -> bool {
        matches!(
            self,
            Self::Connect(_, e) if e.kind() == io::ErrorKind::NetworkUnreachable
        )
    }

    /// Whether a connect to the peer was tried: `false` for a name that did not resolve (or
    /// not in time), an address no fleet dials, and a failure of this machine's own before any
    /// connect, none of which says anything of this machine's route into the LAN.
    #[must_use]
    pub const fn dialled(&self) -> bool {
        !matches!(
            self,
            Self::Resolve(_) | Self::Lookup(..) | Self::NotFleet(_) | Self::Local(_)
        )
    }

    /// Whether the connect was refused to this process itself (`EPERM`, `EACCES`): a sandbox
    /// around it or a firewall, not the peer.
    #[must_use]
    pub fn not_permitted(&self) -> bool {
        matches!(
            self,
            Self::Connect(_, e) if e.kind() == io::ErrorKind::PermissionDenied
        )
    }

    /// The address it came at: `None` for a name that did not resolve (or not in time), a
    /// deadline, and a failure of this machine's own, which say nothing of where the peer is.
    #[must_use]
    pub const fn at(&self) -> Option<SocketAddr> {
        match self {
            Self::Connect(at, _)
            | Self::Handshake(at, _)
            | Self::Reply(at, _)
            | Self::NotFleet(at)
            | Self::OtherNode { at, .. }
            | Self::Impostor { at, .. }
            | Self::Misnamed { at, .. }
            | Self::Unproved { at, .. }
            | Self::Unbound { at, .. } => Some(*at),
            Self::Resolve(_) | Self::Deadline(..) | Self::Lookup(..) | Self::Local(_) => None,
        }
    }
}

impl fmt::Display for Unreached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolve(host) => write!(f, "{host} resolves to no address"),
            Self::Connect(addr, e) => match e.kind() {
                io::ErrorKind::ConnectionRefused => write!(
                    f,
                    "nothing listens on {addr}: no aterm running there, or no [fleet] listen"
                ),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                    write!(f, "{addr} did not answer in time")
                }
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => write!(
                    f,
                    "{addr} reset the connection: its fleet listener closing or restarting; try \
                     again"
                ),
                io::ErrorKind::HostUnreachable => {
                    write!(f, "no route to {}: off the LAN, asleep or off", addr.ip())
                }
                io::ErrorKind::NetworkUnreachable => write!(
                    f,
                    "this machine has no route to {addr}: offline, or a VPN holds the LAN's route"
                ),
                io::ErrorKind::PermissionDenied => f.write_str(
                    "a sandbox here (Claude Code's) or firewall refused: run outside it",
                ),
                _ => write!(f, "{addr}: {e}"),
            },
            Self::Handshake(addr, _) if self.hung_up() => write!(
                f,
                "{addr} hung up mid-handshake: listener busy, restarting, or not aterm; try again"
            ),
            Self::Handshake(addr, e) => match e {
                FleetError::ListenerWrongSecret => write!(
                    f,
                    "another secret at {addr}: the fleet rotated (join again with the new \
                     code) or it left"
                ),
                FleetError::Refused => write!(
                    f,
                    "{addr} refused this machine's proof: this machine holds another fleet's \
                     secret"
                ),
                FleetError::DeniedNode => write!(
                    f,
                    "{addr} refused this machine's node: it is on the swarm roster's deny-list"
                ),
                FleetError::DenyListUnread => write!(
                    f,
                    "{addr} has not read its deny list yet, so it admits no one: try again once \
                     it has"
                ),
                FleetError::Alpn => write!(f, "{addr} is a pre-fleet aterm listener (update it)"),
                FleetError::AddressBusy => write!(
                    f,
                    "{addr} is busy with other reads from this machine's address: try again when \
                     one ends"
                ),
                FleetError::UnknownOp(op) if op == FleetOp::Drive.word() => write!(
                    f,
                    "{addr} lets no one drive it: its person runs `aterm fleet allow drive \
                     --from <this Mac>` (or `--all`, every member) there, or it runs an older \
                     aterm"
                ),
                FleetError::UnknownOp(op) => write!(
                    f,
                    "{addr} does not serve `{op}`: an older aterm (update it)"
                ),
                FleetError::UnknownScope(scope) => write!(
                    f,
                    "{addr} serves a fleet level unknown here: update this aterm (`{}`)",
                    scope.chars().take(16).collect::<String>()
                ),
                FleetError::Tls(e) => write!(f, "{addr} is no fleet listener (TLS: {e})"),
                other => write!(f, "{addr}: {other}"),
            },
            Self::Deadline(stage, within) => {
                let within = within.as_secs_f64();
                match stage {
                    Stage::Connect => write!(f, "no connection within {within:.1}s"),
                    Stage::Handshake => write!(
                        f,
                        "connected, but no fleet handshake within {within:.1}s: an aterm \
                         stopped or overloaded, or no aterm there"
                    ),
                    Stage::Answer => write!(
                        f,
                        "authentication completed, but the exchange did not finish within \
                         {within:.1}s"
                    ),
                    Stage::Book => write!(
                        f,
                        "proved itself, but sent no book within {within:.1}s: an aterm stopped \
                         or overloaded"
                    ),
                }
            }
            Self::Lookup(host, within) => write!(
                f,
                "{host} did not resolve within {:.1}s ({LOOKUP_UNANSWERED})",
                within.as_secs_f64()
            ),
            Self::Reply(addr, why) => write!(f, "{addr} {why}"),
            Self::NotFleet(addr) => write!(
                f,
                "{} is no LAN address (loopback, private, link-local): not dialled, a fleet is \
                 one LAN",
                addr.ip()
            ),
            Self::OtherNode { at, said, want } => write!(
                f,
                "{} is node {} now: rejoin this peer, or `aterm fleet peers rm {}` if it left",
                at.ip(),
                node_head(said, &[want]),
                node_head(want, &[said])
            ),
            Self::Impostor {
                at,
                node,
                issuer: false,
            } => write!(
                f,
                "{} claims node {} with another key: not read (re-keyed? peers rm it on the \
                 issuer)",
                at.ip(),
                node.get(..8).unwrap_or(node)
            ),
            Self::Impostor {
                at,
                node,
                issuer: true,
            } => write!(
                f,
                "{} claims issuer {} with another key: not read (re-keyed? peers rm it here, \
                 rejoin)",
                at.ip(),
                node.get(..8).unwrap_or(node)
            ),
            Self::Misnamed {
                at,
                proved,
                claimed,
            } => write!(
                f,
                "{} proved node {}, its survey says node {}: not read (restart aterm there)",
                at.ip(),
                node_head(proved, &[claimed]),
                // As long as the proved node's head, where the two first differ.
                claimed
                    .chars()
                    .take(node_head(proved, &[claimed]).len())
                    .collect::<String>()
            ),
            Self::Unproved { at, node } => write!(
                f,
                "{} proves no key for node {}, which is bound to one: not read (update aterm \
                 there)",
                at.ip(),
                node.get(..8).unwrap_or(node)
            ),
            Self::Unbound { at, node } => write!(
                f,
                "{} proved no node this machine binds a key to{}: not driven (`aterm fleet \
                 peers` reads the issuer's book)",
                at.ip(),
                node.as_deref().map_or_else(String::new, |node| format!(
                    " (it claims {})",
                    node.get(..8).unwrap_or(node)
                ))
            ),
            Self::Local(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Unreached {}

/// Reach the peer at `addr` (when one is recorded) or `host` on `port`, and prove `op` to it
/// with `secret` and this machine's node `me`, all within one wall-clock deadline `within`.
/// The recorded address goes first, then `host`'s addresses (and `<host>.local`'s for a name
/// with no dot, looked up at once), IPv4 first, at most four; each connect takes at most
/// [`CONNECT_WITHIN`] and each handshake at most [`crate::fleet::HANDSHAKE_WITHIN`]. A stalled
/// address leaves the rest of the deadline for the name's other addresses. A lookup still
/// running at the deadline is left to end on its own thread.
pub fn dial(
    host: &str,
    addr: Option<IpAddr>,
    port: u16,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    op: FleetOp,
    within: Duration,
) -> Result<Reached, Unreached> {
    dial_avoiding(
        host,
        addr,
        port,
        (secret, me),
        op,
        (deadline_after(within), within),
        (&[], resolve, &|_, _| None),
    )
}

/// Dial `host` (`addr` first) on `port` on [`crate::fleet::ALPN_ASK`] by `within`, holding no fleet
/// secret: a Mac asking to join reaches a member's ask handler and nothing else
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10). Each address is walked as [`dial`] walks it: the
/// loopback guard, fleet sources only, the next address after one that does not answer.
/// # Errors
/// As [`dial`]: the first address's failure, or the deadline; a listener that serves no ask (one
/// older than this build) is a handshake failure of [`FleetError::Alpn`].
pub fn dial_ask(
    host: &str,
    addr: Option<IpAddr>,
    port: u16,
    within: Duration,
) -> Result<(TlsTransport<ClientConnection>, SocketAddr), Unreached> {
    let deadline = deadline_after(within);
    walk(
        (host, addr, port),
        (deadline, within),
        (&[], resolve),
        |to| {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return AtAddr::Ended(Unreached::Deadline(Stage::Connect, within));
            }
            match aterm_uds::connect_tcp(&to, left.min(CONNECT_WITHIN)) {
                Ok(tcp) => {
                    let left = deadline
                        .saturating_duration_since(Instant::now())
                        .min(address_handshake_within());
                    match crate::fleet::ask_handshake(tcp, left) {
                        Ok(transport) => AtAddr::Reached((transport, to)),
                        Err(FleetError::Timeout(_)) => handshake_timeout((deadline, within)),
                        Err(error) => AtAddr::Passed(Unreached::Handshake(to, error)),
                    }
                }
                Err(error) => AtAddr::Passed(Unreached::Connect(to, error)),
            }
        },
    )
}

type Resolver = fn(&str, u16) -> Vec<SocketAddr>;

/// What a refusal the listener proved its node beside says of the dial ([`reach`]): why it ends
/// there when that node is not the one dialled, else `None`, and the refusal stands.
type RefusedBy<'a> = &'a dyn Fn(SocketAddr, &NodeProof) -> Option<Unreached>;

fn dial_avoiding(
    host: &str,
    addr: Option<IpAddr>,
    port: u16,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    op: FleetOp,
    (deadline, within): (Instant, Duration),
    (avoid, resolver, refused): (&[SocketAddr], Resolver, RefusedBy<'_>),
) -> Result<Reached, Unreached> {
    walk(
        (host, addr, port),
        (deadline, within),
        (avoid, resolver),
        |to| reach(to, (secret, me), op, (deadline, within), refused),
    )
}

/// The pause before each further pass of a drive's dial ([`passes`]).
const AGAIN_AFTER: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_millis(750),
    Duration::from_secs(2),
];

/// How far into a drive's deadline its dial may go on trying: the longest turn a drive takes
/// (`aterm_types::control_verbs::DRIVE_TURN_MAX_MS`) after this still fits a 28 s drive, which
/// `aterm-agent` holds it to where it compiles against both, and a leg's wait stays its own. No
/// further pass runs past it, so a late handshake refuses with nothing sent rather than leave a typed
/// turn too little time to answer.
pub const DRIVE_DIAL_WITHIN: Duration = Duration::from_secs(6);

/// When a further pass of a drive by `deadline` must end ([`DRIVE_DIAL_WITHIN`]).
fn further_passes_end((by, within): (Instant, Duration)) -> Instant {
    by.checked_sub(within)
        .map_or(by, |started| (started + DRIVE_DIAL_WITHIN).min(by))
}

/// A person's drive `pass`ed over its addresses by `deadline`, and again after each of
/// [`AGAIN_AFTER`] while an address it passed only could not serve it just then
/// ([`Unreached::served_later`]), even when another address supplies the diagnostic, and `dial_by`,
/// the end of each further pass, leaves more than that pause: a listener at its slots drops a dial
/// at once, and a loaded one lets
/// [`crate::fleet::HANDSHAKE_WITHIN`] pass, while the same dial a moment later is served. No request
/// goes before a dial is reached, so a new connection replays nothing. Every other dial takes one
/// pass, as a round repeats it and a join's next place waits on it. A later pass's own answer stands
/// where it says more of the listener than the first's; when none serves the reason is the first's.
fn passes<R, E: Into<PassFailure>>(
    (deadline, dial_by): (Instant, Instant),
    mut pass: impl FnMut(Instant) -> Result<R, E>,
) -> Result<R, Unreached> {
    let first = match pass(deadline) {
        Ok(reached) => return Ok(reached),
        Err(why) => why.into(),
    };
    if !first.retryable {
        return Err(first.why);
    }
    for pause in AGAIN_AFTER {
        if dial_by.saturating_duration_since(Instant::now()) <= pause {
            return Err(first.why);
        }
        std::thread::sleep(pause);
        match pass(dial_by).map_err(Into::into) {
            Ok(reached) => return Ok(reached),
            Err(failed) if failed.retryable => {}
            Err(PassFailure {
                why:
                    Unreached::Resolve(_)
                    | Unreached::Lookup(..)
                    | Unreached::Connect(..)
                    | Unreached::Local(_)
                    | Unreached::Deadline(Stage::Connect, _),
                ..
            }) => return Err(first.why),
            Err(failed) => return Err(failed.why),
        }
    }
    Err(first.why)
}

/// A pass's diagnostic and whether a listener it passed may serve a later connection. A stale
/// address's identity remains the diagnostic even when another address was temporarily busy.
#[derive(Debug)]
struct PassFailure {
    why: Unreached,
    retryable: bool,
}

impl From<Unreached> for PassFailure {
    fn from(why: Unreached) -> Self {
        Self {
            retryable: why.served_later(),
            why,
        }
    }
}

impl PassFailure {
    /// Preserve evidence from passed addresses beside a dial's diagnostic. A local failure
    /// still says this machine could not try, and a terminal refusal uses `From` alone.
    fn exhausted(why: Unreached, retryable: bool) -> Self {
        Self {
            retryable: why.served_later() || (why.dialled() && retryable),
            why,
        }
    }
}

/// What one address of a dial came to ([`walk`]).
enum AtAddr<R> {
    /// It answered: what the dial asks of an address.
    Reached(R),
    /// It was passed over, for this reason: the next address is tried.
    Passed(Unreached),
    /// The dial ends here, for this reason.
    Ended(Unreached),
    /// The listener refused after proving its node: a target-specific walk judges that proof.
    Refused(ProvedRefusal),
}

struct ProvedRefusal {
    why: Unreached,
    peer: NodeProof,
    ended: bool,
}

impl ProvedRefusal {
    fn copied(&self) -> Self {
        Self {
            why: self.why.copied(),
            peer: self.peer.clone(),
            ended: self.ended,
        }
    }
}

impl<R> AtAddr<R> {
    /// Judge an address's successful exchange before it can end the address walk.
    fn and_then<T>(self, judge: impl FnOnce(R) -> Result<T, Unreached>) -> AtAddr<T> {
        match self {
            Self::Reached(reached) => match judge(reached) {
                Ok(judged) => AtAddr::Reached(judged),
                Err(why) => AtAddr::Ended(why),
            },
            Self::Passed(why) => AtAddr::Passed(why),
            Self::Ended(why) => AtAddr::Ended(why),
            Self::Refused(refused) => AtAddr::Refused(refused),
        }
    }
}

/// The longest one address's handshake takes: [`crate::fleet::HANDSHAKE_WITHIN`], or what
/// [`crate::fleet::HANDSHAKE_SEAM`] names for an aterm a test world started. A dial inside a test
/// binary caps no address: its peer serves in process and must finish however loaded the machine
/// is, so only the caller's own deadline bounds it, and [`HeldSilent`] exercises the expiry without
/// a clock.
fn address_handshake_within() -> Duration {
    #[cfg(debug_assertions)]
    {
        if aterm_uds::test_binary::scratch_root().is_some() {
            return Duration::MAX;
        }
        let seamed = std::env::var(crate::fleet::HANDSHAKE_SEAM)
            .ok()
            .filter(|ms| !ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|ms| ms.parse::<u64>().ok())
            .filter(|&ms| ms > 0);
        if let Some(ms) = seamed {
            return Duration::from_millis(ms);
        }
    }
    crate::fleet::HANDSHAKE_WITHIN
}

fn handshake_timeout<R>((deadline, within): (Instant, Duration)) -> AtAddr<R> {
    if Instant::now() >= deadline {
        AtAddr::Ended(Unreached::Deadline(Stage::Handshake, within))
    } else {
        AtAddr::Passed(Unreached::Deadline(
            Stage::Handshake,
            within.min(address_handshake_within()),
        ))
    }
}

/// The addresses of a dial to `host` (`addr` first) on `port` by `deadline`, never one in
/// `avoid`: `attempt` of each in turn until one is reached or ends the dial. Both a short name's
/// lookups start together; each completed batch is tried before waiting for another, at most
/// four addresses per lookup. When none answers the reason is the first failure, or the first
/// past the peer's proof (a busy address),
/// which says where it answers. The loopback guard ([`crate::loopback::check`]) skips an
/// address or a lookup that would leave the loopback, as one past the fleet's kind is skipped,
/// its refusal the reason when nothing else came first.
fn walk<R, Resolve>(
    (host, addr, port): (&str, Option<IpAddr>, u16),
    (deadline, within): (Instant, Duration),
    (avoid, resolver): (&[SocketAddr], Resolve),
    attempt: impl FnMut(SocketAddr) -> AtAddr<R>,
) -> Result<R, Unreached>
where
    Resolve: Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
{
    walk_pass(
        (host, addr, port),
        (deadline, within),
        (avoid, resolver),
        attempt,
    )
    .map_err(|failed| failed.why)
}

/// The same address walk, retaining transient evidence independently of its final diagnostic.
fn walk_pass<R, Resolve>(
    (host, addr, port): (&str, Option<IpAddr>, u16),
    (deadline, within): (Instant, Duration),
    (avoid, resolver): (&[SocketAddr], Resolve),
    attempt: impl FnMut(SocketAddr) -> AtAddr<R>,
) -> Result<R, PassFailure>
where
    Resolve: Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
{
    walk_lookups(
        (host, addr, port),
        within,
        avoid,
        || resolve_batches(host, port, deadline, resolver),
        attempt,
    )
}

/// Consume lookup batches only after the recorded address failed to finish the walk.
fn walk_lookups<R, L: IntoIterator<Item = Lookup>>(
    (host, addr, port): (&str, Option<IpAddr>, u16),
    within: Duration,
    avoid: &[SocketAddr],
    lookup: impl FnOnce() -> L,
    mut attempt: impl FnMut(SocketAddr) -> AtAddr<R>,
) -> Result<R, PassFailure> {
    let mut tried: Vec<SocketAddr> = avoid.to_vec();
    let mut first = None;
    let mut retryable = false;
    let mut at = |to: SocketAddr, first: &mut Option<Unreached>, retryable: &mut bool| {
        if !crate::fleet::is_fleet_source(to.ip()) {
            first.get_or_insert(Unreached::NotFleet(to));
            return None;
        }
        if let Err(refused) = crate::loopback::check(crate::loopback::Reach::Dial(to)) {
            first.get_or_insert(Unreached::Local(refused.to_string()));
            return None;
        }
        match attempt(to) {
            AtAddr::Reached(reached) => Some(Ok(reached)),
            AtAddr::Passed(why)
            | AtAddr::Refused(ProvedRefusal {
                why, ended: false, ..
            }) => {
                *retryable |= why.served_later();
                // What a listener did says more than an address that did not connect. Keep that
                // diagnostic while retryable separately remembers passed transient failures.
                if first.as_ref().is_none_or(|had| {
                    (had.before_proof() && !why.before_proof())
                        || (matches!(had, Unreached::Connect(..)) && why.served_later())
                }) {
                    *first = Some(why);
                }
                None
            }
            AtAddr::Ended(why @ Unreached::Deadline(Stage::Connect, _)) => {
                // No new connection starts: retain what an earlier address established.
                Some(Err(PassFailure::exhausted(
                    first.take().unwrap_or(why),
                    *retryable,
                )))
            }
            AtAddr::Ended(why)
            | AtAddr::Refused(ProvedRefusal {
                why, ended: true, ..
            }) => Some(Err(why.into())),
        }
    };
    if let Some(ip) = addr {
        let to = SocketAddr::new(ip, port);
        if !tried.contains(&to) {
            tried.push(to);
            if let Some(done) = at(to, &mut first, &mut retryable) {
                return done;
            }
        }
    }
    for resolved in lookup() {
        let resolved = match resolved {
            Lookup::Found(resolved) => resolved,
            Lookup::Late => {
                return Err(first
                    .unwrap_or_else(|| Unreached::Lookup(host.to_owned(), within))
                    .into());
            }
            Lookup::Unstarted(e) => {
                return Err(first
                    .unwrap_or_else(|| Unreached::Local(format!("could not look {host} up: {e}")))
                    .into());
            }
            Lookup::Refused(e) => {
                return Err(first
                    .unwrap_or_else(|| Unreached::Local(e.to_string()))
                    .into());
            }
        };
        for to in resolved {
            if tried.contains(&to) {
                continue;
            }
            tried.push(to);
            if let Some(done) = at(to, &mut first, &mut retryable) {
                return done;
            }
        }
    }
    Err(PassFailure::exhausted(
        first.unwrap_or_else(|| Unreached::Resolve(host.to_owned())),
        retryable,
    ))
}

/// One address of a dial: a connect to `to`, then the proof of `op` there, by `deadline`; a
/// refusal by a node the listener proved is judged by `refused` first. A node that dials the mesh
/// ([`NodeIdentity::dialing_mesh`]) offers [`crate::fleet::ALPN_MESH`] first, and where that ends
/// as one that goes again ([`FleetError::falls_back`]: one end's log binds the other's key to no
/// seat yet), it dials the same address once more on [`crate::fleet::ALPN_NODES`] alone, by the
/// same deadline, proving the fleet's secret as before the mesh.
fn reach(
    to: SocketAddr,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    op: FleetOp,
    (deadline, within): (Instant, Duration),
    refused: RefusedBy<'_>,
) -> AtAddr<Reached> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return AtAddr::Ended(Unreached::Deadline(Stage::Connect, within));
    }
    match aterm_uds::connect_tcp(&to, left.min(CONNECT_WITHIN)) {
        Ok(tcp) => {
            let left = deadline
                .saturating_duration_since(Instant::now())
                .min(address_handshake_within());
            #[cfg(any(test, debug_assertions))]
            let left = if held_silent(to, left) {
                Duration::ZERO
            } else {
                left
            };
            match client_handshake_by(tcp, secret, me, op, left) {
                Ok(Dialed {
                    transport,
                    scope,
                    peer,
                    mesh,
                }) => AtAddr::Reached(Reached {
                    transport,
                    scope,
                    addr: to,
                    peer,
                    mesh,
                }),
                Err(Undialed { error, .. })
                    if error.falls_back() && me.is_some_and(NodeIdentity::dials_mesh) =>
                {
                    if matches!(error, FleetError::UnknownKey)
                        && let Some(offer) = UNKNOWN_TO.get()
                    {
                        offer(to);
                    }
                    let fleet = me.map(|me| me.clone().dialing_mesh(None));
                    reach(
                        to,
                        (secret, fleet.as_ref()),
                        op,
                        (deadline, within),
                        refused,
                    )
                }
                Err(Undialed { error, refused_by }) => {
                    if let Some(peer) = refused_by {
                        if let Some(other) = refused(to, &peer) {
                            return AtAddr::Ended(other);
                        }
                        return AtAddr::Refused(ProvedRefusal {
                            ended: matches!(error, FleetError::UnknownOp(_)),
                            why: Unreached::Handshake(to, error),
                            peer,
                        });
                    }
                    match error {
                        FleetError::Timeout(_) => handshake_timeout((deadline, within)),
                        e @ FleetError::UnknownOp(_) => AtAddr::Ended(Unreached::Handshake(to, e)),
                        e => AtAddr::Passed(Unreached::Handshake(to, e)),
                    }
                }
            }
        }
        Err(e) => AtAddr::Passed(Unreached::Connect(to, e)),
    }
}

enum Lookup {
    Found(Vec<SocketAddr>),
    Late,
    Unstarted(io::Error),
    /// The loopback guard asked no resolver ([`crate::loopback::check`]).
    Refused(io::Error),
}

/// Completed lookup batches, consumed only as the address walk needs them. Dropping this
/// receiver never waits for a resolver: a reached peer or its authoritative refusal ends the
/// operation even while the other name remains in DNS.
enum LookupBatches {
    Ready(Option<Lookup>),
    Pending {
        answers: std::sync::mpsc::Receiver<Vec<SocketAddr>>,
        asking: usize,
        unstarted: Option<io::Error>,
        deadline: Instant,
    },
}

impl Iterator for LookupBatches {
    type Item = Lookup;

    fn next(&mut self) -> Option<Lookup> {
        match self {
            Self::Ready(ready) => ready.take(),
            Self::Pending {
                answers,
                asking,
                unstarted,
                deadline,
            } => {
                while *asking > 0 {
                    *asking -= 1;
                    let left = deadline.saturating_duration_since(Instant::now());
                    match answers.recv_timeout(left) {
                        Ok(found) if !found.is_empty() => return Some(Lookup::Found(found)),
                        Ok(_) => {}
                        Err(_) => {
                            *asking = 0;
                            *unstarted = None;
                            return Some(Lookup::Late);
                        }
                    }
                }
                unstarted.take().map(Lookup::Unstarted)
            }
        }
    }
}

const RESOLVER_THREADS_MAX: usize = 2 * AT_ONCE;
static RESOLVERS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

struct ResolverPermit<'a>(&'a AtomicUsize);

impl<'a> ResolverPermit<'a> {
    fn take(in_flight: &'a AtomicUsize) -> Option<Self> {
        let mut held = in_flight.load(Ordering::Relaxed);
        loop {
            if held >= RESOLVER_THREADS_MAX {
                return None;
            }
            match in_flight.compare_exchange_weak(
                held,
                held + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self(in_flight)),
                Err(now) => held = now,
            }
        }
    }
}

impl Drop for ResolverPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn resolve_within(host: &str, port: u16, within: Duration, resolver: Resolver) -> Lookup {
    resolve_with_admission(host, port, within, resolver, || {
        ResolverPermit::take(&RESOLVERS_IN_FLIGHT)
    })
}

fn resolve_with_admission(
    host: &str,
    port: u16,
    within: Duration,
    resolver: impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
    admit: impl FnMut() -> Option<ResolverPermit<'static>>,
) -> Lookup {
    resolve_batches_with_admission(host, port, deadline_after(within), resolver, admit)
        .next()
        .unwrap_or_else(|| Lookup::Found(Vec::new()))
}

fn resolve_batches(
    host: &str,
    port: u16,
    deadline: Instant,
    resolver: impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
) -> LookupBatches {
    resolve_batches_with_admission(host, port, deadline, resolver, || {
        ResolverPermit::take(&RESOLVERS_IN_FLIGHT)
    })
}

fn resolve_batches_with_admission(
    host: &str,
    port: u16,
    deadline: Instant,
    resolver: impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
    mut admit: impl FnMut() -> Option<ResolverPermit<'static>>,
) -> LookupBatches {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return LookupBatches::Ready(Some(Lookup::Found(vec![SocketAddr::new(ip, port)])));
    }
    if let Err(refused) = crate::loopback::check(crate::loopback::Reach::Lookup(host)) {
        return LookupBatches::Ready(Some(Lookup::Refused(refused)));
    }
    if deadline <= Instant::now() {
        return LookupBatches::Ready(Some(Lookup::Late));
    }
    let mut names = vec![host.to_owned()];
    if !host.contains('.') {
        names.push(format!("{host}.local"));
    }
    names.retain(|name| crate::loopback::check(crate::loopback::Reach::Lookup(name)).is_ok());
    let (tx, rx) = std::sync::mpsc::sync_channel(names.len());
    let mut asking = 0usize;
    let mut unstarted = None;
    for name in names {
        let Some(permit) = admit() else {
            unstarted = Some(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the fleet resolver is at capacity",
            ));
            break;
        };
        let tx = tx.clone();
        let resolver = resolver.clone();
        match std::thread::Builder::new()
            .name("aterm-fleet-resolve".to_owned())
            .spawn(move || {
                let _permit = permit;
                let _ = tx.send(resolver(&name, port));
            }) {
            Ok(_) => asking += 1,
            Err(e) => unstarted = Some(e),
        }
    }
    drop(tx);
    if asking == 0 {
        return LookupBatches::Ready(Some(unstarted.map_or(Lookup::Late, Lookup::Unstarted)));
    }
    LookupBatches::Pending {
        answers: rx,
        asking,
        unstarted,
        deadline,
    }
}

/// The longest one fleet lookup runs on its resolver thread ([`resolve`]); its caller's own
/// deadline ends the caller's wait sooner.
const LOOKUP_BOUND: Duration = Duration::from_secs(30);

/// `name`'s addresses on `port` (IPv4 first, at most four), empty for a lookup that failed:
/// `aterm_uds::lan::resolve`, which on macOS has the root LAN broker resolve a `.local` or
/// single-label name and resolves any other name here, `localhost` (the loopback's, RFC 6761
/// §6.3) included.
fn resolve(name: &str, port: u16) -> Vec<SocketAddr> {
    let Ok(found) = aterm_uds::lan::resolve(name, LOOKUP_BOUND) else {
        return Vec::new();
    };
    let mut found: Vec<SocketAddr> = found
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect();
    found.sort_by_key(SocketAddr::is_ipv6);
    found.dedup();
    found.truncate(4);
    found
}

/// The first IPv4 [`SocketAddr`] each `(host, port)` in `places` resolves to within `within`,
/// looked up concurrently through the bounded fleet resolver pool ([`RESOLVER_THREADS_MAX`]) and
/// the loopback guard ([`crate::loopback::check`]), in `places`'s order: a slow or hanging lookup
/// neither holds up the round past `within` nor starves a place after it.
#[must_use]
pub fn resolve_ipv4_hints_within(places: &[(String, u16)], within: Duration) -> Vec<SocketAddr> {
    resolve_ipv4_hints_with(places, within, resolve)
}

/// The first completed nonempty address batch for `host`, in its order (IPv4 first, at most four;
/// for a name with no dot, those of `host` or of `<host>.local`, whichever lookup answers first),
/// looked up within `within` through the bounded fleet resolver pool and the
/// loopback guard: an IP literal is itself, and a lookup that found nothing, was refused or ran
/// past `within` is empty. A dial can continue with the other lookup if this batch does not
/// reach its target; this snapshot does not wait for that lookup.
#[must_use]
pub fn addresses_within(host: &str, within: Duration) -> Vec<IpAddr> {
    match resolve_within(host, 0, within, resolve) {
        Lookup::Found(found) => found.iter().map(SocketAddr::ip).collect(),
        Lookup::Late | Lookup::Unstarted(_) | Lookup::Refused(_) => Vec::new(),
    }
}

fn resolve_ipv4_hints_with(
    places: &[(String, u16)],
    within: Duration,
    resolver: Resolver,
) -> Vec<SocketAddr> {
    let deadline = deadline_after(within);
    let mut slots: Vec<Option<SocketAddr>> = vec![None; places.len()];
    let (tx, rx) = std::sync::mpsc::sync_channel(places.len().max(1));
    let mut asking = 0usize;
    for (idx, (host, port)) in places.iter().enumerate() {
        let port = *port;
        if let Ok(ip) = host.parse::<IpAddr>() {
            let to = SocketAddr::new(ip, port);
            if to.is_ipv4() && crate::loopback::check(crate::loopback::Reach::Dial(to)).is_ok() {
                slots[idx] = Some(to);
            }
            continue;
        }
        if within.is_zero() || crate::loopback::check(crate::loopback::Reach::Lookup(host)).is_err()
        {
            continue;
        }
        let Some(permit) = ResolverPermit::take(&RESOLVERS_IN_FLIGHT) else {
            break;
        };
        let name = host.clone();
        let tx = tx.clone();
        if std::thread::Builder::new()
            .name("aterm-fleet-resolve".to_owned())
            .spawn(move || {
                let _permit = permit;
                let found = resolver(&name, port).into_iter().find(|to| {
                    to.is_ipv4()
                        && crate::loopback::check(crate::loopback::Reach::Dial(*to)).is_ok()
                });
                let _ = tx.send((idx, found));
            })
            .is_ok()
        {
            asking += 1;
        }
    }
    drop(tx);
    for _ in 0..asking {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((idx, found)) => {
                if let Some(slot) = slots.get_mut(idx) {
                    *slot = found;
                }
            }
            Err(_) => break,
        }
    }
    slots.into_iter().flatten().collect()
}

/// One peer to reach: what [`dial`] takes, and the node it should be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Its host name.
    pub host: String,
    /// Its recorded address, tried first.
    pub addr: Option<IpAddr>,
    /// Its fleet port.
    pub port: u16,
    /// Its node id, when the book knows it: a reply naming another is another member's
    /// ([`Unreached::OtherNode`]).
    pub node: Option<String>,
    /// The key the book binds that node to, if any: a peer claiming the node is read as it
    /// only when it proves this key ([`Unreached::Impostor`], [`Unreached::Unproved`]).
    pub key: Option<NodeKey>,
}

/// `ask` of each of `items`, at most `limit` at once on threads of their own (this one among
/// them, so a thread that cannot start only slows the walk): the answers in `items`' order.
pub(crate) fn at_once<T, R>(items: &[T], limit: usize, ask: impl Fn(&T) -> R + Sync) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    let next = AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let at = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(at) else {
                return done;
            };
            done.push((at, ask(item)));
        }
    };
    let mut answers: Vec<(usize, R)> = std::thread::scope(|scope| {
        let helpers: Vec<_> = (1..limit.clamp(1, items.len().max(1)))
            .filter_map(|_| {
                std::thread::Builder::new()
                    .name("aterm-fleet-ask".to_owned())
                    .spawn_scoped(scope, work)
                    .ok()
            })
            .collect();
        let mut answers = work();
        for helper in helpers {
            match helper.join() {
                Ok(done) => answers.extend(done),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        answers
    });
    answers.sort_by_key(|(at, _)| *at);
    answers.into_iter().map(|(_, answer)| answer).collect()
}

/// What a check of one peer found when it answered (`aterm fleet peers`: a [`read_whole`]
/// with `survey facts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// The level it proved it serves.
    pub scope: FleetScope,
    /// The address it answered at.
    pub addr: SocketAddr,
    /// The node that answered ([`Surveyed::node`]).
    pub node: Option<String>,
    /// Whether that node is proved ([`Surveyed::proven`]).
    pub proven: bool,
    /// The instance that answered, as its reply's `instance` line says it ([`Answerer::of`]).
    pub answerer: Answerer,
}

/// The instance that answered a survey, as the reply's first `instance` line says it. A Mac's
/// node job (`aterm node serve`) signs that Mac's book entry with its own version and relays a
/// survey to a live window, whose own version the line then says, so after an update the two can
/// differ (`aterm fleet peers`' NODE VERSION and WINDOW VERSION).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answerer {
    /// A window answered, running this aterm version: its line says `window=1`, or no `window=`
    /// word at all (an older aterm's, whose window held the fleet port itself).
    Window(String),
    /// The node job answered with no window open (`window=0`).
    Windowless,
    /// The reply has no `instance` line, or its line has no version word ([`version_word`]) or a
    /// `window=` word other than `0` or `1`.
    Unsaid,
}

impl Answerer {
    /// What `lines` (a survey reply after its `OK <n>` head) say of the instance that answered:
    /// its first `instance` line's `window=` and `version=` words.
    #[must_use]
    pub fn of(lines: &[String]) -> Self {
        let Some(line) = lines.iter().find_map(|line| line.strip_prefix("instance ")) else {
            return Self::Unsaid;
        };
        let said = fields(line);
        let word = |key: &str| said.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let window = || {
            word("version")
                .and_then(|v| version_word(&aterm_codec::pct::decode(v)))
                .map_or(Self::Unsaid, Self::Window)
        };
        match word("window") {
            Some("0") => Self::Windowless,
            Some("1") | None => window(),
            Some(_) => Self::Unsaid,
        }
    }
}

/// One peer's survey, as it answered it ([`survey_all`]).
#[derive(Debug)]
pub struct Surveyed {
    /// The level it proved it serves.
    pub scope: FleetScope,
    /// The address it answered at.
    pub addr: SocketAddr,
    /// The node that answered: the one it proved on the connection, else (an older aterm) the
    /// one its header names (`node=`); `None` for an aterm that says neither.
    pub node: Option<String>,
    /// Whether that node is proved: it proved the key this machine binds it to, or this
    /// machine's own key (this machine, at a place named by hand).
    pub proven: bool,
    /// The key it proved that node under, if any: what a whole read judges the answer by again
    /// once the issuer's book has come ([`read_whole`]).
    pub key: Option<NodeKey>,
    /// Its reply's lines after the `OK <n>` head, newlines dropped.
    pub lines: Vec<String>,
}

impl Surveyed {
    /// What a check reads of it: the level, the address, the node, whether it is proved, and
    /// what its `instance` line says of the instance that answered.
    #[must_use]
    pub fn checked(&self) -> Checked {
        Checked {
            scope: self.scope,
            addr: self.addr,
            node: self.node.clone(),
            proven: self.proven,
            answerer: Answerer::of(&self.lines),
        }
    }
}

/// The fleet read (`aterm ctl survey --fleet`, design §3): every target asked `survey`, at
/// most [`AT_ONCE`] at a time, each under one wall-clock deadline `within` that covers the
/// name lookup, the connects, TLS, both proofs, the request and the whole reply; a reply past
/// [`REPLY_MAX`] bytes is refused. Targets that reach one address read the one exchange there
/// ([`HeldAddrs`]).
#[must_use]
pub fn survey_all(
    targets: &[Target],
    keys: &Keyring,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    within: Duration,
) -> Vec<Result<Surveyed, Unreached>> {
    let held = std::sync::Arc::new(HeldAddrs::default());
    at_once(targets, AT_ONCE, |target| {
        read_one(target, keys, (secret, me), within, "survey", &held)
    })
}

/// [`survey_all`] of the one `target`, by a `deadline` set from `within` that a read before it
/// (its issuer's book, read again for a word no book here holds) may have spent part of.
pub fn survey_one_by(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    deadline: (Instant, Duration),
) -> Result<Surveyed, Unreached> {
    let held = std::sync::Arc::new(HeldAddrs::default());
    read_one_by(target, keys, (secret, me), deadline, "survey", &held)
}

/// What [`dial`] takes for `peer`, and the node and key it should be.
#[must_use]
pub fn target_of(peer: &Peer) -> Target {
    Target {
        host: peer.host.clone(),
        addr: peer.addr,
        port: peer.port,
        node: peer.node.clone(),
        key: peer.key,
    }
}

fn same_target(a: &Peer, b: &Peer) -> bool {
    a.node == b.node
        && a.key == b.key
        && a.port == b.port
        && a.addr == b.addr
        && a.host.eq_ignore_ascii_case(&b.host)
}

/// One peer of a whole read ([`read_whole`]): the entry it was read by, and its answer.
#[derive(Debug)]
pub struct PeerRead {
    /// The entry: the machine's own, or a copy of the issuer's book as the book read now has it.
    pub peer: Peer,
    /// Its answer, or why there is none.
    pub read: Result<Surveyed, Unreached>,
}

/// What a whole read made of the issuer's book ([`read_whole`]).
#[derive(Debug)]
pub enum BookRead {
    /// The peers name no issuer (`via=issuer`): this machine issues the fleet, or knows only
    /// peers named by hand.
    NoIssuer,
    /// The issuer's book: [`copies_of`] it is the member's copy now.
    Fetched(Book),
    /// The issuer itself answered that it shares no book ([`BookUnread::Older`] or
    /// [`BookUnread::NoBook`], judged its own by its node or its survey's), so the copy goes.
    Refused(BookUnread),
    /// The issuer answered its survey but no book came, for this reason: the copy stands.
    Unshared(String),
    /// The issuer was not reached (its own row says why): the copy stands.
    Unread(String),
}

/// The member's whole read (`aterm ctl survey --fleet`, `aterm fleet peers`): `request`
/// (`survey`, or `survey facts` for a check) of each of `peers` (its book, aterm.toml's peers,
/// then its copy of its issuer's book) and of every member the issuer's book adds, the first
/// [`AT_ONCE`] dials under one deadline `within` from now. The book's exchange at the issuer's
/// recorded address opens before any other dial, and a read the copy's keys refused is read
/// again when the book's keys judge that node otherwise.
#[must_use]
pub fn read_whole(
    peers: &[Peer],
    keys: &Keyring,
    own: Option<&str>,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    within: Duration,
    request: &str,
) -> (Vec<PeerRead>, BookRead) {
    let Some(issuer_at) = peers.iter().position(|peer| peer.via == Via::Issuer) else {
        let targets: Vec<Target> = peers.iter().map(target_of).collect();
        let held = std::sync::Arc::new(HeldAddrs::default());
        let reads = at_once(&targets, AT_ONCE, |target| {
            read_one(target, keys, (secret, me), within, request, &held)
        });
        let reads = peers
            .iter()
            .cloned()
            .zip(reads)
            .map(|(peer, read)| PeerRead { peer, read })
            .collect();
        return (reads, BookRead::NoIssuer);
    };
    let deadline = (deadline_after(within), within);
    let given = peers.len();
    let addrs = std::sync::Arc::new(HeldAddrs::default());
    let held = &addrs;
    let issuer = target_of(&peers[issuer_at]);
    // The book's exchange at the issuer's recorded address, open before any other dial begins.
    let first = issuer
        .addr
        .map(|ip| SocketAddr::new(ip, issuer.port))
        .and_then(|to| Some((to, held.enter(to).ok()?)));
    let queue = std::sync::Mutex::new(Queue::seated(peers.to_vec(), issuer_at, keys.clone()));
    let lock = || {
        queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let work = || {
        let mut done = Vec::new();
        loop {
            let next = {
                let mut queued = lock();
                let next = queued.take(deadline);
                if next.is_none() {
                    queued.live = queued.live.saturating_sub(1);
                }
                next.map(|next| (next, std::sync::Arc::clone(&queued.keys)))
            };
            let Some(((at, target, by, read), keys)) = next else {
                return done;
            };
            let answer = read_one_by(&target, &keys, (secret, me), by, request, held);
            if let Some(node) = refused_for(&answer) {
                lock().refused(at, node, &keys, by);
            }
            done.push((at, read, answer));
        }
    };
    let (reads, book, dropped) = std::thread::scope(|scope| {
        let helper = move || {
            let spawned = std::thread::Builder::new()
                .name("aterm-fleet-ask".to_owned())
                .spawn_scoped(scope, work);
            if spawned.is_err() {
                let mut queued = lock();
                queued.live = queued.live.saturating_sub(1);
            }
            spawned.ok()
        };
        let hands = {
            let mut queued = lock();
            let hands = (given - 1).clamp(1, AT_ONCE - 1);
            queued.live += hands;
            hands
        };
        let helpers: Vec<_> = (1..hands).filter_map(|_| helper()).collect();
        let seat = std::thread::Builder::new()
            .name("aterm-fleet-book".to_owned())
            .spawn_scoped(scope, move || {
                let take = |book: &Book| {
                    let (dropped, more) = {
                        let mut queued = lock();
                        let dropped = queue_book(&mut queued, given, book, own);
                        (dropped, queued.hands())
                    };
                    let started: Vec<_> = (0..more).filter_map(|_| helper()).collect();
                    (dropped, started)
                };
                let mut helpers = Vec::new();
                let mut dropped = Vec::new();
                let fetched = fetch_book_in(
                    &issuer,
                    (secret, me),
                    deadline,
                    (Some(held.as_ref()), first),
                );
                if let Ok(book) = &fetched {
                    (dropped, helpers) = take(book);
                }
                let (issuer_read, book) = match fetched {
                    Err(BookUnread::Unreached(unreached))
                        if ends_the_seat(&unreached, deadline.0, Instant::now()) =>
                    {
                        let why = unreached.to_string();
                        (Err(unreached), BookRead::Unread(why))
                    }
                    mut fetched => {
                        let surveyed = Target {
                            addr: answered_at(&fetched).or(issuer.addr),
                            ..issuer.clone()
                        };
                        let keys = std::sync::Arc::clone(&lock().keys);
                        let issuer_read =
                            read_one_by(&surveyed, &keys, (secret, me), deadline, request, held);
                        let want = issuer.node.as_deref();
                        let found = match &issuer_read {
                            Ok(surveyed) if want.is_some() && surveyed.node.as_deref() == want => {
                                Some(surveyed.addr)
                            }
                            _ => None,
                        };
                        let mut may_ask = true;
                        let book = loop {
                            match judge(fetched, found, want, may_ask) {
                                Judged::Book(book) => break BookRead::Fetched(book),
                                Judged::Refused(refused) => {
                                    dropped = drop_copies(&mut lock(), given);
                                    break BookRead::Refused(refused);
                                }
                                Judged::AskAt(at) => {
                                    may_ask = false;
                                    let there = Target {
                                        host: at.ip().to_string(),
                                        addr: Some(at.ip()),
                                        port: at.port(),
                                        node: issuer.node.clone(),
                                        key: issuer.key,
                                    };
                                    fetched = fetch_book_in(
                                        &there,
                                        (secret, me),
                                        deadline,
                                        (Some(held.as_ref()), None),
                                    );
                                    if let Ok(book) = &fetched {
                                        let (gone, started) = take(book);
                                        dropped = gone;
                                        helpers.extend(started);
                                    }
                                }
                                Judged::Kept(why) if issuer_read.is_ok() => {
                                    break BookRead::Unshared(why);
                                }
                                Judged::Kept(why) => break BookRead::Unread(why),
                            }
                        };
                        (issuer_read, book)
                    }
                };
                let more = lock().hands();
                helpers.extend((0..more).filter_map(|_| helper()));
                let mut done = work();
                done.push((issuer_at, 0, issuer_read));
                for helper in helpers {
                    match helper.join() {
                        Ok(theirs) => done.extend(theirs),
                        Err(panic) => std::panic::resume_unwind(panic),
                    }
                }
                (done, book, dropped)
            });
        let mut done = work();
        for helper in helpers {
            match helper.join() {
                Ok(theirs) => done.extend(theirs),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        let (book, dropped) = match seat {
            Ok(seat) => match seat.join() {
                Ok((theirs, book, dropped)) => {
                    done.extend(theirs);
                    (book, dropped)
                }
                Err(panic) => std::panic::resume_unwind(panic),
            },
            Err(e) => {
                let why = format!("could not start the issuer's read: {e}");
                done.push((issuer_at, 0, Err(Unreached::Local(why.clone()))));
                (BookRead::Unread(why), Vec::new())
            }
        };
        (done, book, dropped)
    });
    let queued = queue
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Each place's latest read: one read again holds what it found.
    let mut latest: Vec<Option<(usize, Result<Surveyed, Unreached>)>> =
        std::iter::repeat_with(|| None)
            .take(queued.peers.len())
            .collect();
    for (at, read, answer) in reads {
        if let Some(slot) = latest.get_mut(at)
            && slot.as_ref().is_none_or(|(had, _)| *had < read)
        {
            *slot = Some((read, answer));
        }
    }
    let mut slots: Vec<Option<Result<Surveyed, Unreached>>> = latest
        .into_iter()
        .map(|slot| slot.map(|(_, answer)| answer))
        .collect();
    for (peer, slot) in queued.peers.iter().zip(&mut slots) {
        *slot = slot.take().map(|read| {
            read.and_then(|surveyed| judged_again(&target_of(peer), &queued.keys, me, surveyed))
        });
    }
    let itself = |peer: &Peer, read: &Option<Result<Surveyed, Unreached>>| {
        peer.via == Via::IssuerBook
            && own.is_some()
            && matches!(read, Some(Ok(surveyed))
                if surveyed.proven && surveyed.node.as_deref() == own)
    };
    let answers = queued
        .peers
        .into_iter()
        .zip(slots)
        .enumerate()
        .filter(|(at, (peer, read))| !dropped.contains(at) && !itself(peer, read))
        .filter_map(|(_, (peer, read))| Some(PeerRead { peer, read: read? }))
        .collect();
    (answers, book)
}

const fn answered_at(fetched: &Result<Book, BookUnread>) -> Option<IpAddr> {
    match fetched {
        Ok(book) => Some(book.at),
        Err(
            BookUnread::Older(at)
            | BookUnread::NoBook { at, .. }
            | BookUnread::NotNow { at, .. }
            | BookUnread::OtherNode { at, .. }
            | BookUnread::Unreached(
                Unreached::Reply(at, _) | Unreached::Handshake(at, FleetError::AddressBusy),
            ),
        ) => Some(at.ip()),
        Err(BookUnread::Unreached(_)) => None,
    }
}

fn ends_the_seat(unreached: &Unreached, deadline: Instant, now: Instant) -> bool {
    unreached.before_proof() || now >= deadline
}

enum Judged {
    Book(Book),
    Refused(BookUnread),
    AskAt(SocketAddr),
    Kept(String),
}

fn judge(
    fetched: Result<Book, BookUnread>,
    found: Option<SocketAddr>,
    want: Option<&str>,
    may_ask: bool,
) -> Judged {
    // Each node by a head the issuer's own id does not share: these say it is not the issuer.
    let short = |node: &str| node_head(node, want.as_slice()).to_owned();
    let (at, node) = match &fetched {
        Err(BookUnread::Unreached(
            Unreached::Impostor { at, .. } | Unreached::Unproved { at, .. },
        )) => (Some(*at), None),
        Ok(_) | Err(BookUnread::Unreached(_)) => (None, None),
        Err(BookUnread::Older(at)) => (Some(*at), None),
        Err(BookUnread::NoBook { at, node, .. } | BookUnread::NotNow { at, node, .. }) => {
            (Some(*at), node.clone())
        }
        Err(BookUnread::OtherNode { at, said, .. }) => (Some(*at), Some(said.clone())),
    };
    let theirs = at.is_some_and(|at| match node.as_deref() {
        Some(node) => want == Some(node),
        None => found == Some(at),
    });
    let why = match fetched {
        Ok(book) => return Judged::Book(book),
        Err(BookUnread::Unreached(
            unreached @ (Unreached::Impostor { .. } | Unreached::Unproved { .. }),
        )) => unreached.to_string(),
        Err(BookUnread::Unreached(unreached)) => return Judged::Kept(unreached.to_string()),
        Err(refused @ (BookUnread::Older(_) | BookUnread::NoBook { .. })) if theirs => {
            return Judged::Refused(refused);
        }
        Err(BookUnread::NotNow { why, .. }) if theirs => return Judged::Kept(why),
        Err(BookUnread::Older(at)) => {
            format!(
                "{at} answered as an older aterm, which the issuer's own survey did not confirm"
            )
        }
        Err(BookUnread::NoBook { at, node, why } | BookUnread::NotNow { at, node, why }) => {
            match node {
                Some(node) => {
                    format!(
                        "{at} answered as node {}, not the issuer: {why}",
                        short(&node)
                    )
                }
                None => {
                    format!("{at} answered, which the issuer's own survey did not confirm: {why}")
                }
            }
        }
        Err(BookUnread::OtherNode { at, said, .. }) => format!(
            "{at} answered with the book of node {}, not the issuer's",
            short(&said)
        ),
    };
    match (found, at) {
        (Some(issuer), Some(at)) if may_ask && issuer != at => Judged::AskAt(issuer),
        _ => Judged::Kept(why),
    }
}

struct Queue {
    peers: Vec<Peer>,
    next: usize,
    skip: Vec<usize>,
    live: usize,
    started: usize,
    keys: std::sync::Arc<Keyring>,
    refused: Vec<Refused>,
    again: Vec<(usize, (Instant, Duration))>,
    reads: usize,
}

/// A read its keys refused ([`refused_for`]), read again once the read's keys judge that node
/// otherwise.
struct Refused {
    at: usize,
    node: String,
    keys: std::sync::Arc<Keyring>,
    by: (Instant, Duration),
}

impl Queue {
    /// `peers` to read by `keys`, the issuer's seat at `issuer_at` reading from the start.
    fn seated(peers: Vec<Peer>, issuer_at: usize, keys: Keyring) -> Self {
        Self {
            peers,
            next: 0,
            skip: vec![issuer_at],
            live: 1,
            started: 1,
            keys: std::sync::Arc::new(keys),
            refused: Vec::new(),
            again: Vec::new(),
            reads: 0,
        }
    }

    /// The next peer to read, its deadline and the read's number: a place to read again
    /// first, by its first read's deadline; then the next queued, by `deadline`, or past the
    /// first [`AT_ONCE`] dials by a deadline of its own from now.
    fn take(
        &mut self,
        deadline: (Instant, Duration),
    ) -> Option<(usize, Target, (Instant, Duration), usize)> {
        let (at, by) = match self.again.pop() {
            Some(again) => again,
            None => loop {
                let at = self.next;
                if at >= self.peers.len() {
                    return None;
                }
                self.next += 1;
                if !self.skip.contains(&at) {
                    self.started += 1;
                    let by = if self.started > AT_ONCE {
                        (deadline_after(deadline.1), deadline.1)
                    } else {
                        deadline
                    };
                    break (at, by);
                }
            },
        };
        self.reads += 1;
        Some((at, target_of(&self.peers[at]), by, self.reads))
    }

    fn hands(&mut self) -> usize {
        let left = self.again.len()
            + (self.next..self.peers.len())
                .filter(|at| !self.skip.contains(at))
                .count();
        let more = left.min(AT_ONCE.saturating_sub(self.live));
        self.live += more;
        more
    }

    /// A read of `at` that `keys` refused for `node`: read again at once when the read's keys
    /// judge that node otherwise, else kept for a book's keys ([`queue_book`]).
    fn refused(
        &mut self,
        at: usize,
        node: &str,
        keys: &std::sync::Arc<Keyring>,
        by: (Instant, Duration),
    ) {
        if self.skip.contains(&at) {
            return;
        }
        if keys.bound(node) != self.keys.bound(node) {
            self.again_by(at, by);
        } else {
            self.refused.push(Refused {
                at,
                node: node.to_owned(),
                keys: std::sync::Arc::clone(keys),
                by,
            });
        }
    }

    /// The place `at` read again by `by`, while that leaves it time.
    fn again_by(&mut self, at: usize, by: (Instant, Duration)) {
        if Instant::now() < by.0 {
            self.again.push((at, by));
        }
    }
}

/// The node a read's keys refused it for ([`Unreached::Impostor`], [`Unreached::Unproved`]),
/// which a book's keys may judge otherwise.
fn refused_for(answer: &Result<Surveyed, Unreached>) -> Option<&str> {
    match answer {
        Err(Unreached::Impostor { node, .. } | Unreached::Unproved { node, .. }) => Some(node),
        _ => None,
    }
}

fn queue_book(queued: &mut Queue, given: usize, book: &Book, own: Option<&str>) -> Vec<usize> {
    queued.keys = std::sync::Arc::new(queued.keys.with_book(book, own));
    // Each refusal these keys judge otherwise is read again under them.
    let (again, still): (Vec<Refused>, Vec<Refused>) = std::mem::take(&mut queued.refused)
        .into_iter()
        .partition(|refused| refused.keys.bound(&refused.node) != queued.keys.bound(&refused.node));
    queued.refused = still;
    let mut merged = queued.peers[..given].to_vec();
    merge_book(&mut merged, book, own);
    let fresh: Vec<Peer> = merged
        .into_iter()
        .filter(|peer| peer.via == Via::IssuerBook)
        .collect();
    let dropped: Vec<usize> = (0..given)
        .filter(|at| {
            queued.peers[*at].via == Via::IssuerBook
                && !fresh
                    .iter()
                    .any(|copy| same_target(copy, &queued.peers[*at]))
        })
        .collect();
    queued.skip.extend(&dropped);
    // A copy the book keeps at its place is listed as the book has it now (its seen_ms, its
    // version): the dial it makes is the same.
    for peer in &mut queued.peers[..given] {
        if let Some(copy) = fresh
            .iter()
            .find(|copy| peer.via == Via::IssuerBook && same_target(copy, peer))
        {
            *peer = copy.clone();
        }
    }
    for refused in again {
        if !queued.skip.contains(&refused.at) {
            queued.again_by(refused.at, refused.by);
        }
    }
    for copy in fresh {
        let known = queued
            .peers
            .iter()
            .enumerate()
            .any(|(at, peer)| !dropped.contains(&at) && stands_for(peer, &copy));
        if !known {
            queued.peers.push(copy);
        }
    }
    dropped
}

fn drop_copies(queued: &mut Queue, given: usize) -> Vec<usize> {
    let dropped: Vec<usize> = (0..given)
        .filter(|at| queued.peers[*at].via == Via::IssuerBook)
        .collect();
    queued.skip.extend(&dropped);
    dropped
}

fn read_one(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    within: Duration,
    request: &str,
    held: &std::sync::Arc<HeldAddrs>,
) -> Result<Surveyed, Unreached> {
    read_one_by(
        target,
        keys,
        (secret, me),
        (deadline_after(within), within),
        request,
        held,
    )
}

fn read_one_by(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    deadline: (Instant, Duration),
    request: &str,
    held: &std::sync::Arc<HeldAddrs>,
) -> Result<Surveyed, Unreached> {
    read_one_with(target, keys, (secret, me), deadline, request, held, resolve)
}

fn read_one_with(
    target: &Target,
    keys: &Keyring,
    identity: (&FleetSecret, Option<&NodeIdentity>),
    deadline: (Instant, Duration),
    request: &str,
    held: &HeldAddrs,
    resolver: impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
) -> Result<Surveyed, Unreached> {
    walk_target(target, keys, deadline, resolver, |to| {
        ask_at(target, keys, to, identity, deadline, request, held)
    })
}

/// A peer reached to be driven ([`drive`]; §5.1 of the sessions-as-subagents design): the
/// proved stream, ready for the control lines the peer's drive gate admits.
pub struct Driven {
    stream: rustls::StreamOwned<ClientConnection, TcpStream>,
    watchdog: Option<HandshakeWatchdog>,
    /// The address it answered at.
    pub addr: SocketAddr,
    /// The node it proved, which this machine binds the proved key to.
    pub node: String,
    /// The exact key authenticated by this connection's handshake.
    pub key: NodeKey,
}

impl Driven {
    /// Whether the drive's deadline hung the connection up ([`HandshakeWatchdog::fired`]): what a
    /// read or write meets then is that deadline's, never the peer's.
    fn cut(&self) -> bool {
        self.watchdog.as_ref().is_some_and(HandshakeWatchdog::fired)
    }
}

impl Read for Driven {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.stream.read(buf) {
            Ok(0) if !buf.is_empty() && self.cut() => Err(io::ErrorKind::TimedOut.into()),
            Err(_) if self.cut() => Err(io::ErrorKind::TimedOut.into()),
            read => read,
        }
    }
}

impl Write for Driven {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.stream.write(buf) {
            Err(_) if self.cut() => Err(io::ErrorKind::TimedOut.into()),
            wrote => wrote,
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.stream.flush() {
            Err(_) if self.cut() => Err(io::ErrorKind::TimedOut.into()),
            flushed => flushed,
        }
    }
}

impl Drop for Driven {
    fn drop(&mut self) {
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.finish();
        }
    }
}

/// Dial `target` to drive it (`aterm ctl --host`): [`FleetOp::Drive`], proved with `secret`
/// and this machine's node `me` (a drive with no node is never admitted), by one `deadline` (set
/// from the `within` its reasons name) for the dial and every exchange after it. A stalled
/// handshake or an answer from a different node gives way to the host name's other addresses;
/// each address must still prove the target's bound node key. A listener that only could not serve
/// the dial just then is dialled again ([`passes`]).
pub fn drive(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, &NodeIdentity),
    deadline: (Instant, Duration),
) -> Result<Driven, Unreached> {
    let (by, within) = deadline;
    let (reached, node) = passes((by, further_passes_end(deadline)), |by| {
        reach_node_pass(
            target,
            keys,
            (secret, me),
            FleetOp::Drive,
            (by, within),
            resolve,
        )
    })?;
    driven(reached, node, deadline)
}

/// Whether `target`'s node answers this machine as itself now: dialled as [`drive`] dials it,
/// with [`FleetOp::Book`] (which a listener answers itself, at every level), it proves the node
/// under the key `target` or `keys` binds it to, within `within`. The address it answered at.
/// # Errors
/// Why it did not: not reached, another node or none there, or another key.
pub fn proves_itself(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, &NodeIdentity),
    within: Duration,
) -> Result<SocketAddr, Unreached> {
    let deadline = (deadline_after(within), within);
    reach_node(target, keys, (secret, me), FleetOp::Book, deadline).map(|(reached, _)| reached.addr)
}

/// `target`'s bound node reached for `op`: at its recorded address, then at its host name's
/// others after a stalled handshake or an answer from another node.
fn reach_node(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, &NodeIdentity),
    op: FleetOp,
    deadline: (Instant, Duration),
) -> Result<(Reached, String), Unreached> {
    reach_node_with(target, keys, (secret, me), op, deadline, resolve)
}

fn reach_node_with(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, &NodeIdentity),
    op: FleetOp,
    deadline: (Instant, Duration),
    resolver: impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
) -> Result<(Reached, String), Unreached> {
    reach_node_pass(target, keys, (secret, me), op, deadline, resolver).map_err(|failed| failed.why)
}

fn reach_node_pass(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, &NodeIdentity),
    op: FleetOp,
    deadline: (Instant, Duration),
    resolver: impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
) -> Result<(Reached, String), PassFailure> {
    walk_target_pass(target, keys, deadline, resolver, |to| {
        reach(to, (secret, Some(me)), op, deadline, &|_, _| None).and_then(|reached| {
            let at = reached.addr;
            match answered_as(target, keys, at, reached.peer.as_ref())? {
                Some((node, true)) => Ok((reached, node)),
                claimed => Err(Unreached::Unbound {
                    at,
                    node: claimed.map(|(node, _)| node),
                }),
            }
        })
    })
}

/// Try every address of one target once under its original deadline. An identity rejected at
/// one address leaves the rest available; if none reaches the target, the first such error
/// stands. A refusal proved by the bound target itself is authoritative and ends this pass.
fn walk_target<R>(
    target: &Target,
    keys: &Keyring,
    deadline: (Instant, Duration),
    resolver: impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
    attempt: impl FnMut(SocketAddr) -> AtAddr<R>,
) -> Result<R, Unreached> {
    walk_target_pass(target, keys, deadline, resolver, attempt).map_err(|failed| failed.why)
}

fn walk_target_pass<R>(
    target: &Target,
    keys: &Keyring,
    deadline: (Instant, Duration),
    resolver: impl Fn(&str, u16) -> Vec<SocketAddr> + Clone + Send + 'static,
    mut attempt: impl FnMut(SocketAddr) -> AtAddr<R>,
) -> Result<R, PassFailure> {
    let mut rejected = false;
    walk_pass(
        (&target.host, target.addr, target.port),
        deadline,
        (&[], resolver),
        |to| {
            let tried = match attempt(to) {
                AtAddr::Refused(refused) => {
                    match answered_as(target, keys, to, Some(&refused.peer)) {
                        Ok(Some((_, true))) => return AtAddr::Ended(refused.why),
                        Err(why) => AtAddr::Ended(why),
                        Ok(_) => AtAddr::Refused(refused),
                    }
                }
                tried => tried,
            };
            match tried {
                AtAddr::Ended(
                    why @ (Unreached::OtherNode { .. }
                    | Unreached::Impostor { .. }
                    | Unreached::Misnamed { .. }
                    | Unreached::Unproved { .. }
                    | Unreached::Unbound { .. }),
                ) => {
                    rejected = true;
                    AtAddr::Passed(why)
                }
                AtAddr::Ended(why @ Unreached::Deadline(..)) if rejected => AtAddr::Passed(why),
                tried => tried,
            }
        },
    )
}

fn driven(
    reached: Reached,
    node: String,
    (deadline, within): (Instant, Duration),
) -> Result<Driven, Unreached> {
    let at = reached.addr;
    let key = reached
        .peer
        .as_ref()
        .filter(|proof| proof.node == node)
        .map(|proof| proof.key)
        .ok_or_else(|| Unreached::Local("drive lost its authenticated peer proof".into()))?;
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Unreached::Deadline(Stage::Answer, within));
    }
    let stream = reached.transport.into_stream();
    let tcp = stream.get_ref();
    let bounded = tcp
        .set_read_timeout(Some(left))
        .and_then(|()| tcp.set_write_timeout(Some(left)))
        .and_then(|()| HandshakeWatchdog::arm(tcp, left));
    let watchdog = bounded
        .map_err(|e| Unreached::Local(format!("could not bound the drive's exchange: {e}")))?;
    Ok(Driven {
        stream,
        watchdog: Some(watchdog),
        addr: at,
        node,
        key,
    })
}

fn ask_at(
    target: &Target,
    keys: &Keyring,
    to: SocketAddr,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    (deadline, within): (Instant, Duration),
    request: &str,
    held: &HeldAddrs,
) -> AtAddr<Surveyed> {
    held.ask(
        to,
        (deadline, within),
        || {
            reach(
                to,
                (secret, me),
                FleetOp::Survey,
                (deadline, within),
                &|_, _| None,
            )
        },
        |peer| answered_as(target, keys, to, peer),
        |transport| {
            ask_lines(transport, Some(request), deadline, reply_head).map(|(_, lines)| lines)
        },
    )
    .and_then(
        |Answer {
             scope,
             addr,
             peer,
             answered,
             lines,
         }| {
            let lines = lines.map_err(|why| match why {
                Asked::Deadline => Unreached::Deadline(Stage::Answer, within),
                Asked::Failed(why) => Unreached::Reply(addr, why),
                Asked::Local(why) => Unreached::Local(why),
            })?;
            let (node, proven) =
                reply_node(answered, (&lines, addr), keys, proves_me(me, peer.as_ref()))?;
            if let (Some(want), Some(got)) = (target.node.as_deref(), node.as_deref())
                && want != got
            {
                return Err(Unreached::OtherNode {
                    at: addr,
                    said: got.to_owned(),
                    want: want.to_owned(),
                });
            }
            Ok(Surveyed {
                scope,
                addr,
                node,
                proven,
                key: peer.map(|proof| proof.key),
                lines,
            })
        },
    )
}

fn reply_node(
    answered: Option<(String, bool)>,
    (lines, at): (&[String], SocketAddr),
    keys: &Keyring,
    mine: bool,
) -> Result<(Option<String>, bool), Unreached> {
    match answered {
        Some((node, proven)) => {
            // A peer that proved a node speaks for that node alone: a header anywhere in its
            // reply naming another contradicts its proof, and nothing it sent is read.
            if let Some(claimed) = header_nodes(lines).find(|claimed| *claimed != node) {
                return Err(Unreached::Misnamed {
                    at,
                    proved: node,
                    claimed: aterm_codec::tty::one_line(&claimed, 32),
                });
            }
            Ok((Some(node), proven || mine))
        }
        None => {
            // A header naming a node bound to a key, from a peer that proved none, is that
            // node's name without its key.
            if let Some(said) = header_nodes(lines).find(|said| keys.bound(said).is_some()) {
                return Err(Unreached::Unproved { at, node: said });
            }
            Ok((header_nodes(lines).find(|said| is_node_id(said)), false))
        }
    }
}

fn judged_again(
    target: &Target,
    keys: &Keyring,
    me: Option<&NodeIdentity>,
    surveyed: Surveyed,
) -> Result<Surveyed, Unreached> {
    let proof = surveyed
        .key
        .zip(surveyed.node.clone())
        .map(|(key, node)| NodeProof { node, key });
    let answered = answered_as(target, keys, surveyed.addr, proof.as_ref())?;
    let (node, proven) = reply_node(
        answered,
        (&surveyed.lines, surveyed.addr),
        keys,
        proves_me(me, proof.as_ref()),
    )?;
    Ok(Surveyed {
        node,
        proven,
        ..surveyed
    })
}

fn proves_me(me: Option<&NodeIdentity>, proof: Option<&NodeProof>) -> bool {
    me.zip(proof)
        .is_some_and(|(me, proof)| proof.node == me.node() && proof.key == me.key())
}

fn answered_as(
    target: &Target,
    keys: &Keyring,
    at: SocketAddr,
    proof: Option<&NodeProof>,
) -> Result<Option<(String, bool)>, Unreached> {
    let bound = |node: &str| {
        target
            .key
            .filter(|_| target.node.as_deref() == Some(node))
            .or_else(|| keys.bound(node))
    };
    let Some(proof) = proof else {
        return match target.node.as_deref() {
            Some(want) if bound(want).is_some() => Err(Unreached::Unproved {
                at,
                node: want.to_owned(),
            }),
            _ => Ok(None),
        };
    };
    if let Some(want) = target.node.as_deref()
        && want != proof.node
    {
        return Err(Unreached::OtherNode {
            at,
            said: proof.node.clone(),
            want: want.to_owned(),
        });
    }
    match bound(&proof.node) {
        Some(key) if key == proof.key => Ok(Some((proof.node.clone(), true))),
        Some(_) => Err(Unreached::Impostor {
            at,
            node: proof.node.clone(),
            issuer: keys.is_issuer(&proof.node),
        }),
        None => Ok(Some((proof.node.clone(), false))),
    }
}

fn header_nodes(lines: &[String]) -> impl Iterator<Item = String> + '_ {
    lines
        .iter()
        .filter(|line| line.starts_with("instance "))
        .flat_map(|line| line.split(' ').skip(1))
        .filter_map(|field| field.split_once('='))
        .filter(|(key, _)| *key == "node")
        .map(|(_, value)| aterm_codec::pct::decode(value))
        .filter(|value| !value.is_empty() && value != "-")
}

#[derive(Clone)]
enum Asked {
    Deadline,
    Failed(String),
    Local(String),
}

type Head = fn(&str) -> Result<usize, String>;

fn ask_lines(
    transport: &mut TlsTransport<ClientConnection>,
    request: Option<&str>,
    deadline: Instant,
    head: Head,
) -> Result<(String, Vec<String>), Asked> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Asked::Deadline);
    }
    let watchdog = HandshakeWatchdog::arm(transport.stream().get_ref(), left)
        .map_err(|e| Asked::Local(format!("could not bound the reply's read: {e}")))?;
    let result = exchange_lines(transport, request, deadline, head);
    match (watchdog.finish(), result) {
        (_, Ok(lines)) => Ok(lines),
        (Claim::Authed, Err(why)) => Err(why),
        (_, Err(_)) => Err(Asked::Deadline),
    }
}

fn exchange_lines(
    transport: &mut TlsTransport<ClientConnection>,
    request: Option<&str>,
    deadline: Instant,
    head: Head,
) -> Result<(String, Vec<String>), Asked> {
    let left = |deadline: Instant| {
        let left = deadline.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left).ok_or(Asked::Deadline)
    };
    let timed_out = |e: &io::Error| {
        matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        )
    };
    if let Some(request) = request {
        let tcp_left = left(deadline)?;
        transport
            .stream()
            .get_ref()
            .set_write_timeout(Some(tcp_left))
            .map_err(|e| Asked::Failed(format!("could not be asked: {e}")))?;
        if let Err(e) = transport
            .stream()
            .write_all(format!("{request}\n").as_bytes())
            .and_then(|()| transport.stream().flush())
        {
            return Err(if timed_out(&e) {
                Asked::Deadline
            } else {
                Asked::Failed(broke_off(false, &e))
            });
        }
    }
    let mut pending: Vec<u8> = Vec::new();
    let mut read = 0usize;
    let mut want: Option<(String, usize)> = None;
    let mut lines: Vec<String> = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if read > REPLY_MAX {
            return Err(Asked::Failed(format!(
                "answered past {} KiB: refused",
                REPLY_MAX / 1024
            )));
        }
        while let Some(nl) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line[..nl]).into_owned();
            match want {
                Some(_) => lines.push(line),
                None => {
                    let n = head(&line).map_err(Asked::Failed)?;
                    want = Some((line, n));
                }
            }
            if let Some((head, n)) = &want
                && *n == lines.len()
            {
                return Ok((head.clone(), lines));
            }
        }
        if let Some((head, n)) = &want
            && *n == lines.len()
        {
            return Ok((head.clone(), lines));
        }
        let tcp_left = left(deadline)?;
        transport
            .stream()
            .get_ref()
            .set_read_timeout(Some(tcp_left))
            .map_err(|e| Asked::Failed(format!("could not be read: {e}")))?;
        match transport.stream().read(&mut chunk) {
            Ok(0) => {
                return Err(Asked::Failed(broke_off(
                    true,
                    &io::ErrorKind::UnexpectedEof.into(),
                )));
            }
            Ok(n) => {
                read += n;
                pending.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if timed_out(&e) => return Err(Asked::Deadline),
            Err(e) => return Err(Asked::Failed(broke_off(true, &e))),
        }
    }
}

/// Why a proved peer's exchange broke off on `e`, before it was asked (`answering` false) or
/// while it answered: the words after its address ([`Unreached::Reply`]), from the error's
/// kind, never its text.
#[must_use]
pub fn broke_off(answering: bool, e: &io::Error) -> String {
    match (answering, e.kind()) {
        (false, kind) => format!("closed before it was asked: {kind}"),
        (true, io::ErrorKind::UnexpectedEof) => {
            "closed the connection before its answer ended".to_owned()
        }
        (true, kind) => format!("cut its answer short: {kind}"),
    }
}

const REPLY_SHOWN_CHARS: usize = 60;

/// How many lines a Lines reply's head `OK <n>` promises.
pub fn reply_head(line: &str) -> Result<usize, String> {
    if let Some(n) = line
        .strip_prefix("OK ")
        .and_then(|rest| rest.split(' ').next())
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse().ok())
    {
        return Ok(n);
    }
    let shown = aterm_codec::tty::one_line(line, REPLY_SHOWN_CHARS);
    Err(if line.starts_with("ERR denied: the fleet serves only") {
        "serves the fleet no survey: an older aterm (update it)".to_owned()
    } else if line.starts_with("ERR ") {
        format!("refused the survey: {shown}")
    } else {
        format!("answered no survey: {shown}")
    })
}

/// Subdirectory under the state directory where a lease holder stores pushed C16 survey facts.
pub const SWARM_FACTS_DIR: &str = "swarm-facts";
const MAX_PUSHED_FACTS_FILES: usize = 256;
const MAX_PUSHED_FACTS_LINES: usize = 4096;
const PUSHED_FACTS_MAGIC: &str = "aterm-pushed-facts-v1\n";
const PUSHED_FACTS_MAX: usize = PUSHED_FACTS_MAGIC.len() + 65 + REPLY_MAX;
const PUSHED_FACTS_LOCK: &str = ".write.lock";

/// One atomic publication: its proved key and the payload whose receipt proved it.
struct PushedFacts {
    key: Option<NodeKey>,
    payload: String,
    received_at: Option<std::time::SystemTime>,
}

/// Both formats name nodes; no metadata read follows an entry to find its name.
fn pushed_facts_nodes(dir: &Path) -> io::Result<std::collections::BTreeSet<String>> {
    Ok(std::fs::read_dir(dir)?
        .filter_map(|entry| {
            let file = entry.ok()?.file_name().into_string().ok()?;
            file.strip_suffix(".facts")
                .or_else(|| file.strip_suffix(".txt"))
                .filter(|node| is_node_id(node))
                .map(str::to_owned)
        })
        .collect())
}

/// New records are authoritative, including a refused read: only absence permits legacy text.
fn pushed_facts_at(
    judged: &aterm_uds::private_file::JudgedDir<'_>,
    node: &str,
) -> Option<PushedFacts> {
    match judged.read_with_metadata(&format!("{node}.facts"), PUSHED_FACTS_MAX) {
        Ok((bytes, metadata)) => {
            let text = String::from_utf8(bytes).ok()?;
            let (key, payload) = text.strip_prefix(PUSHED_FACTS_MAGIC)?.split_once('\n')?;
            let key = if key == "-" {
                None
            } else {
                Some(NodeKey::from_hex(key)?)
            };
            (payload.len() <= REPLY_MAX).then(|| PushedFacts {
                key,
                payload: payload.to_owned(),
                received_at: metadata.modified().ok(),
            })
        }
        Err(PrivateFileError::Missing) => {
            let (bytes, metadata) = judged
                .read_with_metadata(&format!("{node}.txt"), REPLY_MAX)
                .ok()?;
            let payload = String::from_utf8(bytes).ok()?;
            let key = judged
                .read(&format!("{node}.key"), 128)
                .ok()
                .and_then(|raw| String::from_utf8(raw).ok())
                .and_then(|raw| NodeKey::from_hex(raw.trim()));
            Some(PushedFacts {
                key,
                payload,
                received_at: metadata.modified().ok(),
            })
        }
        Err(_) => None,
    }
}

/// Unlink names, never their targets. Current writers and pruning hold the same stable lock.
fn remove_pushed_facts(dir: &Path, node: &str) {
    remove_pushed_facts_using(dir, node, |_| {});
}

fn remove_pushed_facts_using(dir: &Path, node: &str, mut removed: impl FnMut(&str)) {
    // Keep the authoritative record until its legacy payload cannot reappear. If unlinking
    // that payload fails, retain the record; admitted reads still apply its current key.
    for extension in ["txt", "key", "facts"] {
        match std::fs::remove_file(dir.join(format!("{node}.{extension}"))) {
            Err(error) if extension == "txt" && error.kind() != io::ErrorKind::NotFound => return,
            _ => {}
        }
        removed(extension);
    }
}

fn pushed_facts_lock(
    dir: &Path,
    within: Duration,
    held: impl FnMut(),
) -> io::Result<aterm_uds::private_file::PrivateLock> {
    aterm_uds::private_file::PrivateLock::take_with(
        dir,
        PUSHED_FACTS_LOCK,
        within,
        "another survey facts writer holds the cache",
        held,
    )
}

/// Store validated C16 `survey facts` wire text for `node` under `<state_dir>/swarm-facts/`.
///
/// # Errors
/// As [`store_pushed_facts_with_key`]: invalid node, oversized payload, unavailable cache lock,
/// or a write error (including a directory flush after publication).
pub fn store_pushed_facts(state_dir: &Path, node: &str, payload: &str) -> io::Result<()> {
    store_pushed_facts_with_key(state_dir, node, None, payload)
}

/// Store validated C16 `survey facts` wire text and its optional proved [`NodeKey`] as one
/// private atomic `<node>.facts` record, so pruning always reads the key of that payload.
/// Legacy `<node>.txt` and `<node>.key` remain readable only when no new record exists.
/// Older binaries do not read new records; their later legacy writes cannot replace one.
///
/// # Errors
/// Returns an I/O or validation error when `node` is not a 32-hex node id, the payload exceeds
/// [`REPLY_MAX`], the cache lock is unavailable, or the write fails. A directory flush failure
/// after the atomic rename may leave the complete new record published, with durability unknown.
pub fn store_pushed_facts_with_key(
    state_dir: &Path,
    node: &str,
    key: Option<NodeKey>,
    payload: &str,
) -> io::Result<()> {
    store_pushed_facts_using(
        state_dir,
        node,
        key,
        payload,
        Duration::from_secs(2),
        || {},
        write_private,
    )
}

fn store_pushed_facts_using(
    state_dir: &Path,
    node: &str,
    key: Option<NodeKey>,
    payload: &str,
    within: Duration,
    held: impl FnMut(),
    publish: impl FnOnce(&Path, &str, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    if !is_node_id(node) || payload.len() > REPLY_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "node id must be 32 lowercase hex digits and survey facts must fit the reply limit",
        ));
    }
    let dir = state_dir.join(SWARM_FACTS_DIR);
    ensure_private_dir(&dir)?;
    let _lock = pushed_facts_lock(&dir, within, held)?;
    let key = key.map_or_else(|| "-".to_owned(), |key| key.hex());
    let record = format!("{PUSHED_FACTS_MAGIC}{key}\n{payload}");
    publish(&dir, &format!("{node}.facts"), record.as_bytes())?;

    // Count nodes once across both layouts. Evict only after publication succeeded; a refused
    // write must not discard other nodes' previous facts to make room for nothing.
    if let Ok(nodes) = pushed_facts_nodes(&dir) {
        let drop_count = nodes.len().saturating_sub(MAX_PUSHED_FACTS_FILES);
        let mut old: Vec<_> = nodes
            .into_iter()
            .filter(|candidate| candidate != node)
            .map(|candidate| {
                let path = dir.join(format!("{candidate}.facts"));
                let metadata = std::fs::symlink_metadata(path)
                    .or_else(|_| std::fs::symlink_metadata(dir.join(format!("{candidate}.txt"))));
                let modified = metadata
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                (modified, candidate)
            })
            .collect();
        old.sort();
        for (_, node) in old.into_iter().take(drop_count) {
            remove_pushed_facts(&dir, &node);
        }
    }
    Ok(())
}

/// Remove new records and legacy `<node>.txt` / `<node>.key` files belonging to
/// `denied_nodes`, to `peers` whose node is in `denied_nodes` or whose key `log` holds out, or
/// whose recorded key / [`Keyring`] binding is in `denied_nodes` or held out by `log`. A log that
/// cannot be read now ([`MemberKeys::unreadable`]) removes nothing: it judges no key, and
/// [`read_admitted_pushed_facts`] admits none under it. Cleanup is best effort, including when
/// the cache lock is unavailable; admission must use [`read_admitted_pushed_facts`].
pub fn prune_pushed_facts(
    state_dir: &Path,
    peers: &[Peer],
    denied_nodes: &[String],
    log: Option<&MemberKeys>,
) {
    prune_pushed_facts_using(
        state_dir,
        peers,
        denied_nodes,
        log,
        Duration::from_secs(2),
        || {},
    );
}

fn prune_pushed_facts_using(
    state_dir: &Path,
    peers: &[Peer],
    denied_nodes: &[String],
    log: Option<&MemberKeys>,
    within: Duration,
    held: impl FnMut(),
) {
    let log = log.filter(|log| log.unread().is_none());
    let dir = state_dir.join(SWARM_FACTS_DIR);
    let Ok(_lock) = pushed_facts_lock(&dir, within, held) else {
        return;
    };
    let remove_node_facts = |node: &str| {
        remove_pushed_facts(&dir, node);
    };
    for denied in denied_nodes {
        let node = denied.trim_ascii().to_ascii_lowercase();
        if is_node_id(&node) {
            remove_node_facts(&node);
        }
    }
    for peer in peers {
        let is_denied = peer
            .node
            .as_deref()
            .is_some_and(|n| crate::fleet::is_denied_node(n, denied_nodes))
            || peer
                .key
                .as_ref()
                .is_some_and(|k| crate::fleet::is_denied_key(k, denied_nodes));
        let is_held_out = peer
            .key
            .is_some_and(|k| log.is_some_and(|l| l.holds_out(&k)));
        if (is_denied || is_held_out)
            && let Some(node) = peer.node.as_deref()
            && is_node_id(node)
        {
            remove_node_facts(node);
        }
    }
    let keyring = Keyring::read(state_dir);
    let Ok(nodes) = pushed_facts_nodes(&dir) else {
        return;
    };
    let judged = aterm_uds::private_file::JudgedDir::judge(&dir).ok();
    for node in nodes {
        if crate::fleet::is_denied_node(&node, denied_nodes) {
            remove_node_facts(&node);
            continue;
        }
        let Some(facts) = judged
            .as_ref()
            .and_then(|judged| pushed_facts_at(judged, &node))
        else {
            continue;
        };
        let effective_key = facts.key.or_else(|| keyring.bound(&node));
        if let Some(key) = effective_key
            && (crate::fleet::is_denied_key(&key, denied_nodes)
                || log.is_some_and(|l| l.holds_out(&key)))
        {
            remove_node_facts(&node);
        }
    }
}

/// Read private C16 survey-facts payloads, each within [`REPLY_MAX`], from atomic records or,
/// only when absent, legacy text. Non-regular, linked, non-private, oversized, malformed or
/// non-UTF8 records are skipped without falling back to older facts.
#[must_use]
pub fn read_pushed_facts(state_dir: &Path) -> Vec<(String, String)> {
    let dir = state_dir.join(SWARM_FACTS_DIR);
    let Ok(nodes) = pushed_facts_nodes(&dir) else {
        return Vec::new();
    };
    let Ok(judged) = aterm_uds::private_file::JudgedDir::judge(&dir) else {
        return Vec::new();
    };
    nodes
        .into_iter()
        .filter_map(|node| Some((node.clone(), pushed_facts_at(&judged, &node)?.payload)))
        .collect()
}

/// One admitted cached survey, with the receive time of the same file that supplied its bytes.
#[derive(Debug)]
pub struct PushedFactsRecord {
    /// The validated 32-hex node name.
    pub node: String,
    /// Unmodified C16 survey-facts wire text.
    pub payload: String,
    /// Modification time from the opened authoritative record or legacy payload file.
    pub received_at: Option<std::time::SystemTime>,
}

/// Read cached surveys admitted by the current node, peer-key and membership-log standings.
/// Pruning is best-effort cleanup; a busy cache lock never admits a denied cached publication.
/// Missing proof keys retain the established [`Keyring`] fallback, as legacy facts did.
#[must_use]
pub fn read_admitted_pushed_facts(
    state_dir: &Path,
    peers: &[Peer],
    denied_nodes: &[String],
    log: Option<&MemberKeys>,
) -> Vec<PushedFactsRecord> {
    let dir = state_dir.join(SWARM_FACTS_DIR);
    let Ok(nodes) = pushed_facts_nodes(&dir) else {
        return Vec::new();
    };
    let Ok(judged) = aterm_uds::private_file::JudgedDir::judge(&dir) else {
        return Vec::new();
    };
    let keyring = Keyring::read(state_dir);
    let key_denied = |key: &NodeKey| {
        crate::fleet::is_denied_key(key, denied_nodes) || log.is_some_and(|log| log.holds_out(key))
    };
    nodes
        .into_iter()
        .filter_map(|node| {
            if crate::fleet::is_denied_node(&node, denied_nodes)
                || peers.iter().any(|peer| {
                    peer.node.as_deref() == Some(&node) && peer.key.as_ref().is_some_and(key_denied)
                })
            {
                return None;
            }
            let facts = pushed_facts_at(&judged, &node)?;
            if facts
                .key
                .or_else(|| keyring.bound(&node))
                .as_ref()
                .is_some_and(key_denied)
            {
                return None;
            }
            Some(PushedFactsRecord {
                node,
                payload: facts.payload,
                received_at: facts.received_at,
            })
        })
        .collect()
}

/// Listener handler for [`FleetOp::Facts`]: reads a line-framed `OK <n>` C16 survey-facts push,
/// checks that the connection's proved [`NodeProof`] matches `validate`'s parsed node id, stores
/// it with that proved key as an atomic record under `<state_dir>/swarm-facts/`, and answers `OK\n`.
///
/// # Errors
/// Returns an I/O error if the stream cannot be read or written.
pub fn serve_facts<S: Read + Write>(
    stream: &mut S,
    proof: Option<&NodeProof>,
    state_dir: &Path,
    validate: impl FnOnce(&str) -> Result<Option<String>, String>,
) -> io::Result<Result<String, String>> {
    serve_facts_with_denied(stream, proof, state_dir, (&[], None), validate)
}

/// [`serve_facts`], also refusing any node on `denied_nodes`, any key `log` holds out, or any
/// node id that `state_dir` (its own identity or [`Keyring::read`]) binds to a different key.
///
/// # Errors
/// Returns an I/O error if the stream cannot be read or written.
pub fn serve_facts_with_denied<S: Read + Write>(
    stream: &mut S,
    proof: Option<&NodeProof>,
    state_dir: &Path,
    (denied_nodes, log): (&[String], Option<&MemberKeys>),
    validate: impl FnOnce(&str) -> Result<Option<String>, String>,
) -> io::Result<Result<String, String>> {
    let Some(proof) = proof else {
        stream.write_all(b"ERR unproved: facts push requires a proved node key\n")?;
        stream.flush()?;
        return Ok(Err("facts push without a proved node key".to_owned()));
    };
    let mut reader =
        io::BufReader::new(Read::by_ref(stream).take(crate::fleet::REPLY_MAX as u64 + 1));
    let mut head = String::new();
    if reader.read_line(&mut head)? == 0 {
        return Ok(Err("closed before sending facts head".to_owned()));
    }
    if !head.ends_with('\n') {
        drop(reader);
        stream.write_all(b"ERR invalid: facts push begins with OK <count>\n")?;
        stream.flush()?;
        return Ok(Err("invalid facts head".to_owned()));
    }
    let trimmed = head.trim_end_matches(['\r', '\n']);
    let Some(count) = head
        .ends_with('\n')
        .then(|| trimmed.strip_prefix("OK "))
        .flatten()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| (1..=MAX_PUSHED_FACTS_LINES).contains(&n))
    else {
        drop(reader);
        stream.write_all(b"ERR invalid: facts push begins with OK <count>\n")?;
        stream.flush()?;
        return Ok(Err("invalid facts head".to_owned()));
    };
    let mut payload = head;
    for _ in 0..count {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            drop(reader);
            stream.write_all(b"ERR truncated: facts push ended before count\n")?;
            stream.flush()?;
            return Ok(Err("truncated facts push".to_owned()));
        }
        payload.push_str(&line);
        if payload.len() > crate::fleet::REPLY_MAX {
            drop(reader);
            stream.write_all(b"ERR oversize: facts push exceeds REPLY_MAX\n")?;
            stream.flush()?;
            return Ok(Err("oversize facts push".to_owned()));
        }
        if !line.ends_with('\n') {
            drop(reader);
            stream.write_all(b"ERR truncated: facts push ended before count\n")?;
            stream.flush()?;
            return Ok(Err("truncated facts push".to_owned()));
        }
    }
    drop(reader);
    if crate::fleet::is_denied_node(&proof.node, denied_nodes)
        || crate::fleet::is_denied_key(&proof.key, denied_nodes)
    {
        stream.write_all(b"ERR denied: facts node is on the swarm roster's deny-list\n")?;
        stream.flush()?;
        return Ok(Err(
            "facts node is on the swarm roster's deny-list".to_owned()
        ));
    }
    if log.is_some_and(|l| l.holds_out(&proof.key)) {
        stream.write_all(b"ERR denied: facts node key was taken out by the membership log\n")?;
        stream.flush()?;
        return Ok(Err(
            "facts node key was taken out by the membership log".to_owned()
        ));
    }
    let own_key_mismatch = match read_node_identity(state_dir).ok().flatten() {
        Some(own) => own.node() == proof.node && own.key() != proof.key,
        None => read_node_id(state_dir)
            .ok()
            .flatten()
            .is_some_and(|own_node| own_node == proof.node),
    };
    let keyring_mismatch = Keyring::read(state_dir)
        .bound(&proof.node)
        .is_some_and(|bound| {
            bound != proof.key
                && !log.is_some_and(|l| {
                    (l.holds_out(&bound) || crate::fleet::is_denied_key(&bound, denied_nodes))
                        && l.admits(&proof.key)
                })
        });
    if own_key_mismatch || keyring_mismatch {
        stream.write_all(b"ERR key-mismatch: facts node is bound to another key\n")?;
        stream.flush()?;
        return Ok(Err("facts node is bound to another key".to_owned()));
    }
    match validate(&payload) {
        Ok(Some(claimed)) if claimed == proof.node => {
            if let Err(e) =
                store_pushed_facts_with_key(state_dir, &proof.node, Some(proof.key), &payload)
            {
                stream.write_all(b"ERR store: could not persist facts\n")?;
                stream.flush()?;
                return Ok(Err(format!("storing facts: {e}")));
            }
            stream.write_all(b"OK\n")?;
            stream.flush()?;
            Ok(Ok(proof.node.clone()))
        }
        Ok(_) => {
            stream.write_all(b"ERR node-mismatch: facts node does not match proved key\n")?;
            stream.flush()?;
            Ok(Err(
                "facts node= does not match connection's proved node key".to_owned(),
            ))
        }
        Err(why) => {
            let msg = format!(
                "ERR malformed: {}\n",
                aterm_codec::tty::one_line(&why, REPLY_SHOWN_CHARS)
            );
            stream.write_all(msg.as_bytes())?;
            stream.flush()?;
            Ok(Err(why))
        }
    }
}

fn verify_bound_peer_proof(
    peer: &Peer,
    proof: Option<&NodeProof>,
    (denied_nodes, log): (&[String], Option<&MemberKeys>),
) -> Result<(), String> {
    if peer
        .node
        .as_deref()
        .is_some_and(|node| crate::fleet::is_denied_node(node, denied_nodes))
    {
        return Err("peer is on the swarm roster's deny-list".to_owned());
    }
    if let Some(proof) = proof {
        if crate::fleet::is_denied_node(&proof.node, denied_nodes)
            || crate::fleet::is_denied_key(&proof.key, denied_nodes)
        {
            return Err("peer is on the swarm roster's deny-list".to_owned());
        }
        if log.is_some_and(|l| l.holds_out(&proof.key)) {
            return Err("peer node key was taken out by the membership log".to_owned());
        }
        if let Some(expected_node) = peer.node.as_deref()
            && !proof.node.eq_ignore_ascii_case(expected_node)
        {
            return Err("peer proved a different node id than expected".to_owned());
        }
        if let Some(expected_key) = peer.key
            && proof.key != expected_key
            && !log.is_some_and(|l| {
                (l.holds_out(&expected_key)
                    || crate::fleet::is_denied_key(&expected_key, denied_nodes))
                    && l.admits(&proof.key)
            })
        {
            return Err("peer proved a different node key than expected".to_owned());
        }
    } else if peer.key.is_some() {
        return Err("peer did not prove its bound node key".to_owned());
    }
    Ok(())
}

/// Dial `peer` on [`FleetOp::Facts`], verify its [`NodeProof`] against `peer.node`, `peer.key`,
/// `denied_nodes`, and `log`, and push `payload` (`OK <count>\n…`) within `within`. Under a log
/// this Mac cannot read now ([`MemberKeys::unreadable`]) it dials no one: that log admits no peer
/// to take them.
///
/// # Errors
/// Returns why the log cannot be read now, or why the peer could not be dialled, failed
/// node-proof verification, or refused the pushed facts.
pub fn push_facts_to_bound_peer(
    peer: &Peer,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    (denied_nodes, log): (&[String], Option<&MemberKeys>),
    payload: &str,
    within: Duration,
) -> Result<(), String> {
    if let Some(why) = log.and_then(MemberKeys::unread) {
        return Err(why.to_owned());
    }
    let deadline = deadline_after(within);
    let mut reached = dial(
        &peer.host,
        peer.addr,
        peer.port,
        (secret, me),
        FleetOp::Facts,
        within,
    )
    .map_err(|e| e.to_string())?;
    verify_bound_peer_proof(peer, reached.peer.as_ref(), (denied_nodes, log))?;
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err("facts push ran out of deadline after handshake".to_owned());
    }
    let tcp = reached.transport.stream().get_ref();
    tcp.set_read_timeout(Some(left))
        .and_then(|()| tcp.set_write_timeout(Some(left)))
        .map_err(|e| e.to_string())?;
    let watchdog = HandshakeWatchdog::arm(tcp, left).map_err(|e| e.to_string())?;
    let exchange = (|| -> Result<(), String> {
        let stream = reached.transport.stream();
        stream
            .write_all(payload.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| e.to_string())?;
        let mut reader = io::BufReader::new(stream.take(512));
        let mut reply = String::new();
        let n = reader.read_line(&mut reply).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("peer closed without answering facts push".to_owned());
        }
        if reply.len() > 508 || !reply.ends_with('\n') {
            return Err("oversize or unterminated facts push reply".to_owned());
        }
        let trimmed = reply.trim_end_matches(['\r', '\n']);
        if trimmed == "OK" {
            Ok(())
        } else {
            Err(format!("peer refused facts push: {trimmed}"))
        }
    })();
    match (watchdog.finish(), exchange) {
        (_, Ok(())) => Ok(()),
        (Claim::Authed, Err(why)) => Err(why),
        (_, Err(_)) => Err("facts push ran out of deadline".to_owned()),
    }
}

/// Dial a peer on [`FleetOp::Facts`] and push `payload` (`OK <count>\n…`) within `within`.
///
/// # Errors
/// Returns why the peer could not be dialled or why it refused the pushed facts.
pub fn push_facts_to_peer(
    host: &str,
    addr: Option<IpAddr>,
    port: u16,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    payload: &str,
    within: Duration,
) -> Result<(), String> {
    let peer = Peer {
        node: None,
        key: None,
        key_from: None,
        host: host.to_owned(),
        addr,
        port,
        version: None,
        seen_ms: None,
        via: Via::Issuer,
        fleet: None,
        signed: None,
    };
    push_facts_to_bound_peer(&peer, (secret, me), (&[], None), payload, within)
}

const MAX_MAIL_CODE_REQUEST: u64 = 1024;
const MAX_MAIL_CODE_REPLY: u64 = 16 * 1024;

/// Listener handler for [`FleetOp::MailCode`]: reads a single bounded `mail-code <email> …` line,
/// requires a proved [`NodeProof`], delegates to `answer(proof, line)`, and writes either
/// `OK <sealed_b64>\n` or `ERR <refusal>\n`.
///
/// # Errors
/// Returns an I/O error if reading or writing `stream` fails.
pub fn serve_mail_code<S: Read + Write>(
    stream: &mut S,
    proof: Option<&NodeProof>,
    answer: impl FnOnce(&NodeProof, &str) -> Result<String, String>,
) -> io::Result<Result<String, String>> {
    let Some(proof) = proof else {
        stream.write_all(b"ERR unproved: mail-code request requires a proved node key\n")?;
        stream.flush()?;
        return Ok(Err("mail-code request without a proved node key".to_owned()));
    };
    let mut reader = io::BufReader::new(Read::by_ref(stream).take(MAX_MAIL_CODE_REQUEST + 1));
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(Err("closed before sending mail-code request".to_owned()));
    }
    drop(reader);
    if line.len() as u64 > MAX_MAIL_CODE_REQUEST || !line.ends_with('\n') {
        stream.write_all(b"ERR oversize: mail-code request exceeds line bound\n")?;
        stream.flush()?;
        return Ok(Err("oversize or unterminated mail-code request".to_owned()));
    }
    let trimmed = line.trim_end_matches(['\r', '\n']);
    match answer(proof, trimmed) {
        Ok(sealed) => {
            if sealed.is_empty()
                || sealed.len() as u64 > MAX_MAIL_CODE_REPLY
                || sealed.bytes().any(|b| matches!(b, b'\r' | b'\n'))
            {
                stream.write_all(b"ERR internal: invalid sealed mail-code payload\n")?;
                stream.flush()?;
                return Ok(Err("invalid sealed mail-code payload".to_owned()));
            }
            let out = format!("OK {sealed}\n");
            stream.write_all(out.as_bytes())?;
            stream.flush()?;
            Ok(Ok(sealed))
        }
        Err(refusal) => {
            let clean = aterm_codec::tty::one_line(&refusal, REPLY_SHOWN_CHARS);
            let out = format!("ERR {clean}\n");
            stream.write_all(out.as_bytes())?;
            stream.flush()?;
            Ok(Err(clean))
        }
    }
}

/// Dial `peer` on [`FleetOp::MailCode`], verify its [`NodeProof`] against `peer.node`, `peer.key`,
/// `denied_nodes`, and `log`, and exchange one `mail-code <email> …` line.
///
/// # Errors
/// Returns an error string when the peer cannot be dialled, fails node-proof verification, or
/// closes without a framed `OK`/`ERR` reply. An explicit `ERR …` from the peer returns
/// `Ok(Err(reason))`.
pub fn request_mail_code_from_bound_peer(
    peer: &Peer,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    (denied_nodes, log): (&[String], Option<&MemberKeys>),
    request_line: &str,
    within: Duration,
) -> Result<Result<String, String>, String> {
    let deadline = deadline_after(within);
    let mut reached = dial(
        &peer.host,
        peer.addr,
        peer.port,
        (secret, me),
        FleetOp::MailCode,
        within,
    )
    .map_err(|e| e.to_string())?;
    verify_bound_peer_proof(peer, reached.peer.as_ref(), (denied_nodes, log))?;
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err("mail-code request ran out of deadline after handshake".to_owned());
    }
    let tcp = reached.transport.stream().get_ref();
    tcp.set_read_timeout(Some(left))
        .and_then(|()| tcp.set_write_timeout(Some(left)))
        .map_err(|e| e.to_string())?;
    let watchdog = HandshakeWatchdog::arm(tcp, left).map_err(|e| e.to_string())?;
    let clean = request_line.trim_end_matches(['\r', '\n']);
    let framed = format!("{clean}\n");
    let exchange = (|| -> Result<Result<String, String>, String> {
        let stream = reached.transport.stream();
        stream
            .write_all(framed.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| e.to_string())?;
        let mut reader = io::BufReader::new(stream.take(MAX_MAIL_CODE_REPLY + 64));
        let mut reply = String::new();
        let n = reader.read_line(&mut reply).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("peer closed without answering mail-code request".to_owned());
        }
        if reply.len() as u64 > MAX_MAIL_CODE_REPLY + 6 || !reply.ends_with('\n') {
            return Err("oversize or unterminated mail-code reply".to_owned());
        }
        let trimmed = reply.trim_end_matches(['\r', '\n']);
        if let Some(sealed) = trimmed.strip_prefix("OK ") {
            if sealed.is_empty() || sealed.len() as u64 > MAX_MAIL_CODE_REPLY {
                return Err("oversize or empty mail-code reply".to_owned());
            }
            Ok(Ok(sealed.to_owned()))
        } else if let Some(refusal) = trimmed.strip_prefix("ERR ") {
            if refusal.len() as u64 > MAX_MAIL_CODE_REPLY {
                return Err("oversize mail-code refusal".to_owned());
            }
            Ok(Err(refusal.to_owned()))
        } else {
            Err(format!("unrecognized mail-code reply: {trimmed}"))
        }
    })();
    match (watchdog.finish(), exchange) {
        (_, Ok(done)) => Ok(done),
        (Claim::Authed, Err(why)) => Err(why),
        (_, Err(_)) => Err("mail-code request ran out of deadline".to_owned()),
    }
}

/// Dial `host:port` on [`FleetOp::MailCode`] and exchange one `mail-code <email> …` line.
///
/// # Errors
/// Returns an error string when the peer cannot be dialled or closes without a framed `OK`/`ERR`
/// reply. An explicit `ERR …` from the peer returns `Ok(Err(reason))`.
pub fn request_mail_code_from_peer(
    host: &str,
    addr: Option<IpAddr>,
    port: u16,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    request_line: &str,
    within: Duration,
) -> Result<Result<String, String>, String> {
    let peer = Peer {
        node: None,
        key: None,
        key_from: None,
        host: host.to_owned(),
        addr,
        port,
        version: None,
        seen_ms: None,
        via: Via::Issuer,
        fleet: None,
        signed: None,
    };
    request_mail_code_from_bound_peer(&peer, (secret, me), (&[], None), request_line, within)
}

/// Maximum byte length of a [`FleetOp::Census`] JSON payload (`256 KiB`).
pub const MAX_CENSUS_REPLY: usize = crate::fleet::REPLY_MAX;

/// Listener handler for [`FleetOp::Census`]: writes a single bounded `OK <compact-json>\n` or
/// `ERR <refusal>\n` line computed from `answer()`.
///
/// # Errors
/// Returns an I/O error if writing or flushing `stream` fails.
pub fn serve_census<S: Read + Write>(
    stream: &mut S,
    answer: impl FnOnce() -> Result<String, String>,
) -> io::Result<Result<usize, String>> {
    match answer() {
        Ok(json) => {
            if json.is_empty()
                || json.len() > MAX_CENSUS_REPLY
                || json.bytes().any(|b| matches!(b, b'\r' | b'\n'))
            {
                stream.write_all(b"ERR internal: invalid census payload\n")?;
                stream.flush()?;
                return Ok(Err("invalid census payload".to_owned()));
            }
            let out = format!("OK {json}\n");
            stream.write_all(out.as_bytes())?;
            stream.flush()?;
            Ok(Ok(json.len()))
        }
        Err(refusal) => {
            let clean = aterm_codec::tty::one_line(&refusal, REPLY_SHOWN_CHARS);
            let out = format!("ERR {clean}\n");
            stream.write_all(out.as_bytes())?;
            stream.flush()?;
            Ok(Err(clean))
        }
    }
}

/// Dial `host:port` on [`FleetOp::Census`] and read one bounded `OK <compact-json>` or
/// `ERR <refusal>` line within `within`.
///
/// # Errors
/// Returns why the peer could not be dialled or closed without a framed `OK`/`ERR` reply. An
/// explicit `ERR …` from the peer returns `Ok(Err(reason))`.
pub fn request_census_from_peer(
    host: &str,
    addr: Option<IpAddr>,
    port: u16,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    within: Duration,
) -> Result<Result<String, String>, String> {
    let deadline = deadline_after(within);
    let mut reached =
        dial(host, addr, port, (secret, me), FleetOp::Census, within).map_err(|e| e.to_string())?;
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err("census request ran out of deadline after handshake".to_owned());
    }
    let tcp = reached.transport.stream().get_ref();
    tcp.set_read_timeout(Some(left))
        .and_then(|()| tcp.set_write_timeout(Some(left)))
        .map_err(|e| e.to_string())?;
    let watchdog = HandshakeWatchdog::arm(tcp, left).map_err(|e| e.to_string())?;
    let exchange = (|| -> Result<Result<String, String>, String> {
        let stream = reached.transport.stream();
        let mut reader = io::BufReader::new(stream.take(MAX_CENSUS_REPLY as u64 + 64));
        let mut reply = String::new();
        let n = reader.read_line(&mut reply).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("peer closed without answering census request".to_owned());
        }
        if reply.len() > MAX_CENSUS_REPLY + 6 || !reply.ends_with('\n') {
            return Err("oversize or unterminated census reply".to_owned());
        }
        let trimmed = reply.trim_end_matches(['\r', '\n']);
        if let Some(json) = trimmed.strip_prefix("OK ") {
            if json.is_empty() || json.len() > MAX_CENSUS_REPLY {
                return Err("oversize or empty census reply".to_owned());
            }
            Ok(Ok(json.to_owned()))
        } else if let Some(refusal) = trimmed.strip_prefix("ERR ") {
            if refusal.len() > MAX_CENSUS_REPLY {
                return Err("oversize census refusal".to_owned());
            }
            Ok(Err(refusal.to_owned()))
        } else {
            Err(format!("unrecognized census reply: {trimmed}"))
        }
    })();
    match (watchdog.finish(), exchange) {
        (_, Ok(done)) => Ok(done),
        (Claim::Authed, Err(why)) => Err(why),
        (_, Err(_)) => Err("census request ran out of deadline".to_owned()),
    }
}

#[cfg(test)]
#[path = "fleet_member_tests.rs"]
mod tests;
