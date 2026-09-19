// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HOW THE BRIDGE REACHES THE BROKER — the three transports §11.2's `serve`
//! synopsis lists, behind one erased stream type.
//!
//! ```text
//! aterm-link serve --broker <ep> [--tcp] [--key-file PATH]
//! ```
//!
//! * no flag — a Unix socket, same machine. Reachability is scoped by the
//!   socket path's permissions and nothing crosses a wire.
//! * `--tcp` — PLAINTEXT TCP to `<host>:<port>`. §8.6's "trusted-network-only
//!   setting", and it says so on stderr every time, because a cross-host fabric
//!   on plaintext TCP carries every keystroke and every message in the clear.
//! * `--tcp --key-file <path>` — the same TCP inside astream's sealed record
//!   layer: XChaCha20-Poly1305 under a 32-byte pre-shared key, with each
//!   record's AAD binding both hellos, the direction and the sequence, and a
//!   key-confirming record each way so a peer without the key is refused INSIDE
//!   the handshake (§8.6). This is what A5 runs on.
//!
//! ## The two transports this module does NOT offer, and why
//!
//! astream has built two more (its Rungs 7 and 8): an ephemeral-X25519 key
//! agreement (`handshake`) and a mutual signed-DH with static per-peer identity
//! (`identity`). §8.6 says a fabric node SHOULD use `identity`. §11.2 pins this
//! crate's third-party surface to "**exactly** the two vetted crypto
//! dependencies the `cap`/`aead` features already isolate — `sha2`, and
//! `chacha20poly1305` + `getrandom`". Both cannot hold: `handshake` adds
//! `x25519-dalek`, `identity` adds `ed25519-dalek` on top, and `asb` itself
//! exposes no `identity` transport to serve one against. So this rung
//! implements the two flags §11.2's own synopsis names, and `--handshake` /
//! `--identity` are refused BY NAME with that reason rather than silently
//! absent — see the deviation note in `serve`'s usage.
//!
//! ## The erased stream, and why the closer is built here
//!
//! `Client<S>` and `Subscription<S>` are generic over the byte stream, but
//! [`astream_broker::Subscription::closer`] exists only for the concrete
//! `UnixStream` case. The bridge MUST be able to end a subscription from
//! another thread — `recv` blocks in a socket read with no timeout, and a
//! reconnect that left the old group subscription running would deliver every
//! record twice and could walk the commit backwards. So the closer is made at
//! CONNECT time, from a duplicate of the socket underneath whatever wrapper the
//! transport put on top of it, and it outlives the `subscribe` that consumes
//! the client.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;

use astream_broker::{Client, Record};

/// Any byte stream a broker connection can ride. `Send` is a supertrait because
/// each subscription is pumped by its own thread.
pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// One broker connection, with its transport erased so the bridge has ONE code
/// path per verb rather than one per wire.
pub type Conn = Client<Box<dyn Stream>>;

/// How many rows one `Last` page asks the broker for.
///
/// The broker CLAMPS this to its own `LAST_PAGE_MAX`, so asking for more is not
/// an error and asking for fewer only costs round trips — which is exactly why
/// it must not be read as "the answer fits in one page".
pub const LAST_PAGE_ROWS: u32 = 256;

/// The most pages one last-value walk may take before it is called a FAILURE.
///
/// A liveness bound, not a size one: the resume cursor advances every page, so a
/// walk that has not finished in this many has met something pathological, and
/// the honest answer to a caller that must not read absence as evidence is an
/// error rather than a short list. At [`LAST_PAGE_ROWS`] a page the ceiling is a
/// million rows.
pub const LAST_PAGES_MAX: usize = 4096;

