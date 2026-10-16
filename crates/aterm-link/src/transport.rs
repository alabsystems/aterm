// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How the bridge reaches the broker: the three transports of §11.2's `serve`
//! synopsis, behind one erased stream type.
//!
//! ```text
//! aterm-link serve --broker <ep> [--tcp] [--key-file PATH]
//! ```
//!
//! * no flag: a Unix socket on this machine, scoped by the socket path's permissions.
//! * `--tcp`: plaintext TCP to `<host>:<port>`, §8.6's trusted-network-only setting;
//!   it says so on stderr every time.
//! * `--tcp --key-file <path>`: the same TCP inside astream's sealed record layer
//!   (XChaCha20-Poly1305 under a 32-byte pre-shared key; each record's AAD binds both
//!   hellos, the direction and the sequence, and a key-confirming record each way
//!   refuses a peer without the key inside the handshake, §8.6).
//!
//! `--handshake` and `--identity` (astream's ephemeral-X25519 and signed-DH
//! transports) are refused by name (`cli::parse`): §11.2 holds this crate's
//! third-party crypto to `chacha20poly1305` + `getrandom`, which the curve crates
//! would break, and `asb` serves no `identity` transport.
//!
//! The bridge must end a subscription from another thread (`recv` blocks without a
//! timeout, and a reconnect that left the old group subscription running would
//! deliver every record twice and could walk the commit backwards), so
//! [`astream_broker::connect`] makes a [`astream_broker::Closer`] from a duplicate of
//! the socket. This module keeps the policy: which transports a node may use.

use std::io;
use std::time::{Duration, Instant};

use astream_broker::Record;

/// Any byte stream a broker connection can ride. `Send` is a supertrait because each
/// subscription is pumped by its own thread.
pub use astream_broker::AnyStream as Stream;

/// Ends a subscription from outside its reading thread, and bounds a connection's
/// reads and writes. Past a deadline the connection is out of step (a late reply may
/// still arrive), so astream's client refuses every later request on it
/// (`BrokenPipe`) and the caller reconnects. A subscription wants `None`, since
/// parking in `recv` is its normal state, until the broker's first idle mark arms a
/// liveness bound through this same closer (`Subscription::recv_live`).
pub use astream_broker::Closer;

/// One broker connection with its transport erased: one code path per verb, not one
/// per wire.
pub type Conn = astream_broker::AnyClient;

/// Attach a whole capability ring to `conn` under one `Hello` with astream's
/// [`attach_all`](astream_broker::Client::attach_all): two round trips however many
/// grants the ring holds, the `Attach`es pipelined. An empty ring sends nothing (an
/// unguarded broker with no cap files).
///
/// # Errors
///
/// The socket, or the broker's first refusal in ring order (a
/// [`astream_broker::BrokerRefusal`] inside). The grants ahead of the refused one stay
/// attached, so a caller that needs the whole ring drops the connection, as every
/// caller here does.
pub fn attach_ring(conn: &mut Conn, ring: &[crate::bridge::Cap]) -> io::Result<()> {
    conn.attach_all(
        ring.iter()
            .map(|cap| (cap.grant.as_str(), cap.tag.as_slice())),
    )
    .map(drop)
}

/// [`attach_ring`] over every capability in `cap_files`, in file order. The files are
/// read on every call, so a connection opened after a re-mint attaches the new ring.
///
/// # Errors
///
/// A cap file that cannot be read, or [`attach_ring`]'s.
pub fn attach_cap_files(conn: &mut Conn, cap_files: &[String]) -> io::Result<()> {
    let mut ring = Vec::new();
    for path in cap_files {
        ring.extend(crate::bridge::read_cap_file(path)?);
    }
    attach_ring(conn, &ring)
}

