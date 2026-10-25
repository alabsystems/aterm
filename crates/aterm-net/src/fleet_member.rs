// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet membership (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5): the state a
//! machine keeps beside the fleet secret ([`crate::fleet::KEY_FILE`]), namely its node id and
//! key, book of peers, join code and copy of the issuer's book, and the exchanges made once the
//! handshake ([`crate::fleet`]) proved both ends: join, book read, survey and drive.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rustls::ClientConnection;

use crate::fleet::{
    Dialed, DriveGrant, FleetError, FleetMark, FleetOp, FleetScope, FleetSecret, KEY_FILE,
    NodeIdentity, NodeKey, NodeProof, REPLY_MAX, Served, client_handshake_as,
};
use crate::slots::{Claim, HandshakeWatchdog};
use crate::tls::TlsTransport;

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

const PEERS_LOCK: &str = ".fleet-peers.lock";

/// The most peers one book holds: a fleet is one LAN's machines, and a secret holder cannot
/// grow an issuer's book past this with made-up node ids.
pub const PEERS_MAX: usize = 256;

/// The most members the book records from one address ([`record`]): room for several aterm
/// instances on one host, each on its own fleet port, while a secret holder cannot fill an
/// issuer's book with made-up nodes from one address.
pub const JOINED_PER_ADDRESS: usize = 4;

/// The fleet port when none is named: `[fleet] listen`'s documented example.
pub const DEFAULT_PORT: u16 = 7100;

/// How many bytes of secret a join code carries: 160 bits, the fleet's shortest secret.
pub const SECRET_LEN: usize = FleetSecret::MIN_LEN;

/// The longest line either end of the join exchange sends.
pub const RECORD_MAX: usize = 512;

const PEERS_FILE_MAX: usize = 256 * 1024;

const LOCK_WAIT: Duration = Duration::from_secs(2);

#[cfg(test)]
thread_local! {
    /// A test's wait for the book's lock on its thread in place of [`LOCK_WAIT`], and who is told each time the lock is found held.
    static TEST_WAIT: std::cell::RefCell<Option<(Duration, Option<std::sync::mpsc::Sender<()>>)>> =
        const { std::cell::RefCell::new(None) };
}

fn lock_wait() -> Duration {
    #[cfg(test)]
    if let Some(wait) = TEST_WAIT.with_borrow(|seam| seam.as_ref().map(|(wait, _)| *wait)) {
        return wait;
    }
    LOCK_WAIT
}

fn found_held() {
    #[cfg(test)]
    TEST_WAIT.with_borrow(|seam| {
        if let Some((_, Some(told))) = seam {
            let _ = told.send(());
        }
    });
}

use aterm_uds::private_file::{
    PrivateDir, check_private_dir, judge_private_dir, remove_private, write_private,
};
pub use aterm_uds::private_file::{PrivateFileError, ensure_private_dir, read_private};

/// A new fleet secret, [`SECRET_LEN`] bytes from the OS CSPRNG.
pub fn mint_secret() -> io::Result<[u8; SECRET_LEN]> {
    let mut secret = [0u8; SECRET_LEN];
    aterm_uds::rand::fill(&mut secret)?;
    Ok(secret)
}

/// Whether `word` is a node id: 32 lowercase hex digits.
#[inline]
#[must_use]
pub fn is_node_id(word: &str) -> bool {
    aterm_codec::hex::is_lower(word, 32)
}

/// This machine's node id, from [`NODE_FILE`] in `dir`, minted and written the first time
/// (128 bits from the OS CSPRNG).
pub fn node_id(dir: &Path) -> io::Result<String> {
    if let Some(id) = read_node_id(dir)? {
        return Ok(id);
    }
    let id = aterm_uds::rand::hex_token::<16>()?;
    write_private(dir, NODE_FILE, format!("{id}\n").as_bytes())?;
    Ok(id)
}

const CODE_FILE_MAX: usize = 4096;

/// The join code this machine issued, as [`CODE_FILE`] in `dir` holds it: the secret, and
/// the host and port members dial.
pub fn read_join_code(dir: &Path) -> Result<JoinCode, String> {
    let bytes = read_private(dir, CODE_FILE, CODE_FILE_MAX).map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "it is not text".to_owned())?;
    JoinCode::parse(&text).map_err(|e| e.to_string())
}

/// This machine's node id as [`NODE_FILE`] in `dir` holds it, never minted: `None` when there
/// is none yet (no `aterm fleet init` or `join` ran here).
pub fn read_node_id(dir: &Path) -> io::Result<Option<String>> {
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
/// book's lock, waited for as every writer waits.
pub fn node_identity(dir: &Path) -> io::Result<NodeIdentity> {
    let _identity = BookLock::take(dir, lock_wait())
        .map_err(|e| io::Error::new(e.kind(), lock_refused(dir, &e)))?;
    let node = node_id(dir)?;
    let seed = match read_node_seed(dir)? {
        Some(seed) => seed,
        None => {
            let mut seed = [0u8; NODE_SEED_LEN];
            aterm_uds::rand::fill(&mut seed)?;
            let mut text = aterm_codec::hex::encode_lower(&seed);
            text.push('\n');
            write_private(dir, NODE_KEY_FILE, text.as_bytes())?;
            seed
        }
    };
    identity_of(&node, &seed)
}

/// This machine's node as [`NODE_FILE`] and [`NODE_KEY_FILE`] in `dir` hold it, never
/// minted: `None` while either is missing.
pub fn read_node_identity(dir: &Path) -> io::Result<Option<NodeIdentity>> {
    let Some(node) = read_node_id(dir)? else {
        return Ok(None);
    };
    read_node_seed(dir)?
        .map(|seed| identity_of(&node, &seed))
        .transpose()
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

/// A join code: the fleet secret and where its issuer listens.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCode {
    /// The fleet secret.
    pub secret: [u8; SECRET_LEN],
    /// The issuer's host name or address, as members dial it.
    pub host: String,
    /// The issuer's fleet port.
    pub port: u16,
}

impl fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JoinCode(.. @{})", host_port(&self.host, self.port))
    }
}