/// WALK EVERY ROW OF A LAST-VALUE FACE, paging on the RESUME CURSOR — the one
/// place in this crate that knows how a `Last` answer ends.
///
/// §5.2 and [`astream_broker::Client::last_page`]'s own doc state the rule
/// literally: the broker clamps `max` AND bounds how many index entries one
/// request may VISIT, matched or not, so a page shorter than `max` — AN EMPTY
/// ONE INCLUDED — is not the end of the answer. Only an empty `resume` is.
///
/// [`astream_broker::Client::last`] is the wrong verb for a walk and cannot be
/// made right by its caller: it is `last_page(..).map(|(page, mark, _)| ..)`,
/// so the cursor that carries the answer is thrown away before the caller sees
/// it, and the only cursor left to page on is the last ROW — which an empty page
/// does not have. Every reader in this crate that pages a `Last` face calls
/// THIS, and the reason it is here rather than in one of them is that the crate
/// has now paid twice for the same rule being restated per reader: four bridge
/// readers in round 2, and `aterm-link ls` — §9.3's cross-host escalation view,
/// where a silently short answer is a missed 3 a.m. escalation — in round 3.
///
/// STREAMING, not collecting: `on_row` sees each row as its page arrives, so a
/// fleet-sized answer costs one page of memory rather than the whole roster.
/// An `on_row` that fails ends the walk with its error.
///
/// # Errors
///
/// Any broker or transport failure, an `on_row` that fails, or a walk that did
/// not FINISH within [`LAST_PAGES_MAX`] pages — because a caller that cannot
/// tell "no rows" from "I stopped looking" is the caller that reports an empty
/// fleet while a node is escalating.
pub fn walk_last<F>(conn: &mut Conn, filter: &str, mut on_row: F) -> io::Result<()>
where
    F: FnMut(&Record) -> io::Result<()>,
{
    let mut after = String::new();
    for _ in 0..LAST_PAGES_MAX {
        let (page, _, resume) = conn.last_page(filter, &after, LAST_PAGE_ROWS)?;
        for row in &page {
            on_row(row)?;
        }
        if resume.is_empty() {
            return Ok(());
        }
        after = resume;
    }
    Err(io::Error::other(format!(
        "the last-value walk of {filter} did not finish within {LAST_PAGES_MAX} pages"
    )))
}

/// Ends a subscription from outside its reading thread by shutting the socket
/// underneath it down in both directions: the parked `recv` returns `Ok(None)`
/// and the broker sees the peer go away. Idempotent.
///
/// It is also the ONE handle to the socket under the erased stream, so it is
/// where a read deadline is set: `Conn` is `Client<Box<dyn Stream>>` and the
/// socket cannot be reached through it, while a bridge must bound how long a
/// request may wait for the broker's answer ([`Closer::set_read_timeout`]) — a
/// broker that accepts the connection and never acks is otherwise a bridge
/// parked forever in `attach`, reporting nothing.
pub struct Closer {
    close: Box<dyn Fn() + Send>,
    timeout: Box<dyn Fn(Option<std::time::Duration>) -> io::Result<()> + Send>,
}

impl Closer {
    /// Shut the connection down.
    pub fn close(&self) {
        (self.close)();
    }

    /// Bound how long a read on this connection parks with nothing arriving —
    /// `SO_RCVTIMEO` on the socket under whatever wrapper the transport put on
    /// top, so it applies to every frame the `Conn` reads from now on. Past it
    /// the read fails `WouldBlock`/`TimedOut` and, on a request/reply
    /// connection, the stream is no longer framed (the late reply may still
    /// arrive): the caller drops the connection, it does not retry on it. A
    /// subscription that has been opened wants `None` here, because parking in
    /// `recv` with nothing to deliver is its normal state.
    ///
    /// # Errors
    ///
    /// The `setsockopt`.
    pub fn set_read_timeout(&self, d: Option<std::time::Duration>) -> io::Result<()> {
        (self.timeout)(d)
    }

    fn unix(dup: UnixStream) -> io::Result<Self> {
        let timeout_dup = dup.try_clone()?;
        Ok(Self {
            close: Box::new(move || {
                let _ = dup.shutdown(std::net::Shutdown::Both);
            }),
            timeout: Box::new(move |d| timeout_dup.set_read_timeout(d)),
        })
    }

    fn tcp(dup: TcpStream) -> io::Result<Self> {
        let timeout_dup = dup.try_clone()?;
        Ok(Self {
            close: Box::new(move || {
                let _ = dup.shutdown(std::net::Shutdown::Both);
            }),
            timeout: Box::new(move |d| timeout_dup.set_read_timeout(d)),
        })
    }
}

/// How the bridge reaches the broker.
#[derive(Clone)]
pub enum Transport {
    /// A Unix socket path.
    Unix,
    /// Plaintext TCP — a trusted network only.
    Tcp,
    /// TCP inside the XChaCha20-Poly1305 sealed record layer, under a PSK.
    ///
    /// Boxed so the 32-byte key is not copied into every clone of the config,
    /// and so `Transport` stays small enough that moving it is not a secret
    /// smeared across the stack.
    Sealed(Box<[u8; 32]>),
}

impl std::fmt::Debug for Transport {
    /// NEVER prints the key. A `Config` is `Debug`, and a bridge that logged its
    /// own config on a bad day would put the fleet's pre-shared key in a file.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Transport::Unix => "unix",
            Transport::Tcp => "tcp",
            Transport::Sealed(_) => "tcp+sealed",
        })
    }
}