/// Walk every row of a last-value face, paging on the resume cursor
/// ([`astream_broker::Client::last_walk`]).
///
/// The broker clamps `max` and bounds the index entries one request visits (§5.2),
/// so a short or empty page is not the end; only an empty `resume` is. `on_row` sees
/// each row as its page arrives, so a fleet-sized answer costs one page of memory;
/// an `on_row` that fails ends the walk.
///
/// # Errors
///
/// A broker or transport failure, a failing `on_row`, or a walk that did not end
/// within [`astream_broker::LAST_WALK_PAGES_MAX`] pages: a caller that cannot tell
/// "no rows" from "stopped looking" would report an empty fleet.
pub fn walk_last<F>(conn: &mut Conn, filter: &str, mut on_row: F) -> io::Result<()>
where
    F: FnMut(&Record) -> io::Result<()>,
{
    let (_, rest) = conn.last_walk(filter, "", u32::MAX, |row| {
        on_row(&row)?;
        Ok(astream_broker::Walk::Continue)
    })?;
    match rest {
        None => Ok(()),
        // Only a u32::MAX-row answer ends here; still never a short roster.
        Some(_) => Err(io::Error::other(format!(
            "last-value walk of {filter} did not end within {} rows",
            u32::MAX
        ))),
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
    /// Boxed so moving or cloning a config does not smear the key across the
    /// stack.
    Sealed(Box<[u8; 32]>),
}

impl std::fmt::Debug for Transport {
    /// Never prints the key: a logged `Config` must not leak the fleet's PSK.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
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

/// Open one connection under `deadline`, answering it and the closer that can end it.
///
/// The deadline bounds every step a broker could leave this caller parked in: the
/// TCP dial (each resolved address in turn), the sealed handshake, and every read and
/// write after them (`SO_RCVTIMEO`/`SO_SNDTIMEO`, set through the closer before the
/// first frame). A black-holed address, a broker that never answers, and one that
/// stopped reading mid-frame all fail `TimedOut`/`WouldBlock` after `deadline`. A
/// timed-out request leaves the stream unframed: drop the connection, do not retry
/// on it. A subscriber lifts only the read deadline once subscribed
/// (`closer.set_read_timeout(None)`); it writes nothing after its request. A Unix
/// dial is unbounded (std has no bounded one) and fails at once when nothing listens.
///
/// The only connect here, so no caller can forget the bound; the sealed wire is
/// refused by name in a build without it.
///
/// # Errors
///
/// The connect itself, `TimedOut` past the bound, the sealed handshake (a peer that
/// lacks the key fails here, not on the first verb), the descriptor duplication the
/// closer needs, or a zero `deadline` (`InvalidInput`).
pub fn connect(
    transport: &Transport,
    endpoint: &str,
    deadline: Duration,
) -> io::Result<(Conn, Closer)> {
    let wire = match transport {
        Transport::Unix => astream_broker::Transport::Unix,
        Transport::Tcp => astream_broker::Transport::Tcp,
        #[cfg(not(feature = "sealed"))]
        Transport::Sealed(_) => {
            // The variant stays so `--key-file` parses, `Debug` redacts and
            // `ls` names it; only the connect is absent without `sealed`.
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                SEALED_UNAVAILABLE,
            ));
        }
        #[cfg(feature = "sealed")]
        Transport::Sealed(key) => astream_broker::Transport::Sealed(key.clone()),
    };
    let (conn, closer) = astream_broker::connect(&wire, endpoint, Some(deadline))?;
    closer.set_read_timeout(Some(deadline))?;
    closer.set_write_timeout(Some(deadline))?;
    Ok((conn, closer))
}

/// The reconnect back-off floor and ceiling. §7: a bridge reconnects with jittered
/// back-off and opens its publisher connection before its drain connection, because
/// `MAX_CONNS` can refuse a herd and the publisher's `live inc+1` suppresses a fenced
/// will.
const RECONNECT_MIN: Duration = Duration::from_millis(100);
pub(crate) const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// How long a link must have stayed up for its loss to restart the back-off from
/// [`RECONNECT_MIN`] and be redialled at once. A link lost sooner is a failed attach:
/// a subscription the broker refused (astream answers a `Subscribe` only when it
/// delivers, so a refusal is the reader's first `recv` error, after the attach
/// reported `up`), or a broker that accepts and drops. Equal to the ceiling, so a
/// link that keeps dying that way dials at most once per [`RECONNECT_MAX`].
const LINK_STABLE: Duration = RECONNECT_MAX;