/// Why a text is no join code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeError {
    /// It does not start `atf1-`.
    Prefix,
    /// No `@<host>:<port>` at its end.
    NoIssuer,
    /// The character at this 1-based place (code characters only) is no code character.
    Character(usize),
    /// This many code characters, not [`CODE_SYMBOLS`].
    Length(usize),
    /// The check characters do not match: a typo.
    Typo,
    /// The issuer part is no host and port, for this reason.
    Issuer(&'static str),
}

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prefix => f.write_str(
                "a join code starts `atf1-` (the file `aterm fleet init` wrote holds one)",
            ),
            Self::NoIssuer => {
                f.write_str("the join code names no issuer (`@<host>:<port>` at its end)")
            }
            Self::Character(at) => write!(
                f,
                "the join code's character {at} is none of 0-9 and a-z without i, l, o, u: \
                 it was not copied whole"
            ),
            Self::Length(n) => write!(
                f,
                "a join code has {CODE_SYMBOLS} characters before its `@`, not {n}: it was \
                 not copied whole"
            ),
            Self::Typo => f.write_str(
                "the join code's check characters do not match: a character is mistyped or \
                 two are swapped; copy it again",
            ),
            Self::Issuer(why) => write!(f, "the join code's issuer is no host and port: {why}"),
        }
    }
}

impl std::error::Error for CodeError {}