impl Transport {
    /// The word `ls` and the usage line use for this transport.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Transport::Unix => "unix",
            Transport::Tcp => "tcp",
            Transport::Sealed(_) => "tcp+sealed",
        }
    }
}

/// Open one connection, answering it and the closer that can end it.
///
/// # Errors
///
/// The connect itself, the sealed handshake (a peer that lacks the key fails
/// HERE, not on the first verb), or the descriptor duplication the closer needs.
pub fn connect(transport: &Transport, endpoint: &str) -> io::Result<(Conn, Closer)> {
    match transport {
        Transport::Unix => {
            let s = UnixStream::connect(endpoint)?;
            let closer = Closer::unix(s.try_clone()?)?;
            Ok((Client::from_stream(Box::new(s)), closer))
        }
        Transport::Tcp => {
            let s = TcpStream::connect(endpoint)?;
            s.set_nodelay(true)?;
            let closer = Closer::tcp(s.try_clone()?)?;
            Ok((Client::from_stream(Box::new(s)), closer))
        }
        #[cfg(not(feature = "sealed"))]
        Transport::Sealed(key) => {
            // REFUSED BY NAME, WITH THE REASON — the idiom `main.rs` already uses
            // for `--handshake`/`--identity`. The variant stays in the enum so
            // `--key-file` still parses, `Debug` still redacts it and `ls` still
            // prints the transport's name; only the CONNECT is absent, because
            // `Client::connect_tcp_sealed` lives behind the `sealed` feature and
            // a default build does not enable it. This crate's Cargo.toml says
            // why a shipped binary carries no cipher tree.
            let _ = (key, endpoint);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                SEALED_UNAVAILABLE,
            ))
        }
        #[cfg(feature = "sealed")]
        Transport::Sealed(key) => {
            let sealed = Client::connect_tcp_sealed(endpoint, **key)?.into_stream();
            // The DUP IS OF THE SOCKET, not of the sealed wrapper: shutting the
            // socket down is what unparks the reader, and a read deadline set on
            // it lands under the record layer, which resumes a partial record
            // exactly as a partial frame. A second `SealedStream` over the same
            // socket would be a second record-layer sequence and is exactly what
            // must not exist.
            let closer = Closer::tcp(sealed.get_ref().try_clone()?)?;
            Ok((Client::from_stream(Box::new(sealed)), closer))
        }
    }
}

/// Read a `--key-file`: 64 hex characters, surrounding whitespace ignored — the
/// same shape and the same tolerance as `asb --key-file`, so one key file feeds
/// the broker and every bridge on the fleet.
///
/// The intermediate buffer is scrubbed before it is dropped. That is not a
/// defence against a memory-reading attacker (the key lives on in the
/// `Transport` for the life of the process); it is hygiene against the copy
/// nobody meant to keep.
///
/// # Errors
///
/// The file read, or contents that are not exactly 64 hex characters.
pub fn read_key_file(path: &str) -> io::Result<[u8; 32]> {
    let mut raw = std::fs::read(path)?;
    let text = raw.trim_ascii();
    let mut key = [0u8; 32];
    let parsed = decode_hex(text, &mut key);
    raw.fill(0);
    std::hint::black_box(&raw);
    if parsed {
        Ok(key)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{path}: the sealed transport's key is 64 hex characters (32 bytes)"),
        ))
    }
}

/// Whether THIS build carries the sealed TCP transport — the `sealed` cargo
/// feature (this crate's Cargo.toml says why it is off by default). Every verb
/// that needs it asks here first and refuses by name with
/// [`SEALED_UNAVAILABLE`] when it is not, rather than parsing its flags and
/// failing later with an `Unsupported` from a connect.
pub const SEALED: bool = cfg!(feature = "sealed");

/// What a default build says when a verb needs the sealed transport: the
/// feature, the rebuild, and why the shipped binary does not carry it.
pub const SEALED_UNAVAILABLE: &str = "the sealed TCP transport is not in this build: it is \
     the `sealed` cargo feature, off by default and off in the shipped `aterm` (it compiles \
     astream-aead's chacha20poly1305 + getrandom tree, which a one-host fleet never opens) — \
     rebuild with `targo --unverified build --release -p aterm --features sealed` \
     (or `-p aterm-link --features sealed` for the `aterm-link` binary alone)";

/// Refuse a SECRET file anyone but its owner can read: a mint secret, a
/// pre-shared key, a capability file. `mode & 0o077` must be zero — 0600 or
/// tighter — and the refusal names the `chmod` that fixes it rather than
/// fixing it silently, because a key that sat world-readable may already have
/// been read and the operator should know that it did.
///
/// # Errors
///
/// The `stat`, or a mode with any group/other bit set.
pub fn check_private(path: &str) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{path} is mode {mode:04o}: a secret file must be readable by its owner alone \
                 (0600) — `chmod 600 {path}`, and treat what it holds as already seen if other \
                 users share this machine"
            ),
        ))
    }
}