/// One broker client's reconnect back-off: doubling from [`RECONNECT_MIN`] to
/// [`RECONNECT_MAX`], each wait drawn from `[b/2, b]` (equal jitter, so a refused herd
/// does not come back in step), and reset only by a link that stayed up
/// [`LINK_STABLE`], never by a dial that merely succeeded (a refused subscription
/// would then redial at round-trip speed). Pure state: the caller passes the clock
/// and does the waiting.
#[derive(Debug)]
pub(crate) struct Backoff {
    next: Duration,
    up_since: Option<Instant>,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            next: RECONNECT_MIN,
            up_since: None,
        }
    }
}

impl Backoff {
    /// A dial or attach failed: how long to wait before the next one, which waits
    /// twice as long, up to the ceiling.
    pub(crate) fn failed(&mut self) -> Duration {
        let wait = random_u64().map_or(self.next, |r| jittered(self.next, r));
        self.next = (self.next * 2).min(RECONNECT_MAX);
        wait
    }

    /// The link came up at `now`.
    pub(crate) fn attached(&mut self, now: Instant) {
        self.up_since = Some(now);
    }

    /// The link that came up was lost at `now`: `None` when it had stayed up
    /// [`LINK_STABLE`] (the back-off restarts at the floor and the redial goes now),
    /// else the wait a failed attach gets ([`Self::failed`]). `None` too when no link
    /// was up, so one loss is waited out once.
    pub(crate) fn lost(&mut self, now: Instant) -> Option<Duration> {
        let up = self.up_since.take()?;
        if now.saturating_duration_since(up) >= LINK_STABLE {
            self.next = RECONNECT_MIN;
            None
        } else {
            Some(self.failed())
        }
    }
}

/// `b` jittered into `[b/2, b]` by `r`: integer-only, and never longer than
/// `b`.
fn jittered(b: Duration, r: u64) -> Duration {
    let half = b / 2;
    let span = u64::try_from((b - half).as_micros()).unwrap_or(u64::MAX);
    half + Duration::from_micros(r % span.saturating_add(1))
}

/// A jitter draw from the OS CSPRNG; `None` when it cannot be read, and the
/// caller then waits the whole `b` — never a shorter wait.
fn random_u64() -> Option<u64> {
    let mut raw = [0u8; 8];
    aterm_uds::rand::fill(&mut raw).ok()?;
    Some(u64::from_le_bytes(raw))
}

/// Read a `--key-file`: 64 hex characters, surrounding whitespace ignored — the
/// same shape and the same tolerance as `asb --key-file`, so one key file feeds
/// the broker and every bridge on the fleet.
///
/// The intermediate buffer is scrubbed: hygiene against a stray copy, not a
/// defence (the key lives on in the `Transport`).
///
/// # Errors
///
/// The file read, or contents that are not exactly 64 hex characters.
pub fn read_key_file(path: &str) -> io::Result<[u8; 32]> {
    let mut raw = std::fs::read(path)?;
    let text = raw.trim_ascii();
    let mut key = [0u8; 32];
    let parsed = aterm_codec::hex::decode_into(text, &mut key);
    raw.fill(0);
    std::hint::black_box(&raw);
    if parsed {
        Ok(key)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{path}: key must be 64 hex characters"),
        ))
    }
}