impl JoinCode {
    /// The code as text: `atf1-`, nine groups of four, `@<host>:<port>`.
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
        out.push('@');
        out.push_str(&host_port(&self.host, self.port));
        out
    }

    /// Read a join code: `atf1-` in either case, its characters in any case with `-` and
    /// white space between them ignored, then `@<host>:<port>`.
    pub fn parse(text: &str) -> Result<Self, CodeError> {
        let text = text.trim();
        let body = text
            .get(..CODE_PREFIX.len())
            .filter(|head| head.eq_ignore_ascii_case(CODE_PREFIX))
            .and_then(|_| text.get(CODE_PREFIX.len()..))
            .ok_or(CodeError::Prefix)?;
        let (code, issuer) = body.rsplit_once('@').ok_or(CodeError::NoIssuer)?;
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
        let (host, port) = parse_host_port(issuer, None).map_err(CodeError::Issuer)?;
        Ok(Self {
            secret: from_symbols(&symbols[..DATA_SYMBOLS]),
            host,
            port,
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
            .parse::<u16>()
            .ok()
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
}

impl Peer {
    /// The peer as one line of the book: `key_from=` and `fleet=` only where there is one.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "peer node={} key={}{} host={} addr={} port={} version={} seen_ms={} via={}{}",
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
        let port = get("port")?.parse::<u16>().ok().filter(|p| *p != 0)?;
        let version = match get("version") {
            Some(v) => Some(version_word(&aterm_codec::pct::decode(v))?),
            None => None,
        };
        let seen_ms = match get("seen_ms") {
            Some(ms) => Some(ms.parse().ok()?),
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

    fn answers_to(&self, what: &str) -> bool {
        let what = what.trim().to_ascii_lowercase();
        let short = self.host.split('.').next().unwrap_or(&self.host);
        self.node
            .as_deref()
            .is_some_and(|node| node == what || (what.len() >= 8 && node.starts_with(&what)))
            || self.host.eq_ignore_ascii_case(&what)
            || short.eq_ignore_ascii_case(&what)
            || self.addr.is_some_and(|addr| addr.to_string() == what)
            || self.target().eq_ignore_ascii_case(&what)
    }
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

/// THE WRITE RULE (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5): every write of fleet state — [`KEY_FILE`], [`CODE_FILE`], [`PEERS_FILE`], [`ISSUER_BOOK_FILE`] and aterm.toml's `[fleet]` — is made in `write`, under the book's lock in `dir` and only while `dir` holds the fleet `began` names, from what `write` reads under it; a leave's is [`leave_held`]'s, and a level's swaps aterm.toml alone. The files are written by the [`HeldBook`] `write` is handed, and aterm.toml's `[fleet]` beside it, by convention. The lock is taken once per call, never inside another, for at most two seconds; aterm.toml's write lock, where `write` takes it, comes after it.
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

/// A leave's write, a first write of its own ([`write_held`]): the directory made private first, as an init or a join makes it (made where there is none, narrowed where it is open to others: [`Leaving::narrowed`]), then the book's lock taken there (one that is there but could not be made private decided by its lock, as any directory is), and gone on without the lock only where no writer could make the directory or take the lock either ([`stops_every_edit`]), so no odd state directory keeps a machine in a fleet. `remove` is handed a [`Leaving`], which removes the fleet's files and writes none.
/// # Errors
/// [`Unwritten::Lock`]: the directory cannot be made or narrowed, or the lock taken, for a reason that does not stop every writer ([`stops_every_edit`]): another writer holding the lock past its wait, the directory removed under it, no descriptor or space free among them.
pub fn leave_held<R>(dir: &Path, remove: impl FnOnce(&Leaving<'_>) -> R) -> Result<R, Unwritten> {
    let narrowed = match judge_private_dir(dir) {
        Ok(PrivateDir::Missing | PrivateDir::Open(_)) => match ensure_private_dir(dir) {
            Ok(narrowed) => narrowed,
            // None to make, by any writer: none to lock either.
            Err(e)
                if stops_every_edit(&e)
                    && matches!(judge_private_dir(dir), Ok(PrivateDir::Missing)) =>
            {
                return Ok(remove(&Leaving {
                    dir,
                    narrowed: None,
                    _lock: None,
                }));
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
    Ok(remove(&Leaving {
        dir,
        narrowed,
        _lock: lock,
    }))
}

/// The book's lock in a state directory, held while this lives ([`write_held`]): no other writer, an older aterm's edit of the book included, comes between the writes made under it. Through this API it alone writes the fleet's files ([`KEY_FILE`], [`CODE_FILE`], [`PEERS_FILE`], [`ISSUER_BOOK_FILE`]), so none is written without the lock; a leave removes them by a [`Leaving`].
pub struct HeldBook<'a> {
    dir: &'a Path,
    held: Held,
    _lock: BookLock,
}

/// A leave's hold on a state directory ([`leave_held`]): the book's lock, or none where no writer could take it either. It removes the fleet's files and writes none.
pub struct Leaving<'a> {
    dir: &'a Path,
    narrowed: Option<u32>,
    _lock: Option<BookLock>,
}

impl Leaving<'_> {
    /// The mode the state directory had before it was narrowed, where it was open to others.
    #[must_use]
    pub const fn narrowed(&self) -> Option<u32> {
        self.narrowed
    }

    /// Remove the state directory's file `name`: whether it was there.
    /// # Errors
    /// `name` is no one file name, or the file is there and cannot be removed.
    pub fn remove(&self, name: &str) -> io::Result<bool> {
        remove_private(self.dir, name)
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
             that book (`aterm fleet peers`, `aterm ctl survey --fleet`, a drive's check of an \
             old copy), never by hand.\n\
             {ISSUER_BOOK_HEAD}{issuer}\n"
        );
        for copy in copies.iter().filter(|copy| copy.via == Via::IssuerBook) {
            text.push_str(&copy.line());
            text.push('\n');
        }
        write_private(self.dir, ISSUER_BOOK_FILE, text.as_bytes())
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
        write_private(self.dir, CODE_FILE, format!("{}\n", code.text()).as_bytes())
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
    /// A person's peer the book already holds at that place.
    Already,
}

/// Put `peer` in `book`: one with a node id replaces the entry with that id, and a person's
/// entry for the place it stands for (for a member's registration the address and port it
/// registered from, never the host name it gives itself; for an issuer its place); one a
/// person named is added unless the book holds that place already ([`Peer::same_place`]).
/// It refuses a copy of the issuer's book ([`Via::IssuerBook`]), a registration whose key
/// does not fit the book's entry for its node or whose address already has
/// [`JOINED_PER_ADDRESS`] other members, and a new entry once the book is full.
pub fn record(book: &mut Vec<Peer>, peer: Peer) -> Result<Recorded, String> {
    let full = || {
        format!("the book holds {PEERS_MAX} peers already: remove one with `aterm fleet peers rm`")
    };
    if peer.via == Via::IssuerBook {
        return Err(
            "a copy of the issuer's book is written by a read of that book, never recorded"
                .to_owned(),
        );
    }
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
    if joined
        && book
            .iter()
            .filter(|known| known.via == Via::Joined && at_addr(known) && known.node != peer.node)
            .count()
            >= JOINED_PER_ADDRESS
    {
        return Err(format!(
            "{} registered {JOINED_PER_ADDRESS} members already: remove one with `aterm fleet \
             peers rm`",
            peer.addr
                .map_or_else(|| "its address".to_owned(), |a| a.to_string())
        ));
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
        let key_from = (joined && known.key.is_none() && peer.key.is_none())
            .then(|| known.key_from.or(known.addr))
            .flatten()
            .filter(|from| Some(*from) != peer.addr);
        *known = Peer { key_from, ..peer };
        return Ok(Recorded::Updated);
    }
    if book.len() >= PEERS_MAX {
        return Err(full());
    }
    book.push(peer);
    Ok(Recorded::New)
}

fn bound_as(known: &Peer, peer: &Peer) -> Result<(), String> {
    let node = peer.node.as_deref().unwrap_or("-");
    let node = node.get(..8).unwrap_or(node);
    let rm = format!("`aterm fleet peers rm {node}` {REBIND}");
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
    let shown = aterm_codec::tty::one_line(what, 80);
    let matches: Vec<usize> = peers
        .iter()
        .enumerate()
        .filter(|(_, peer)| peer.answers_to(what))
        .map(|(at, _)| at)
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [at] => Ok(Some(*at)),
        many => Err(format!(
            "`{shown}` names {} peers ({}): name one by its node id or its address and port",
            many.len(),
            many.iter()
                .filter_map(|at| peers.get(*at))
                .map(|peer| format!(
                    "{} at {}",
                    peer.node
                        .as_deref()
                        .map_or("-", |n| n.get(..8).unwrap_or(n)),
                    peer.target()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Take the one peer `what` names out of `book` ([`find`]).
pub fn remove(book: &mut Vec<Peer>, copies: &[Peer], what: &str) -> Result<Peer, String> {
    let shown = aterm_codec::tty::one_line(what, 80);
    match find(book, what)? {
        Some(at) => Ok(book.remove(at)),
        None if !matches!(find(copies, what), Ok(None)) => Err(format!(
            "`{shown}` is in the fleet issuer's book, which this machine copies at each read: \
             `aterm fleet peers rm` on the issuer takes it out, and this copy follows"
        )),
        None => Err(format!("no peer is `{shown}`")),
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

    fn copied_alone(&self, node: &str) -> bool {
        !self.own.iter().any(|(bound, _)| bound == node)
            && self.copied.iter().any(|(bound, _)| bound == node)
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

impl fmt::Display for Unproven {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = |node: &str| node.get(..8).unwrap_or(node).to_owned();
        match self {
            Self::NoProof => {
                f.write_str("the peer proved no node (an older aterm, or one with no node key)")
            }
            Self::Unbound(node) => write!(
                f,
                "node {} is bound to no key here: it is bound when it joins the issuer again",
                short(node)
            ),
            Self::OtherKey(node) => write!(
                f,
                "the peer claims node {} and proved another key than this machine binds to it",
                short(node)
            ),
        }
    }
}

impl std::error::Error for Unproven {}

/// The proven node of the peer of a fleet connection this listener serves, for an
/// authorization that names nodes (a remote step-in): [`Keyring::proven`] over the keys in
/// `state_dir`.
pub fn proven_peer(served: &Served, state_dir: &Path) -> Result<String, Unproven> {
    Keyring::read(state_dir).proven(served.peer_node())
}

/// Why a peer may not drive this machine ([`Keyring::driver`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Undriven {
    /// It is no node this machine can name ([`Keyring::proven`]).
    Unproven(Unproven),
    /// No grant names its node.
    Unlisted(String),
    /// A grant names its node under another key than the one it proved: the node id was
    /// bound afresh since the person here let it drive.
    Rekeyed(String),
    /// Only this machine's copy of its issuer's book binds its node, the copy is older than
    /// [`COPY_DRIVES_FOR`], and the issuer's book could not be read again ([`proven_driver`]).
    Unchecked {
        /// The node.
        node: String,
        /// Why the issuer's book was not read.
        why: String,
    },
}

impl fmt::Display for Undriven {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = |node: &str| node.get(..8).unwrap_or(node).to_owned();
        match self {
            Self::Unproven(why) => write!(f, "{why}"),
            Self::Unlisted(node) => write!(
                f,
                "node {} is not one this Mac lets drive it (aterm fleet allow drive --from, \
                 there)",
                short(node)
            ),
            Self::Rekeyed(node) => write!(
                f,
                "node {} proved another key than the one it held when this Mac's person let it \
                 drive: it drives again only once they run aterm fleet allow drive --from, there",
                short(node)
            ),
            Self::Unchecked { node, why } => write!(
                f,
                "node {} is bound here only by this Mac's copy of its issuer's book, which is \
                 over {} s old, and this Mac could not read that book again to check it ({}): it \
                 drives again once this Mac reaches its issuer (aterm fleet peers, there, says why \
                 it does not)",
                short(node),
                COPY_DRIVES_FOR.as_secs(),
                aterm_codec::tty::one_line(why, 160)
            ),
        }
    }
}

impl std::error::Error for Undriven {}

impl Keyring {
    /// The driver behind `proof`: the node it proved under the key this keyring binds it to
    /// ([`Self::proven`]), when `grants` names that node with that same key.
    pub fn driver(
        &self,
        proof: Option<&NodeProof>,
        grants: &[DriveGrant],
    ) -> Result<String, Undriven> {
        let node = self.proven(proof).map_err(Undriven::Unproven)?;
        match grants.iter().find(|grant| grant.node == node) {
            None => Err(Undriven::Unlisted(node)),
            Some(grant) if proof.is_some_and(|proof| proof.key == grant.key) => Ok(node),
            Some(_) => Err(Undriven::Rekeyed(node)),
        }
    }
}

/// How long a member's copy of its issuer's book speaks for the issuer when a drive is
/// admitted ([`proven_driver`]): one minute since the copy was written.
pub const COPY_DRIVES_FOR: Duration = Duration::from_secs(60);

/// The one deadline of the issuer's-book read a drive's admission makes ([`proven_driver`]):
/// a fleet read's default (3 s), which fits inside the drive's connection
/// ([`crate::fleet::SERVED_WITHIN`], 30 s) beside a turn's 22 s at most.
pub const RECHECK_WITHIN: Duration = Duration::from_secs(3);

/// The driver of a fleet connection this machine's listener serves: [`Keyring::driver`] over
/// the book in `state_dir`, read fresh on each call, so on the issuer a `peers rm` or a join
/// counts at once. A node only a copy older than [`COPY_DRIVES_FOR`] binds is checked against
/// the issuer's book, read again within [`RECHECK_WITHIN`] and kept as the new copy.
pub fn proven_driver(
    served: &Served,
    state_dir: &Path,
    grants: &[DriveGrant],
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
) -> Result<String, Undriven> {
    driver_as_of(
        served.peer_node(),
        (state_dir, grants, secret),
        std::time::SystemTime::now(),
        |issuer| {
            fetch_book(
                issuer,
                (secret, me),
                (Instant::now() + RECHECK_WITHIN, RECHECK_WITHIN),
            )
        },
    )
}

/// The driver of a fleet connection as this machine's book and its copy of its issuer's book
/// hold it now, however old the copy ([`Keyring::driver`] over [`Keyring::read`]): what a
/// survey tells its reader about driving here, never what admits a drive ([`proven_driver`]).
pub fn listed_driver(
    served: &Served,
    state_dir: &Path,
    grants: &[DriveGrant],
) -> Result<String, Undriven> {
    Keyring::read(state_dir).driver(served.peer_node(), grants)
}

fn driver_as_of(
    proof: Option<&NodeProof>,
    (state_dir, grants, secret): (&Path, &[DriveGrant], &FleetSecret),
    now: std::time::SystemTime,
    fetch: impl FnOnce(&Target) -> Result<Book, BookUnread>,
) -> Result<String, Undriven> {
    let written = issuer_book_written(state_dir);
    let (peers, copies) = book_and_copies(state_dir);
    let keys = Keyring::of(peers.iter().chain(&copies));
    let node = keys.driver(proof, grants)?;
    let fresh = written
        .and_then(|written| now.duration_since(written).ok())
        .is_some_and(|age| age <= COPY_DRIVES_FOR);
    if fresh || !keys.copied_alone(&node) {
        return Ok(node);
    }
    let unchecked = |why: String| Undriven::Unchecked {
        node: node.clone(),
        why,
    };
    let issuer = issuer_of(&peers).ok_or_else(|| unchecked("this Mac names no issuer".into()))?;
    let book = fetch(&target_of(issuer)).map_err(|why| unchecked(unread_words(&why)))?;
    if !book.proven {
        return Err(unchecked(
            "this Mac binds no key to its issuer, so its book binds none".into(),
        ));
    }
    let own = read_node_id(state_dir).ok().flatten();
    let _ = keep_issuer_book(
        state_dir,
        secret,
        Some((&book.node, &copies_of(&book, own.as_deref()))),
    );
    keys.with_book(&book, own.as_deref()).driver(proof, grants)
}

fn issuer_book_written(state_dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::symlink_metadata(state_dir.join(ISSUER_BOOK_FILE))
        .and_then(|meta| meta.modified())
        .ok()
}

fn unread_words(why: &BookUnread) -> String {
    let short = |node: &str| node.get(..8).unwrap_or(node).to_owned();
    match why {
        BookUnread::Older(at) => {
            format!("the issuer at {at} runs an older aterm, which shares no book")
        }
        BookUnread::NoBook { at, why, .. } => format!("the machine at {at} shares no book: {why}"),
        BookUnread::NotNow { at, why, .. } => {
            format!("the issuer at {at} cannot share its book now: {why}")
        }
        BookUnread::OtherNode(at, node) => {
            format!(
                "{at} answered as node {}, not this Mac's issuer",
                short(node)
            )
        }
        BookUnread::Unreached(unreached) => unreached.to_string(),
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

/// Why the book's lock in `dir` was not taken, naming `dir` once: [`BookLock::take`]'s own refusals name it, a bare OS error does not.
fn lock_refused(dir: &Path, e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(_) => format!("{PEERS_FILE}'s lock in {}: {e}", dir.display()),
        None => format!("{PEERS_FILE}'s lock: {e}"),
    }
}

struct BookLock(std::fs::File);

impl BookLock {
    fn take(dir: &Path, within: Duration) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        check_private_dir(dir)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join(PEERS_LOCK))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is no regular file", dir.join(PEERS_LOCK).display()),
            ));
        }
        let deadline = Instant::now() + within;
        let mut backoff = Duration::from_millis(1);
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    found_held();
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(50));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!(
                            "another aterm is writing the book in {}; run this again",
                            dir.display()
                        ),
                    ));
                }
            }
        }
    }
}

impl Drop for BookLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// What a member tells the issuer it joins ([`Registration::line`]).
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
}