/// [`read_key_file`] behind [`check_private`]: what the broker and `aterm
/// fabric on|join` read a pre-shared key with. The bridge's own `--key-file`
/// keeps the plain reader, so a fleet whose key predates this check still
/// dials.
///
/// # Errors
///
/// A key file that is not 0600, or not 64 hex characters.
pub fn read_private_key_file(path: &str) -> io::Result<[u8; 32]> {
    check_private(path)?;
    read_key_file(path)
}

/// A key as the key FILE spells it: 64 lowercase hex characters and a newline —
/// the shape [`read_key_file`] and `asb --key-file` read.
#[must_use]
pub fn key_file_text(key: &[u8; 32]) -> String {
    let mut s: String = key.iter().map(|b| format!("{b:02x}")).collect();
    s.push('\n');
    s
}

/// Whether every address `endpoint` (`<host>:<port>`) resolves to is a
/// LOOPBACK address — the only kind a TCP broker binds without
/// `--allow-remote`. `0.0.0.0` and `[::]` are the unspecified addresses, which
/// listen on EVERY interface, so they are not loopback.
///
/// # Errors
///
/// An endpoint that does not resolve, or resolves to nothing.
pub fn is_loopback_endpoint(endpoint: &str) -> io::Result<bool> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<std::net::SocketAddr> = endpoint.to_socket_addrs()?.collect();
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{endpoint} resolves to no address"),
        ));
    }
    Ok(addrs.iter().all(|a| a.ip().is_loopback()))
}

/// The port of a `<host>:<port>` endpoint, when it has one that parses.
#[must_use]
pub fn endpoint_port(endpoint: &str) -> Option<u16> {
    endpoint.rsplit_once(':')?.1.parse().ok()
}

/// The address THIS host's own clients dial for a broker bound at `bind`: the
/// loopback address of the same family when `bind` is an unspecified address
/// (`0.0.0.0:<p>` → `127.0.0.1:<p>`, `[::]:<p>` → `[::1]:<p>`), which a bind
/// accepts on every interface but a connect cannot name; `bind` itself
/// otherwise.
#[must_use]
pub fn dial_for_bind(bind: &str) -> String {
    match bind.rsplit_once(':') {
        Some(("0.0.0.0", port)) => format!("127.0.0.1:{port}"),
        Some(("[::]", port)) => format!("[::1]:{port}"),
        _ => bind.to_string(),
    }
}