/// One error from [`read_key_file`], [`check_private`], [`read_private_key_file`] or
/// [`read_mint_secret`] as text naming `path` once: their own refusals lead with it,
/// and an OS error never carries one. A caller prefixes only its flag.
#[must_use]
pub fn describe(path: &str, e: &io::Error) -> String {
    if e.raw_os_error().is_some() {
        format!("{path}: {e}")
    } else {
        e.to_string()
    }
}

/// Whether this build carries the sealed TCP transport (the `sealed` feature).
/// Verbs that need it ask first and refuse with [`SEALED_UNAVAILABLE`], rather
/// than failing later with an `Unsupported` from a connect.
pub const SEALED: bool = cfg!(feature = "sealed");

/// What a default build says when a verb needs the sealed transport.
pub const SEALED_UNAVAILABLE: &str = "the sealed TCP transport is not in this build \
     (the `sealed` cargo feature); rebuild with \
     `targo --unverified build --release -p aterm --features sealed`";

/// Why this build cannot run a command line: `Some` only when `args` ask for
/// the sealed wire (`--tcp` and `--key-file`) in a build without it
/// ([`SEALED`]). `serve`, `ls` and `notify` ask before parsing, so this comes
/// before any complaint about a key file the build could not use anyway.
#[must_use]
pub fn sealed_flags_refusal(args: &[String]) -> Option<&'static str> {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    (!SEALED && has("--tcp") && has("--key-file")).then_some(SEALED_UNAVAILABLE)
}

/// Refuse a secret file (mint secret, pre-shared key, capability) anyone but its
/// owner can read: `mode & 0o077` must be zero. The refusal names the `chmod` rather
/// than fixing it silently, since a key that sat readable may already have been read.
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
                "{path} is mode {mode:04o}, not owner-only; run `chmod 600 {path}` \
                 and treat its contents as exposed"
            ),
        ))
    }
}

/// Read a mint secret: the one reader for every verb that mints or guards with a
/// secret (`aterm link broker --secret-file`, `aterm link mint`, `aterm fabric on`,
/// `mint-for`/`join`). The file must pass [`check_private`] and hold at least
/// [`crate::enable::SECRET_LEN`] bytes: HMAC takes a key of any length, so a short
/// one would mint well-formed capabilities that seal nothing. Its bytes are the
/// secret exactly as read, a trailing newline included, as `asb`'s `load_secret`
/// reads it: minter and verifier must agree byte for byte.
///
/// # Errors
///
/// A missing or unreadable file (`NotFound` when it is not there, which a
/// caller may answer with a hint), a mode with any group/other bit set
/// (`PermissionDenied`, naming the `chmod`), or a short secret (`InvalidData`).
pub fn read_mint_secret(path: &str) -> io::Result<Vec<u8>> {
    check_private(path)?;
    let secret = std::fs::read(path)?;
    if secret.len() < crate::enable::SECRET_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{path} holds {} bytes; a mint secret is at least {} (a short key mints \
                 capabilities that seal nothing)",
                secret.len(),
                crate::enable::SECRET_LEN
            ),
        ));
    }
    Ok(secret)
}

/// [`read_key_file`] behind [`check_private`]: how the broker and `aterm fabric
/// on|join` read a pre-shared key. The bridge's own `--key-file` keeps the plain
/// reader, so a fleet key that is not owner-only still dials.
///
/// # Errors
///
/// A key file that is not 0600, or not 64 hex characters.
pub fn read_private_key_file(path: &str) -> io::Result<[u8; 32]> {
    check_private(path)?;
    read_key_file(path)
}

/// A key as the key file spells it: 64 lowercase hex characters and a newline, the
/// shape [`read_key_file`] and `asb --key-file` read.
#[must_use]
pub fn key_file_text(key: &[u8; 32]) -> String {
    let mut s = aterm_codec::hex::encode_lower(key);
    s.push('\n');
    s
}

/// Whether every address `endpoint` (`<host>:<port>`) resolves to is loopback, the
/// only kind a TCP broker binds without `--allow-remote`. The unspecified `0.0.0.0`
/// and `[::]` listen on every interface, so they are not.
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