impl Registration {
    /// The registration as its line, newline included: `key=` last, which an older issuer
    /// passes over.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "register node={} host={} port={} version={}{}\n",
            self.node,
            aterm_codec::pct::encode(&self.host),
            self.port,
            aterm_codec::pct::encode(&self.version),
            self.key
                .map_or_else(String::new, |key| format!(" key={}", key.hex())),
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
        Ok(Self {
            node: node.to_owned(),
            host,
            port,
            version,
            key,
        })
    }
}

/// The issuer, as it names itself in its answer to a join.
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

/// What a join's registration came to: the issuer, and how many peers its book holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Joined {
    /// The issuer.
    pub issuer: Issuer,
    /// Its book's size, this member included.
    pub peers: usize,
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

/// The refusal a machine that holds no join code answers a `join` with ([`serve_join`]): a
/// member's `aterm fleet join` reads it as no issuer.
pub const NOT_THE_ISSUER: &str = "this machine is no fleet's issuer: it holds no join code, so \
                                  it records no members (join at the machine whose `aterm fleet \
                                  init` wrote the code)";

/// The refusal of a join proved under a fleet left or rotated before it was recorded.
pub const MOVED_ON: &str = "this machine left the fleet, or rotated its secret, while the join was \
                            served, so nothing was recorded: join with the code it issues now";