/// Decode exactly `out.len() * 2` hex characters into `out`.
fn decode_hex(text: &[u8], out: &mut [u8]) -> bool {
    if text.len() != out.len() * 2 {
        return false;
    }
    let (pairs, _) = text.as_chunks::<2>();
    for (byte, pair) in out.iter_mut().zip(pairs) {
        let hi = (pair[0] as char).to_digit(16);
        let lo = (pair[1] as char).to_digit(16);
        let (Some(hi), Some(lo)) = (hi, lo) else {
            return false;
        };
        // Both digits are < 16, so the sum is < 256.
        *byte = u8::try_from(hi * 16 + lo).unwrap_or(0);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key file is 64 hex characters and nothing else. A short, long or
    /// non-hex file is an ERROR rather than a silently zero-padded key — a
    /// bridge that ran on a key nobody minted would fail its handshake against
    /// every peer and look like a network problem.
    #[test]
    fn a_key_file_is_exactly_sixty_four_hex_characters() {
        let dir = std::env::temp_dir().join(format!("atlink-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("k");
        let p = path.to_str().expect("utf8");

        std::fs::write(&path, format!("{}\n", "ab".repeat(32))).expect("write");
        assert_eq!(read_key_file(p).expect("read"), [0xab; 32]);
        // Whitespace around it is fine; a short one, a long one and a non-hex
        // one are not.
        std::fs::write(&path, format!("  {}  \n", "00".repeat(32))).expect("write");
        assert_eq!(read_key_file(p).expect("read"), [0x00; 32]);
        std::fs::write(&path, "ab").expect("write");
        assert!(read_key_file(p).is_err());
        std::fs::write(&path, "zz".repeat(32)).expect("write");
        assert!(read_key_file(p).is_err());
        std::fs::write(&path, "ab".repeat(33)).expect("write");
        assert!(read_key_file(p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE READ DEADLINE REACHES THE SOCKET UNDER THE ERASED STREAM. A listener
    /// that accepts and never answers used to park a `hello` for ever; with the
    /// deadline set through the closer's dup, the same `hello` fails
    /// `WouldBlock`/`TimedOut` at the deadline — which is what turns "a broker
    /// that accepts and never acks" into `fabric=stalled reason=no-ack` rather
    /// than a bridge that reports nothing for the life of the process.
    #[test]
    fn a_read_deadline_set_through_the_closer_ends_a_hello_nobody_answers() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("atlink-deadline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let sock = dir.join("s.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind");
        let (mut conn, closer) =
            connect(&Transport::Unix, sock.to_str().expect("utf8")).expect("connect");
        let (peer, _) = listener.accept().expect("accept");
        closer
            .set_read_timeout(Some(std::time::Duration::from_millis(300)))
            .expect("deadline");
        let started = std::time::Instant::now();
        let err = conn.hello().expect_err("nobody answers");
        let waited = started.elapsed();
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "{err:?}"
        );
        assert!(
            waited >= std::time::Duration::from_millis(250)
                && waited < std::time::Duration::from_secs(5),
            "the deadline, not a hang and not an instant refusal: {waited:?}"
        );
        drop(peer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ROUND 16'S SECRET-FILE RULE: 0600 or tighter passes, any group/other
    /// bit is refused with the `chmod` that fixes it — and the checked key
    /// reader applies it before it parses a byte.
    #[test]
    fn a_secret_file_other_users_can_read_is_refused_naming_the_chmod() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("atlink-priv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("k");
        let p = path.to_str().expect("utf8");
        std::fs::write(&path, key_file_text(&[0x5a; 32])).expect("write");
        for (mode, ok) in [(0o600, true), (0o400, true), (0o640, false), (0o604, false)] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
            assert_eq!(check_private(p).is_ok(), ok, "{mode:o}");
            assert_eq!(read_private_key_file(p).is_ok(), ok, "{mode:o}");
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let e = read_private_key_file(p).expect_err("0644").to_string();
        assert!(
            e.contains("0644") && e.contains(&format!("chmod 600 {p}")),
            "{e}"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        assert_eq!(read_private_key_file(p).expect("read"), [0x5a; 32]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE KEY FILE A HOST MINTS reads back through the reader every host and
    /// the broker use: 64 lowercase hex and a newline.
    #[test]
    fn a_minted_key_file_is_sixty_four_hex_and_reads_back() {
        let text = key_file_text(&[0xab; 32]);
        assert_eq!(text, format!("{}\n", "ab".repeat(32)));
        let mut out = [0u8; 32];
        assert!(decode_hex(text.trim().as_bytes(), &mut out));
        assert_eq!(out, [0xab; 32]);
    }

    /// LOOPBACK, UNSPECIFIED, AND WHAT A CLIENT DIALS: 127.0.0.1 and ::1 are
    /// loopback; 0.0.0.0 is not (it listens everywhere); a bind on it is
    /// dialed on loopback by this host's own bridges; the port is read off the
    /// end.
    #[test]
    fn loopback_binds_and_the_address_this_host_dials() {
        assert!(is_loopback_endpoint("127.0.0.1:7000").expect("resolves"));
        assert!(is_loopback_endpoint("[::1]:7000").expect("resolves"));
        assert!(!is_loopback_endpoint("0.0.0.0:7000").expect("resolves"));
        assert!(!is_loopback_endpoint("[::]:7000").expect("resolves"));
        assert!(is_loopback_endpoint("no-port-here").is_err());
        assert_eq!(dial_for_bind("0.0.0.0:7000"), "127.0.0.1:7000");
        assert_eq!(dial_for_bind("[::]:7000"), "[::1]:7000");
        assert_eq!(dial_for_bind("10.0.0.5:7000"), "10.0.0.5:7000");
        assert_eq!(dial_for_bind("127.0.0.1:7000"), "127.0.0.1:7000");
        assert_eq!(endpoint_port("h1:7000"), Some(7000));
        assert_eq!(endpoint_port("[::1]:0"), Some(0));
        assert_eq!(endpoint_port("h1"), None);
        assert_eq!(endpoint_port("h1:x"), None);
    }

    /// A `Transport`'s `Debug` NEVER carries the key. `Config` derives `Debug`,
    /// and one eprintln of a config on a bad day would put the fleet's
    /// pre-shared key in a log file.
    #[test]
    fn a_transports_debug_never_prints_the_key() {
        let t = Transport::Sealed(Box::new([0xde; 32]));
        let shown = format!("{t:?}");
        assert_eq!(shown, "tcp+sealed");
        assert!(!shown.contains("de"), "{shown}");
    }
}