/// The address this host's own clients dial for a broker bound at `bind`: the same
/// family's loopback when `bind` is unspecified (`0.0.0.0:<p>` → `127.0.0.1:<p>`,
/// `[::]:<p>` → `[::1]:<p>`), which a connect cannot name; `bind` itself otherwise.
#[must_use]
pub fn dial_for_bind(bind: &str) -> String {
    match bind.rsplit_once(':') {
        Some(("0.0.0.0", port)) => format!("127.0.0.1:{port}"),
        Some(("[::]", port)) => format!("[::1]:{port}"),
        _ => bind.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key file is 64 hex characters and nothing else; a short, long or
    /// non-hex file is an error, never a zero-padded key.
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

    /// A connect's deadline bounds both directions: a `hello` nobody answers and a
    /// post bigger than the socket's buffers to a broker that stopped reading both
    /// fail at it. Each runs on its own thread behind the one-minute hang detector, so
    /// a missing deadline fails the test rather than hanging it.
    #[test]
    fn a_connects_deadline_ends_a_hello_nobody_answers_and_a_publish_nobody_reads() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("atlink-deadline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let sock = dir.join("s.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind");
        let path = sock.to_str().expect("utf8").to_string();
        let deadline = Duration::from_millis(300);
        for what in ["hello", "publish"] {
            let (mut conn, _closer) = connect(&Transport::Unix, &path, deadline).expect("connect");
            let (peer, _) = listener.accept().expect("accept");
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let served = if what == "hello" {
                    conn.hello().map(|_| ())
                } else {
                    // 256 KiB: the bridge's post cap, far past a Unix
                    // socket's buffers.
                    conn.publish(1, 1, "/f/t/x", &[0; 256 * 1024]).map(|_| ())
                };
                let _ = tx.send(served);
            });
            let err = rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .unwrap_or_else(|_| panic!("a {what} nobody serves outlived the deadline"))
                .expect_err("nobody serves it");
            assert!(
                matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ),
                "{what}: {err:?}"
            );
            drop(peer);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 0600 or tighter passes; any group/other bit is refused naming the
    /// `chmod`, and the checked key reader applies it before parsing.
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

    /// A mint secret is owner-only and at least 32 bytes, or refused with the reason,
    /// and is read byte for byte, a trailing newline included.
    #[test]
    fn a_mint_secret_is_owner_only_long_enough_and_read_byte_for_byte() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("atlink-mint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("mint.secret");
        let p = path.to_str().expect("utf8");
        let chmod = |mode| {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod")
        };
        assert_eq!(
            read_mint_secret(p).expect_err("absent").kind(),
            io::ErrorKind::NotFound
        );
        let mut bytes = vec![0x5a; 31];
        bytes.push(b'\n');
        std::fs::write(&path, &bytes).expect("write");
        chmod(0o644);
        let e = read_mint_secret(p).expect_err("0644");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains(&format!("chmod 600 {p}")), "{e}");
        chmod(0o600);
        assert_eq!(read_mint_secret(p).expect("0600, 32 bytes"), bytes);
        std::fs::write(&path, [0x5a; 31]).expect("write");
        let e = read_mint_secret(p).expect_err("short");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains("holds 31 bytes"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A key-file error names its path once, whichever way it failed.
    #[test]
    fn describe_names_the_path_once() {
        let dir = std::env::temp_dir().join(format!("atlink-describe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("k");
        let p = path.to_str().expect("utf8");
        let missing = read_key_file(p).expect_err("absent");
        std::fs::write(&path, "not hex").expect("write");
        let malformed = read_key_file(p).expect_err("malformed");
        for e in [missing, malformed] {
            let text = describe(p, &e);
            assert!(
                text.starts_with(&format!("{p}: ")) && text.matches(p).count() == 1,
                "{text}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only `--tcp --key-file`, and only in a build without the sealed wire, is
    /// refused before parsing: a Unix socket, plaintext TCP and a key without
    /// `--tcp` (which `parse` refuses itself) all pass.
    #[test]
    fn only_the_sealed_wire_in_a_build_without_it_is_refused() {
        let argv = |line: &str| line.split(' ').map(str::to_string).collect::<Vec<_>>();
        let base = "--fleet f1 --broker 127.0.0.1:7000";
        assert_eq!(sealed_flags_refusal(&argv(base)), None);
        assert_eq!(sealed_flags_refusal(&argv(&format!("{base} --tcp"))), None);
        assert_eq!(
            sealed_flags_refusal(&argv(&format!("{base} --key-file /none/k"))),
            None
        );
        let sealed = sealed_flags_refusal(&argv(&format!("{base} --tcp --key-file /none/k")));
        assert_eq!(sealed.is_some(), !SEALED);
        if let Some(why) = sealed {
            assert!(why.contains("`sealed` cargo feature"), "{why}");
        }
    }

    /// A minted key file is 64 lowercase hex and a newline, and reads back.
    #[test]
    fn a_minted_key_file_is_sixty_four_hex_and_reads_back() {
        let text = key_file_text(&[0xab; 32]);
        assert_eq!(text, format!("{}\n", "ab".repeat(32)));
        let mut out = [0u8; 32];
        assert!(aterm_codec::hex::decode_into(
            text.trim().as_bytes(),
            &mut out
        ));
        assert_eq!(out, [0xab; 32]);
    }

    /// Loopback vs unspecified binds, the address this host dials, the port.
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

    /// Equal jitter: every draw lies in `[b/2, b]`, both ends reachable.
    #[test]
    fn a_jittered_wait_lies_in_the_upper_half_of_the_back_off() {
        let b = RECONNECT_MAX;
        assert_eq!(jittered(b, 0), b / 2);
        let span = u64::try_from((b - b / 2).as_micros()).expect("fits");
        assert_eq!(jittered(b, span), b);
        for r in [1, 7, span + 1, u64::MAX / 3, u64::MAX] {
            let d = jittered(b, r);
            assert!(b / 2 <= d && d <= b, "{r}: {d:?}");
        }
    }

    /// A link lost right after it came up is a failed attach (it waits and doubles);
    /// only one that stayed up restarts the back-off at the floor and redials at once.
    #[test]
    fn only_a_link_that_stayed_up_resets_the_back_off() {
        let within = |d: Duration, b: Duration| b / 2 <= d && d <= b;
        let t0 = Instant::now();
        let mut b = Backoff::default();
        // Nothing was up: nothing to wait out twice.
        assert_eq!(b.lost(t0), None);
        b.attached(t0);
        let early = b
            .lost(t0 + Duration::from_millis(1))
            .expect("an early loss waits");
        assert!(within(early, RECONNECT_MIN), "{early:?}");
        let next = b.failed();
        assert!(within(next, RECONNECT_MIN * 2), "it doubled: {next:?}");
        // Up long enough: the redial goes now, from the floor again.
        b.attached(t0);
        assert_eq!(b.lost(t0 + LINK_STABLE), None);
        let floor = b.failed();
        assert!(within(floor, RECONNECT_MIN), "{floor:?}");
        // The ceiling holds, including when entropy is unavailable and each
        // failure waits the full delay.
        let draws: Vec<Duration> = (0..64).map(|_| b.failed()).collect();
        assert!(
            draws[8..].iter().all(|d| within(*d, RECONNECT_MAX)),
            "{draws:?}"
        );
    }

    /// A `Transport`'s `Debug` never carries the key.
    #[test]
    fn a_transports_debug_never_prints_the_key() {
        let t = Transport::Sealed(Box::new([0xde; 32]));
        let shown = format!("{t:?}");
        assert_eq!(shown, "tcp+sealed");
        assert!(!shown.contains("de"), "{shown}");
    }
}