/// The issuer's end of a join, on a stream the handshake proved for op `join` under `secret`:
/// read the member's [`Registration`] and, while that fleet is held ([`write_held`]), record it
/// in the book in `state_dir` with the address `from` it came from; a key it names must be
/// the one `proof` ([`Served::peer_node`]) proved for that node. It answers `OK` with the
/// issuer `issuer` gives and the book's size, or `ERR <why>`: a malformed record, an unproved
/// key, a refusal of [`record`], an unreadable book, the issuer's own node id, a fleet left or
/// rotated meanwhile ([`MOVED_ON`]), or an issuer that cannot name itself (no node id, or a
/// host that is no [`is_host`], which the member's [`register`] would refuse after the book
/// recorded it). A machine with no [`CODE_FILE`] records nothing ([`NOT_THE_ISSUER`]), so a
/// member's book holds only what its own join and `peers add` put there.
pub fn serve_join<S: Read + Write>(
    stream: &mut S,
    (from, proof): (IpAddr, Option<&NodeProof>),
    (state_dir, secret): (&Path, &FleetSecret),
    issuer: impl FnOnce() -> Result<Issuer, String>,
    now_ms: u64,
) -> io::Result<Result<(Recorded, Peer), String>> {
    let line = read_record(stream)?;
    let issues = || {
        !matches!(
            std::fs::symlink_metadata(state_dir.join(CODE_FILE)),
            Err(e) if e.kind() == io::ErrorKind::NotFound
        )
    };
    let outcome = match Registration::parse(&line) {
        Err(why) => Err(why.to_owned()),
        Ok(_) if !issues() => Err(NOT_THE_ISSUER.to_owned()),
        Ok(reg)
            if reg.key.is_some_and(|key| {
                proof.is_none_or(|proof| proof.node != reg.node || proof.key != key)
            }) =>
        {
            Err(
                "the registration names a node key this connection did not prove: a member \
                 proves its key as it dials (`aterm fleet join` does)"
                    .to_owned(),
            )
        }
        Ok(reg) => issuer()
            .and_then(|issuer| {
                if is_host(&issuer.host) {
                    Ok(issuer)
                } else {
                    Err("its host name is no host a member can dial".to_owned())
                }
            })
            .map_err(|why| format!("the issuer cannot name itself: {why}"))
            .and_then(|issuer| {
                if reg.node == issuer.node {
                    return Err(
                        "that node is this machine, the fleet's issuer: it does not join itself"
                            .to_owned(),
                    );
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
                };
                let recorded_as = peer.clone();
                // Only while the fleet the connection was proved under is held, and this
                // machine still issues it.
                match write_held(
                    state_dir,
                    Began::Under(&Held::Key(secret.clone())),
                    |book| {
                        if !issues() {
                            return Err(NOT_THE_ISSUER.to_owned());
                        }
                        book.update(|book| {
                            record(book, peer).map(|recorded| (recorded, book.len()))
                        })
                    },
                ) {
                    Ok(recorded) => recorded,
                    Err(Unwritten::Moved) => Err(MOVED_ON.to_owned()),
                    Err(Unwritten::Lock(why)) => Err(why),
                }
                .map(|(recorded, peers)| (recorded, peers, issuer, recorded_as))
            }),
    };
    let (answer, outcome) = match outcome {
        Ok((recorded, peers, issuer, recorded_as)) => (
            format!(
                "OK node={} host={} port={} version={} peers={peers}",
                issuer.node,
                aterm_codec::pct::encode(&issuer.host),
                issuer.port,
                aterm_codec::pct::encode(&issuer.version),
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

/// The member's end of a join, on a stream proved for op `join`: send `reg`, read the answer.
pub fn register<S: Read + Write>(stream: &mut S, reg: &Registration) -> Result<Joined, String> {
    stream
        .write_all(reg.line().as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| format!("the registration could not be sent: {e}"))?;
    let line = read_record(stream).map_err(|e| format!("the issuer did not answer: {e}"))?;
    if let Some(why) = line.strip_prefix("ERR ") {
        return Err(format!(
            "the issuer refused: {}",
            aterm_codec::tty::one_line(why, 400)
        ));
    }
    let not_an_answer = || "the issuer's answer is no join answer (an older aterm?)".to_owned();
    let fields = fields(line.strip_prefix("OK ").ok_or_else(not_an_answer)?);
    let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    let node = get("node")
        .filter(|node| is_node_id(node))
        .ok_or_else(not_an_answer)?;
    let host = aterm_codec::pct::decode(get("host").ok_or_else(not_an_answer)?);
    let port = get("port")
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(not_an_answer)?;
    let version = version_word(&aterm_codec::pct::decode(
        get("version").ok_or_else(not_an_answer)?,
    ))
    .ok_or_else(not_an_answer)?;
    let peers = get("peers")
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
) -> Result<Joined, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    let late = || {
        format!(
            "the issuer did not answer within {:.1}s",
            left.as_secs_f64()
        )
    };
    if left.is_zero() {
        return Err(late());
    }
    let tcp = reached.transport.stream().get_ref();
    tcp.set_read_timeout(Some(left))
        .and_then(|()| tcp.set_write_timeout(Some(left)))
        .map_err(|e| format!("could not bound the registration: {e}"))?;
    let watchdog = HandshakeWatchdog::arm(tcp, left)
        .map_err(|e| format!("could not bound the registration: {e}"))?;
    let joined = register(reached.transport.stream(), reg);
    match (watchdog.finish(), joined) {
        (_, Ok(joined)) => Ok(joined),
        (Claim::Authed, Err(why)) => Err(why),
        (_, Err(_)) => Err(late()),
    }
}

/// The refusal a machine that holds no join code answers a `book` read with ([`serve_book`]).
pub const NO_BOOK: &str = "this machine is no fleet's issuer: it holds no join code, so it \
                           keeps no book of the fleet's members to share";

/// The word of a book read's `ERR` from a machine that holds no join code: it has no book to
/// share, so the member's copy goes when it is the issuer that says so ([`read_whole`]).
pub const BOOK_NONE: &str = "no-book";

/// The word of a book read's `ERR` from an issuer that cannot share its book this time (it
/// cannot read it, or cannot name itself): the member keeps its copy.
pub const BOOK_NOT_NOW: &str = "not-now";

/// The listener's end of a book read, on a stream proved for op `book`: answered unasked.
pub fn serve_book<S: Write>(
    stream: &mut S,
    state_dir: &Path,
    node: impl FnOnce() -> Result<String, String>,
) -> io::Result<Result<usize, String>> {
    let issues = !matches!(
        std::fs::symlink_metadata(state_dir.join(CODE_FILE)),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    );
    let node = node().and_then(|node| {
        if is_node_id(&node) {
            Ok(node)
        } else {
            Err("its node id is no node id".to_owned())
        }
    });
    let shared = match (issues, &node) {
        (false, _) => Err((BOOK_NONE, NO_BOOK.to_owned())),
        (true, Err(why)) => Err((
            BOOK_NOT_NOW,
            format!("the issuer cannot name itself: {why}"),
        )),
        (true, Ok(_)) => read_peers(state_dir).map_err(|e| {
            (
                BOOK_NOT_NOW,
                format!("the issuer cannot read its book: {e}"),
            )
        }),
    };
    let named = node
        .as_ref()
        .map_or_else(|_| String::new(), |node| format!("node={node} "));
    let (answer, outcome) = match shared {
        Ok(book) => {
            let first_hand: Vec<&Peer> = book
                .iter()
                .filter(|peer| peer.via.first_hand())
                .take(PEERS_MAX)
                .collect();
            let mut answer = format!("OK {} {}\n", first_hand.len(), named.trim_end());
            for peer in &first_hand {
                answer.push_str(&peer.line());
                answer.push('\n');
            }
            (answer, Ok(first_hand.len()))
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
    /// The address answered with this other node's book (`node=`).
    OtherNode(SocketAddr, String),
    /// Not reached, or its answer was no book.
    Unreached(Unreached),
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
    (held, mut first): (Option<&'a HeldAddrs>, Option<(SocketAddr, Opened<'a>)>),
) -> Result<Book, BookUnread> {
    // Open until the book's stream is gone, which is dropped first.
    let mut holding = None;
    let mut reached = walk(
        (&issuer.host, issuer.addr, issuer.port),
        (deadline, within),
        (&[], resolve),
        |to| {
            let mine = match first.take() {
                Some((at, opened)) if at == to => Some(opened),
                _ => held.and_then(|held| held.enter_after(to, deadline)),
            };
            let reached = reach(to, (secret, me), FleetOp::Book, (deadline, within));
            if let Some(mut mine) = mine {
                match &reached {
                    AtAddr::Reached(_) => holding = Some(mine),
                    AtAddr::Passed(why @ Unreached::Connect(..)) => mine.close(Exchanged::Failed {
                        passed: true,
                        why: why.copied(),
                    }),
                    AtAddr::Passed(_) | AtAddr::Ended(_) => mine.close(Exchanged::Unasked),
                }
            }
            reached
        },
    )
    .map_err(|unreached| match unreached {
        Unreached::Handshake(at, FleetError::UnknownOp(_)) => BookUnread::Older(at),
        other => BookUnread::Unreached(other),
    })?;
    let at = reached.addr;
    let keys = Keyring {
        issuer: issuer.node.clone(),
        ..Keyring::default()
    };
    let proven = match answered_as(issuer, &keys, at, reached.peer.as_ref()) {
        Ok(answered) => answered.is_some_and(|(_, proven)| proven),
        Err(Unreached::OtherNode { at, said, .. }) => return Err(BookUnread::OtherNode(at, said)),
        Err(unreached) => return Err(BookUnread::Unreached(unreached)),
    };
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
    if issuer.node.as_deref().is_some_and(|want| want != node) {
        return Err(BookUnread::OtherNode(at, node));
    }
    let peers = lines
        .iter()
        .filter_map(|line| Peer::parse(line))
        .filter(|peer| peer.via.first_hand())
        .collect();
    Ok(Book {
        node,
        peers,
        at: at.ip().to_canonical(),
        proven,
    })
}

fn book_head(line: &str) -> Result<usize, String> {
    if line.starts_with("ERR ") {
        return Ok(0);
    }
    let shown = aterm_codec::tty::one_line(line, 120);
    match line
        .strip_prefix("OK ")
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse::<usize>().ok())
    {
        Some(n) if n <= PEERS_MAX => Ok(n),
        Some(n) => Err(format!(
            "promised {n} entries, past the {PEERS_MAX} a book holds: refused"
        )),
        None => Err(format!("answered no book: {shown}")),
    }
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
                Exchanged::Proved(scope, peer) | Exchanged::Answered(scope, peer, _) => {
                    Some(Seen::Proved(*scope, peer.clone()))
                }
                Exchanged::Unasked => Some(Seen::Gone),
            });
            let (scope, peer) = match seen {
                None => return AtAddr::Ended(Unreached::Deadline(Stage::Connect, within)),
                Some(Seen::Gone) => continue,
                // Its deadline came there, and this dial's has not.
                Some(Seen::Failed(_, Unreached::Deadline(..))) if Instant::now() < deadline => {
                    continue;
                }
                Some(Seen::Failed(true, why)) => return AtAddr::Passed(why),
                Some(Seen::Failed(false, why)) => return AtAddr::Ended(why),
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
    /// Proved, and its answer not come: that aterm's control server is stuck.
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

    /// Whether the connect failed with no route to the host (`EHOSTUNREACH`): what macOS's
    /// local-network privacy gives an app the person has not allowed, and a host off the LAN.
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
                io::ErrorKind::HostUnreachable => write!(
                    f,
                    "no route to {}: off the LAN, or not allowed in Privacy & Security › Local \
                     Network",
                    addr.ip()
                ),
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
                "{addr} hung up mid-handshake: its fleet listener restarting, or no aterm; try \
                 again"
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
                FleetError::Alpn => write!(f, "{addr} is a pre-fleet aterm listener (update it)"),
                FleetError::AddressBusy => write!(
                    f,
                    "{addr} is busy with other reads from this machine's address: try again when \
                     one ends"
                ),
                FleetError::UnknownOp(op) if op == FleetOp::Drive.word() => write!(
                    f,
                    "{addr} lets no one drive it: its person runs `aterm fleet allow drive \
                     --from <this Mac>` there (or it runs an older aterm)"
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
                        "proved itself, but sent no survey within {within:.1}s: its aterm's \
                         control server is stuck"
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
                "{host} did not resolve within {:.1}s (no DNS or multicast-DNS answer)",
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
                said.get(..8).unwrap_or(said),
                want.get(..8).unwrap_or(want)
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
                proved.get(..8).unwrap_or(proved),
                claimed.chars().take(8).collect::<String>()
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
/// [`CONNECT_WITHIN`], and a lookup still running at the deadline is left to end on its own
/// thread.
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
        (Instant::now() + within, within),
        (&[], resolve),
    )
}

type Resolver = fn(&str, u16) -> Vec<SocketAddr>;

fn dial_avoiding(
    host: &str,
    addr: Option<IpAddr>,
    port: u16,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    op: FleetOp,
    (deadline, within): (Instant, Duration),
    (avoid, resolver): (&[SocketAddr], Resolver),
) -> Result<Reached, Unreached> {
    walk(
        (host, addr, port),
        (deadline, within),
        (avoid, resolver),
        |to| reach(to, (secret, me), op, (deadline, within)),
    )
}

/// What one address of a dial came to ([`walk`]).
enum AtAddr<R> {
    /// It answered: what the dial asks of an address.
    Reached(R),
    /// It was passed over, for this reason: the next address is tried.
    Passed(Unreached),
    /// The dial ends here, for this reason.
    Ended(Unreached),
}

/// The addresses of a dial to `host` (`addr` first) on `port` by `deadline`, never one in
/// `avoid`: `attempt` of each in turn until one is reached or ends the dial. When none answers
/// the reason is the first failure, or the first past the peer's proof (a busy address),
/// which says where it answers.
fn walk<R>(
    (host, addr, port): (&str, Option<IpAddr>, u16),
    (deadline, within): (Instant, Duration),
    (avoid, resolver): (&[SocketAddr], Resolver),
    mut attempt: impl FnMut(SocketAddr) -> AtAddr<R>,
) -> Result<R, Unreached> {
    let mut tried: Vec<SocketAddr> = avoid.to_vec();
    let mut first = None;
    let mut at = |to: SocketAddr, first: &mut Option<Unreached>| {
        if !crate::fleet::is_fleet_source(to.ip()) {
            first.get_or_insert(Unreached::NotFleet(to));
            return None;
        }
        match attempt(to) {
            AtAddr::Reached(reached) => Some(Ok(reached)),
            AtAddr::Passed(why) => {
                if first
                    .as_ref()
                    .is_none_or(|had| had.before_proof() && !why.before_proof())
                {
                    *first = Some(why);
                }
                None
            }
            AtAddr::Ended(why) => Some(Err(why)),
        }
    };
    if let Some(ip) = addr {
        let to = SocketAddr::new(ip, port);
        if !tried.contains(&to) {
            tried.push(to);
            if let Some(done) = at(to, &mut first) {
                return done;
            }
        }
    }
    let left = deadline.saturating_duration_since(Instant::now());
    let resolved = match resolve_within(host, port, left, resolver) {
        Lookup::Found(resolved) => resolved,
        Lookup::Late => {
            return Err(first.unwrap_or_else(|| Unreached::Lookup(host.to_owned(), within)));
        }
        Lookup::Unstarted(e) => {
            return Err(
                first.unwrap_or_else(|| Unreached::Local(format!("could not look {host} up: {e}")))
            );
        }
    };
    for to in resolved {
        if tried.contains(&to) {
            continue;
        }
        tried.push(to);
        if let Some(done) = at(to, &mut first) {
            return done;
        }
    }
    Err(first.unwrap_or_else(|| Unreached::Resolve(host.to_owned())))
}

/// One address of a dial: a connect to `to`, then the proof of `op` there, by `deadline`.
fn reach(
    to: SocketAddr,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    op: FleetOp,
    (deadline, within): (Instant, Duration),
) -> AtAddr<Reached> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return AtAddr::Ended(Unreached::Deadline(Stage::Connect, within));
    }
    match TcpStream::connect_timeout(&to, left.min(CONNECT_WITHIN)) {
        Ok(tcp) => {
            let left = deadline.saturating_duration_since(Instant::now());
            match client_handshake_as(tcp, secret, me, op, left) {
                Ok(Dialed {
                    transport,
                    scope,
                    peer,
                }) => AtAddr::Reached(Reached {
                    transport,
                    scope,
                    addr: to,
                    peer,
                }),
                Err(FleetError::Timeout(_)) => {
                    AtAddr::Ended(Unreached::Deadline(Stage::Handshake, within))
                }
                Err(e @ FleetError::UnknownOp(_)) => AtAddr::Ended(Unreached::Handshake(to, e)),
                Err(e) => AtAddr::Passed(Unreached::Handshake(to, e)),
            }
        }
        Err(e) => AtAddr::Passed(Unreached::Connect(to, e)),
    }
}

