// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How the bridge reaches the broker: the three transports §11.2's `serve`
//! synopsis lists, behind one erased stream type.
//!
//! ```text
//! aterm-link serve --broker <ep> [--tcp] [--key-file PATH]
//! ```
//!
//! * no flag — a Unix socket, same machine. Reachability is scoped by the
//!   socket path's permissions and nothing crosses a wire.
//! * `--tcp` — PLAINTEXT TCP to `<host>:<port>`. §8.6's "trusted-network-only
//!   setting", and it says so on stderr every time: every message and presence
//!   row crosses in the clear.
//! * `--tcp --key-file <path>` — the same TCP inside astream's sealed record
//!   layer: XChaCha20-Poly1305 under a 32-byte pre-shared key, with each
//!   record's AAD binding both hellos, the direction and the sequence, and a
//!   key-confirming record each way so a peer without the key is refused INSIDE
//!   the handshake (§8.6).
//!
//! ## Not offered: `--handshake` / `--identity`
//!
//! astream also has ephemeral-X25519 (`handshake`) and signed-DH identity
//! (`identity`) transports, and §8.6 says a node SHOULD use `identity`. But
//! §11.2 pins this crate's third-party surface to "**exactly** the two vetted
//! crypto dependencies the `cap`/`aead` features already isolate — `sha2`, and
//! `chacha20poly1305` + `getrandom`", which the curve crates would break, and
//! `asb` serves no `identity` transport. So both flags are refused by name
//! (`cli::parse`).
//!
//! ## The erased stream and its closer
//!
//! The bridge must end a subscription from another thread — `recv` blocks with
//! no timeout, and a reconnect that left the old group subscription running
//! would deliver every record twice and could walk the commit backwards. So
//! astream's [`astream_broker::connect`] makes a [`astream_broker::Closer`] at
//! connect time from a duplicate of the underlying socket. What stays here is
//! policy: which transports a node may use, and refusing the sealed wire by
//! name in a build without it.

use std::io;

use astream_broker::Record;

/// Any byte stream a broker connection can ride — astream's [`AnyStream`](astream_broker::AnyStream).
/// `Send` is a supertrait because each subscription is pumped by its own thread.
pub use astream_broker::AnyStream as Stream;

/// Ends a subscription from outside its reading thread, and bounds a
/// connection's reads and writes — astream's [`Closer`](astream_broker::Closer).
/// Past a read deadline the stream is no longer framed (the late reply may
/// still arrive), so the caller drops the connection rather than retrying; a
/// subscription wants `None`, since parking in `recv` is its normal state.
pub use astream_broker::Closer;

/// One broker connection, with its transport erased so the bridge has ONE code
/// path per verb rather than one per wire.
pub type Conn = astream_broker::AnyClient;

/// Walk every row of a last-value face, paging on the resume cursor.
///
/// The broker clamps `max` AND bounds how many index entries one request may
/// visit (§5.2, [`astream_broker::Client::last_page`]), so a short page — an
/// empty one included — is not the end; only an empty `resume` is. The walk is
/// astream's [`astream_broker::Client::last_walk`]; a page is at most the
/// broker's `LAST_PAGE_MAX` rows.
///
/// Streaming: `on_row` sees each row as its page arrives, so a fleet-sized
/// answer costs one page of memory. An `on_row` that fails ends the walk.
///
/// # Errors
///
/// Any broker or transport failure, an `on_row` that fails, or a walk that did
/// not end within [`astream_broker::LAST_WALK_PAGES_MAX`] pages — a caller that
/// cannot tell "no rows" from "stopped looking" reports an empty fleet while a
/// node escalates.
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

/// Open one connection, answering it and the closer that can end it.
///
/// astream's [`astream_broker::connect`] does the work; this maps the fabric's
/// transport onto it and refuses the sealed wire BY NAME in a build without it.
///
/// # Errors
///
/// The connect itself, the sealed handshake (a peer that lacks the key fails
/// HERE, not on the first verb), or the descriptor duplication the closer needs.
pub fn connect(transport: &Transport, endpoint: &str) -> io::Result<(Conn, Closer)> {
    connect_within(transport, endpoint, None)
}

/// [`connect`] with a bound on ESTABLISHING the connection (every resolved TCP
/// address in turn; on the sealed wire it also replaces the handshake's own
/// deadline). A Unix connect is not bounded — std has no bounded one.
///
/// # Errors
///
/// As [`connect`], and `TimedOut` past the bound.
pub fn connect_within(
    transport: &Transport,
    endpoint: &str,
    timeout: Option<std::time::Duration>,
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
    astream_broker::connect(&wire, endpoint, timeout)
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

/// One error from [`read_key_file`], [`check_private`] or
/// [`read_private_key_file`] as text that names `path` ONCE: their own refusals
/// already lead with it, and an OS error (a missing file, a denied `stat`)
/// never carries one. A caller prefixes only its flag (`--key-file {…}`).
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

/// Refuse a SECRET file anyone but its owner can read: a mint secret, a
/// pre-shared key, a capability file. `mode & 0o077` must be zero — 0600 or
/// tighter. The refusal names the `chmod` rather than fixing it silently: a key
/// that sat readable may already have been read.
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
    let mut s = aterm_codec::hex::encode_lower(key);
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

    /// A read deadline set through the closer reaches the socket under the
    /// erased stream: a `hello` nobody answers fails at the deadline (what
    /// `fabric=stalled reason=no-ack` rests on).
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
        // Not an instant refusal; the upper bound is a hang detector.
        assert!(
            waited >= std::time::Duration::from_millis(250)
                && waited < std::time::Duration::from_secs(60),
            "the deadline, not a hang and not an instant refusal: {waited:?}"
        );
        drop(peer);
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

    /// A `Transport`'s `Debug` never carries the key.
    #[test]
    fn a_transports_debug_never_prints_the_key() {
        let t = Transport::Sealed(Box::new([0xde; 32]));
        let shown = format!("{t:?}");
        assert_eq!(shown, "tcp+sealed");
        assert!(!shown.contains("de"), "{shown}");
    }
}