enum Lookup {
    Found(Vec<SocketAddr>),
    Late,
    Unstarted(io::Error),
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
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Lookup::Found(vec![SocketAddr::new(ip, port)]);
    }
    if within.is_zero() {
        return Lookup::Late;
    }
    let deadline = Instant::now() + within;
    let mut names = vec![host.to_owned()];
    if !host.contains('.') {
        names.push(format!("{host}.local"));
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(names.len());
    let mut asking = 0usize;
    let mut unstarted = None;
    for name in names {
        let Some(permit) = ResolverPermit::take(&RESOLVERS_IN_FLIGHT) else {
            return Lookup::Unstarted(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the fleet resolver is at capacity",
            ));
        };
        let tx = tx.clone();
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
        return unstarted.map_or(Lookup::Late, Lookup::Unstarted);
    }
    for _ in 0..asking {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(found) if !found.is_empty() => return Lookup::Found(found),
            Ok(_) => {}
            Err(_) => return Lookup::Late,
        }
    }
    Lookup::Found(Vec::new())
}

fn resolve(name: &str, port: u16) -> Vec<SocketAddr> {
    let Ok(found) = (name, port).to_socket_addrs() else {
        return Vec::new();
    };
    let mut found: Vec<SocketAddr> = found.collect();
    found.sort_by_key(SocketAddr::is_ipv6);
    found.dedup();
    found.truncate(4);
    found
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
    /// What a check reads of it: the level, the address, the node and whether it is proved.
    #[must_use]
    pub fn checked(&self) -> Checked {
        Checked {
            scope: self.scope,
            addr: self.addr,
            node: self.node.clone(),
            proven: self.proven,
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
    let deadline = (Instant::now() + within, within);
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
            | BookUnread::OtherNode(at, _)
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
    let short = |node: &str| node.get(..8).unwrap_or(node).to_owned();
    let (at, node) = match &fetched {
        Err(BookUnread::Unreached(
            Unreached::Impostor { at, .. } | Unreached::Unproved { at, .. },
        )) => (Some(*at), None),
        Ok(_) | Err(BookUnread::Unreached(_)) => (None, None),
        Err(BookUnread::Older(at)) => (Some(*at), None),
        Err(BookUnread::NoBook { at, node, .. } | BookUnread::NotNow { at, node, .. }) => {
            (Some(*at), node.clone())
        }
        Err(BookUnread::OtherNode(at, node)) => (Some(*at), Some(node.clone())),
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
        Err(BookUnread::OtherNode(at, node)) => format!(
            "{at} answered with the book of node {}, not the issuer's",
            short(&node)
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
                        (Instant::now() + deadline.1, deadline.1)
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
        (Instant::now() + within, within),
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
    let (at, other) = match ask_at(
        target,
        keys,
        target.addr,
        (secret, me),
        deadline,
        request,
        (&[], held),
    ) {
        Ok(first) => {
            let (Some(want), Some(got)) = (target.node.as_deref(), first.node.as_deref()) else {
                return Ok(first);
            };
            if want == got {
                return Ok(first);
            }
            let other = Unreached::OtherNode {
                at: first.addr,
                said: got.to_owned(),
                want: want.to_owned(),
            };
            (first.addr, other)
        }
        Err(
            other @ (Unreached::OtherNode { at, .. }
            | Unreached::Impostor { at, .. }
            | Unreached::Misnamed { at, .. }
            | Unreached::Unproved { at, .. }),
        ) => (at, other),
        Err(unreached) => return Err(unreached),
    };
    match ask_at(
        target,
        keys,
        None,
        (secret, me),
        deadline,
        request,
        (&[at], held),
    ) {
        Ok(again)
            if again
                .node
                .as_deref()
                .is_none_or(|node| Some(node) == target.node.as_deref()) =>
        {
            Ok(again)
        }
        _ => Err(other),
    }
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
}

impl Read for Driven {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

impl Write for Driven {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
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
/// and this machine's node `me` (a drive with no node is never admitted), within one deadline
/// `within` for the dial and every exchange after it. When the first address that answers is
/// not the target's bound node, the host name's other addresses are tried.
pub fn drive(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, &NodeIdentity),
    within: Duration,
) -> Result<Driven, Unreached> {
    let deadline = (Instant::now() + within, within);
    match drive_at(target, keys, target.addr, (secret, me), deadline, &[]) {
        Err(
            other @ (Unreached::OtherNode { at, .. }
            | Unreached::Impostor { at, .. }
            | Unreached::Unproved { at, .. }
            | Unreached::Unbound { at, .. }),
        ) => drive_at(target, keys, None, (secret, me), deadline, &[at]).map_err(|_| other),
        done => done,
    }
}

fn drive_at(
    target: &Target,
    keys: &Keyring,
    addr: Option<IpAddr>,
    (secret, me): (&FleetSecret, &NodeIdentity),
    (deadline, within): (Instant, Duration),
    avoid: &[SocketAddr],
) -> Result<Driven, Unreached> {
    let reached = dial_avoiding(
        &target.host,
        addr,
        target.port,
        (secret, Some(me)),
        FleetOp::Drive,
        (deadline, within),
        (avoid, resolve),
    )?;
    let at = reached.addr;
    let node = match answered_as(target, keys, at, reached.peer.as_ref())? {
        Some((node, true)) => node,
        claimed => {
            return Err(Unreached::Unbound {
                at,
                node: claimed.map(|(node, _)| node),
            });
        }
    };
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
    })
}

fn ask_at(
    target: &Target,
    keys: &Keyring,
    addr: Option<IpAddr>,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    (deadline, within): (Instant, Duration),
    request: &str,
    (avoid, held): (&[SocketAddr], &HeldAddrs),
) -> Result<Surveyed, Unreached> {
    let Answer {
        scope,
        addr,
        peer,
        answered,
        lines,
    } = walk(
        (&target.host, addr, target.port),
        (deadline, within),
        (avoid, resolve),
        |to| {
            held.ask(
                to,
                (deadline, within),
                || reach(to, (secret, me), FleetOp::Survey, (deadline, within)),
                |peer| answered_as(target, keys, to, peer),
                |transport| {
                    ask_lines(transport, Some(request), deadline, reply_head)
                        .map(|(_, lines)| lines)
                },
            )
        },
    )?;
    let lines = lines.map_err(|why| match why {
        Asked::Deadline => Unreached::Deadline(Stage::Answer, within),
        Asked::Failed(why) => Unreached::Reply(addr, why),
        Asked::Local(why) => Unreached::Local(why),
    })?;
    let (node, proven) = reply_node(answered, (&lines, addr), keys, proves_me(me, peer.as_ref()))?;
    Ok(Surveyed {
        scope,
        addr,
        node,
        proven,
        key: peer.map(|proof| proof.key),
        lines,
    })
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

#[cfg(test)]
#[path = "fleet_member_tests.rs"]
mod tests;
