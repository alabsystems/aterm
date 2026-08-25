// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-net** — layer L3 of the RFC "The Reactive Surface": carrying the
//! control protocol (read + drive + `await`) over a NETWORK boundary so one
//! aterm can drive another on a different host, byte-identically to local.
//!
//! ## Why this needs no astream to *drive* a remote aterm
//!
//! The design review's key correction: predicate evaluation runs on the
//! **authoritative aterm surface** (the host that owns the engine), never on a
//! subset-diverging remote fold. So "drive a remote aterm" is just **relaying the
//! control protocol over a network transport** — the remote host runs the
//! watchers; the driver sends `await idle`/`send`/`key` and reads back. No
//! astream fold is on the critical path. astream remains the OPTIONAL record/
//! replay format for an *offline observer* and is the one genuinely external
//! piece (a sibling repo, not on disk) — out of scope here.
//!
//! ## Status: built, tested, and wired into the listener (opt-in)
//!
//! Every piece below is built and tested (unit + end-to-end over real
//! loopback-TLS). The LISTENER side is wired into `aterm-gui`'s control server via
//! `net_listen` — **secure-default-OFF**: it binds only when an operator sets
//! `ATERM_NET_LISTEN`/`_CERT`/`_KEY` (those selectors are deny-listed, so a nested
//! aterm never inherits them). The DRIVER side ([`dial_and_relay`](drive::dial_and_relay))
//! ships as the `dial <name>` control verb (`aterm_gui::control::try_net_dial` ->
//! `net_connections::dial_relay` -> `dial_and_relay`), and `aterm_agent::RelayClient`
//! drives a `Turn` over that relay end-to-end (see aterm-agent's
//! `remote_turn_loopback_tls` test); the astream record codec for an OFFLINE observer
//! remains the one genuinely external follow-up. The pieces:
//!
//! 1. [`Transport`] — generalizes `proxy.rs`'s `UnixStream::connect` dial to any
//!    connected channel, yielding **owned** read/write halves (TLS has no
//!    `UnixStream::try_clone`, so the relay owns its halves — the teardown pitfall
//!    the review flagged). [`LoopbackTransport`] is the 0-hop case; [`tls`] is the
//!    real TLS 1.3 transport and [`drive`] composes both ends of the network drive.
//! 2. [`pump`] — the transport-agnostic byte relay (the `connect_and_relay` core)
//!    that carries any verb's framing verbatim, incl. binary.
//! 3. [`channel_bind`] / [`verify_presented`] — the capability binding that
//!    replaces the local same-uid `SO_PEERCRED` check, which has **no network
//!    analog**. The presented secret is `HMAC-SHA256(edge_token, channel_exporter)`
//!    (the token is the MAC key), verified in constant time, so a token captured
//!    on one connection **cannot replay** on another (a different exporter → a
//!    different tag the verifier rejects) and cannot be forged without the token.
//!    Over the TLS 1.3 exporter ([`TlsTransport`](tls::TlsTransport), RFC 5705) it
//!    resists even an active MITM. Proven by `channel_bind_model` +
//!    `net_capability_grant_model` (`aterm-spec`).
//! 4. [`RemoteEndpoint`] — the pinned `(host, sid, nonce, fingerprint)` a dialer
//!    checks before presenting the token (the cert fingerprint is TLS-enforced; the
//!    nonce rebind guard is the dialer's responsibility — see [`RemoteEndpoint`]).
//! 5. [`RemoteOp::DialRemote`] — the object-capability gating WHICH remote endpoint
//!    a node may dial (the local `Scope::Owner` is all-or-nothing).

use std::io::{self, Read, Write};

// HMAC-SHA256 (first-party, `aterm-digest`): the channel-binding MAC. The edge
// token is the key.
use aterm_digest::HmacSha256;
use aterm_session::{EdgeToken, hex_nibble};
use aterm_uds::CtlStream;

/// TLS 1.3 transport (rustls) with the RFC 5705 keying-material exporter that
/// binds the capability to the channel.
pub mod tls;

/// The network drive — both ends ([`serve`](drive::serve)/[`dial_and_relay`]
/// (drive::dial_and_relay)) composed from the transport, the channel-bound
/// capability, and the relay. Secure-default-OFF: nothing binds without an
/// explicit operator-stood-up listener.
pub mod drive;

/// A connected network channel for the control protocol. Generalizes the local
/// `UnixStream::connect` dial: the relay takes OWNED halves (no `try_clone`
/// dependency), so a TLS/QUIC transport drops in unchanged.
pub trait Transport {
    /// Consume the transport into its owned read and write halves.
    fn split(self: Box<Self>) -> (Box<dyn Read + Send>, Box<dyn Write + Send>);
    /// The per-connection exporter that binds a capability to THIS channel (the
    /// TLS keying material in production; a per-connection nonce here). Distinct
    /// per connection, so a token bound to one channel is useless on another.
    fn exporter(&self) -> Vec<u8>;
}

/// The 0-hop transport: a connected `CtlStream` pair (on Unix, a plain std
/// `UnixStream` socketpair). The same `pump` and binding run over it as over a
/// real network transport — local is the trivial case of remote, by
/// construction.
pub struct LoopbackTransport {
    stream: CtlStream,
    exporter: Vec<u8>,
}

impl LoopbackTransport {
    /// A connected loopback pair `(a, b)`, each carrying the SAME channel
    /// exporter `exporter` (the two ends of one channel share keying material).
    #[must_use]
    pub fn pair(exporter: Vec<u8>) -> (Self, Self) {
        let (a, b) = CtlStream::pair().expect("socketpair");
        (
            Self {
                stream: a,
                exporter: exporter.clone(),
            },
            Self {
                stream: b,
                exporter,
            },
        )
    }
}

impl Transport for LoopbackTransport {
    fn split(self: Box<Self>) -> (Box<dyn Read + Send>, Box<dyn Write + Send>) {
        let w = self.stream.try_clone().expect("clone loopback");
        (Box::new(self.stream), Box::new(w))
    }
    fn exporter(&self) -> Vec<u8> {
        self.exporter.clone()
    }
}

/// Relay bytes from `reader` to `writer` until EOF — the transport-agnostic core
/// of `connect_and_relay`, carrying any verb's framing (status lines, `OK <n>`
/// bodies, `subscribe` push frames, the `bytes` stream, binary) verbatim.
///
/// # Errors
/// Propagates the first I/O error from either half.
pub fn pump<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> io::Result<u64> {
    let mut buf = [0u8; 32 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(total);
        }
        writer.write_all(&buf[..n])?;
        writer.flush()?;
        total += n as u64;
    }
}

/// The channel-bound presented secret: `HMAC-SHA256(edge_token, channel_exporter)`.
/// The edge token is the HMAC KEY (the unforgeable secret) and the channel
/// exporter (TLS RFC 5705 keying material — see [`TlsTransport`]) is the message,
/// so the output is a MAC that depends on BOTH and cannot be produced without the
/// token. Binding it to the connection's exporter means a value captured on one
/// channel is rejected on any other — the network replacement for the same-uid
/// peer check, and (over a real TLS exporter) it resists even an active MITM who
/// terminates one TLS leg, because that attacker holds a different exporter.
#[must_use]
pub fn channel_bind(token: &EdgeToken, exporter: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(token.as_bytes())
        .expect("HMAC-SHA256 accepts a key of any length");
    mac.update(exporter);
    mac.finalize()
}

/// Verify a `presented` secret against `token` on the CURRENT channel's
/// `exporter`, in **constant time**. Recomputes the HMAC and compares with
/// [`HmacSha256::verify_slice`] (constant-time tag compare), so a token bound to a
/// different channel (a captured replay) fails — its `presented` was computed over
/// a different exporter — and the comparison leaks no timing.
#[must_use]
pub fn verify_presented(token: &EdgeToken, exporter: &[u8], presented: &[u8]) -> bool {
    let Ok(mut mac) = HmacSha256::new_from_slice(token.as_bytes()) else {
        return false;
    };
    mac.update(exporter);
    mac.verify_slice(presented).is_ok()
}

// ---------------------------------------------------------------------------
// Capability handshake — the application-layer step that runs AFTER the TLS
// handshake (so an exporter exists) and BEFORE any control bytes relay. The
// dialer presents `channel_bind(token, exporter)`; the listener recomputes and
// verifies it in constant time against the token it minted for that (src, op).
// The token itself never crosses the wire; only the channel-bound MAC does.
// ---------------------------------------------------------------------------

/// What a verified dialer is authorized for: the driver identity (`src`) and the
/// op it may drive (`op`). Returned by [`verify_capability`] on success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Granted {
    /// The driver/session id that presented the capability.
    pub src: String,
    /// The op the capability authorizes (e.g. `drive`, `read`).
    pub op: String,
}

/// Upper bound on the AUTH line we will read before rejecting — a DoS guard so a
/// peer cannot make us buffer unboundedly before the capability is even checked.
const MAX_AUTH_LINE: usize = 4096;

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // unwrap: a hex digit (0..=15) is always representable.
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap());
        s.push(char::from_digit(u32::from(b & 0x0f), 16).unwrap());
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let len = b.len();
    // Every string that reaches us is a field of one AUTH line, and that line
    // was read bounded by `MAX_AUTH_LINE` (`read_line` errors past it), so a
    // longer input can never occur; rejecting it is a no-op that also bounds
    // the allocation below.
    if len > MAX_AUTH_LINE {
        return None;
    }
    if !len.is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(len / 2);
    let mut i = 0usize;
    // `i < len && i + 1 < len` keeps BOTH byte accesses provably in bounds
    // (and the `i + 1` probe itself overflow-free) without needing the
    // evenness invariant; on the even lengths that reach the loop it agrees
    // with the plain `i < len` bound exactly, so no trailing byte is skipped.
    while i < len && i + 1 < len {
        // `& 0x0f` is an identity on a hex-digit value (0..=15); it restates
        // the bound locally, and the shift/or composition of two nibbles
        // cannot overflow a byte.
        let hi = hex_nibble(b[i])? & 0x0f;
        let lo = hex_nibble(b[i + 1])? & 0x0f;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

/// Read one `\n`-terminated line (the `\n` is consumed, not returned), bounded by
/// `max` bytes. Errors on EOF before any byte, on overflow, or on non-UTF-8.
fn read_line<R: Read>(r: &mut R, max: usize) -> io::Result<String> {
    let mut buf = Vec::new();
    let mut one = [0u8; 1];
    loop {
        if r.read(&mut one)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before the capability line",
            ));
        }
        if one[0] == b'\n' {
            break;
        }
        if buf.len() >= max {
            return Err(io::Error::other(
                "capability line exceeds the maximum length",
            ));
        }
        buf.push(one[0]);
    }
    String::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Parse `AUTH <src> <op> <tag_hex>` (exactly four space-separated fields) into
/// `(src, op, tag_bytes)`. Returns `None` on any structural mismatch.
fn parse_auth(line: &str) -> Option<(String, String, Vec<u8>)> {
    let mut it = line.split(' ');
    if it.next()? != "AUTH" {
        return None;
    }
    let src = it.next()?;
    let op = it.next()?;
    let tag_hex = it.next()?;
    if it.next().is_some() {
        return None; // exactly four fields
    }
    if src.is_empty() || op.is_empty() {
        return None;
    }
    let tag = hex_decode(tag_hex)?;
    Some((src.to_owned(), op.to_owned(), tag))
}

/// `src`/`op` must be single tokens — no whitespace — or the `AUTH` framing would
/// be ambiguous on the wire.
fn is_wire_token(s: &str) -> bool {
    !s.is_empty() && !s.bytes().any(|b| b == b' ' || b == b'\n' || b == b'\r')
}

/// Dialer side: present the channel-bound capability over `stream` and await the
/// listener's verdict. `exporter` is THIS channel's exporter (the TLS RFC 5705
/// keying material from [`TlsTransport::exporter`](tls::TlsTransport::exporter)).
/// Sends `AUTH <src> <op> <tag_hex>\n` where the tag is
/// [`channel_bind(token, exporter)`](channel_bind), then reads back the verdict.
///
/// # Errors
/// If `src`/`op` are not wire tokens, on I/O failure, or if the listener denies
/// the capability (`Err` with the listener's reason).
pub fn present_capability<S: Read + Write>(
    stream: &mut S,
    exporter: &[u8],
    src: &str,
    op: &str,
    token: &EdgeToken,
) -> io::Result<()> {
    if !is_wire_token(src) || !is_wire_token(op) {
        return Err(io::Error::other("src/op must not contain whitespace"));
    }
    let tag = channel_bind(token, exporter);
    let line = format!("AUTH {src} {op} {}\n", hex_encode(&tag));
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    let verdict = read_line(stream, MAX_AUTH_LINE)?;
    if verdict == "OK" {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "capability denied by the listener: {verdict}"
        )))
    }
}

/// Listener side: read and verify the dialer's presentation over `stream`,
/// returning the [`Granted`] `(src, op)` on success. `exporter` is THIS channel's
/// exporter; `lookup(src, op)` yields the [`EdgeToken`] the listener minted for
/// that driver+op (`None` ⇒ no such grant). The HMAC is verified in **constant
/// time** ([`verify_presented`]); a captured tag from another channel fails
/// because its exporter differs. Writes `OK\n` on success, `DENIED\n` otherwise,
/// so the dialer gets a definite verdict either way.
///
/// # Errors
/// On I/O failure, or if the presentation is malformed / unknown / fails
/// verification (after a best-effort `DENIED` is written back).
pub fn verify_capability<S, F>(stream: &mut S, exporter: &[u8], lookup: F) -> io::Result<Granted>
where
    S: Read + Write,
    F: FnOnce(&str, &str) -> Option<EdgeToken>,
{
    let deny = |stream: &mut S, why: &str| -> io::Result<Granted> {
        let _ = stream.write_all(b"DENIED\n");
        let _ = stream.flush();
        Err(io::Error::other(format!("capability rejected: {why}")))
    };
    let line = read_line(stream, MAX_AUTH_LINE)?;
    let Some((src, op, tag)) = parse_auth(&line) else {
        return deny(stream, "malformed AUTH line");
    };
    let Some(token) = lookup(&src, &op) else {
        return deny(stream, "no capability for that (src, op)");
    };
    if verify_presented(&token, exporter, &tag) {
        stream.write_all(b"OK\n")?;
        stream.flush()?;
        Ok(Granted { src, op })
    } else {
        deny(stream, "channel-binding verification failed")
    }
}

/// A pinned remote identity. A dialer is expected to check this BEFORE presenting
/// the bound token, so a redirected/MITM'd endpoint is rejected before any secret
/// crosses the wire (the filesystem `confine_proxy_sock` discipline has no network
/// analog, so identity pinning replaces it). The `fingerprint` half is enforced by
/// the TLS layer ([`tls::client_config`] pins the cert); the `nonce` rebind half is
/// checked via [`RemoteEndpoint::matches`], enforced on the dial path by
/// [`dial_and_relay_pinned`](drive::dial_and_relay_pinned) BEFORE any relay. Because
/// no shipping listener yet echoes its live launch identity, a pinned nonce
/// currently FAILS CLOSED there (refuses to dial) rather than relay unverified — an
/// operator-requested guard is never silently skipped. (`sid` is carried for the
/// endpoint record but is not part of the `matches` check.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEndpoint {
    /// Host/address the endpoint was published at.
    pub host: String,
    /// The remote session id being driven.
    pub sid: String,
    /// The remote session's launch nonce (rebind guard).
    pub nonce: String,
    /// The pinned transport identity (TLS cert fingerprint / ssh host key).
    pub fingerprint: String,
}

impl RemoteEndpoint {
    /// Does an observed `(fingerprint, nonce)` match the pin? Both must hold, so
    /// neither a relaunched session (fresh nonce) nor a swapped host (fresh
    /// fingerprint) is dialed by a stale capability.
    #[must_use]
    pub fn matches(&self, observed_fingerprint: &str, observed_nonce: &str) -> bool {
        self.fingerprint == observed_fingerprint && self.nonce == observed_nonce
    }
}

/// The network object-capability: WHICH remote endpoint a node may dial. The
/// local fabric's `Scope::Owner` is all-or-nothing; crossing a host needs a
/// capability scoped to one endpoint, presented (channel-bound) on the dial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteOp {
    /// Authority to dial exactly this endpoint (and no other).
    DialRemote(RemoteEndpoint),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pump_carries_an_arbitrary_exchange_over_the_transport_verbatim() {
        // The relay carries a control-protocol response — including a multi-line
        // `OK <n>` body and a raw non-UTF-8 byte — across the transport unchanged.
        let (client, server) = LoopbackTransport::pair(b"chan-1".to_vec());
        let payload = b"OK 1\n{\"k\":1}\n\xff\n".to_vec();
        let p2 = payload.clone();
        let srv = std::thread::spawn(move || {
            let (mut r, mut w) = Box::new(server).split();
            let mut first = [0u8; 6];
            std::io::Read::read_exact(&mut r, &mut first).unwrap();
            assert_eq!(&first, b"screen"); // the relayed verb arrived
            w.write_all(&p2).unwrap();
            w.flush().unwrap();
        });
        let (mut cr, mut cw) = Box::new(client).split();
        cw.write_all(b"screen").unwrap();
        cw.flush().unwrap();
        let mut got = vec![0u8; payload.len()];
        std::io::Read::read_exact(&mut cr, &mut got).unwrap();
        assert_eq!(
            got, payload,
            "the byte pump is format-agnostic and lossless"
        );
        srv.join().unwrap();
    }

    #[test]
    fn channel_binding_rejects_a_cross_channel_replay() {
        // A token captured on channel A must NOT authorize on channel B — the
        // network analog of the same-uid peer check.
        let token = EdgeToken::generate();
        let exporter_a = b"tls-exporter-A".to_vec();
        let exporter_b = b"tls-exporter-B".to_vec();

        let presented_on_a = channel_bind(&token, &exporter_a);
        // Legit: presented on the SAME channel it was bound to -> accepted.
        assert!(verify_presented(&token, &exporter_a, &presented_on_a));
        // Replay: the captured channel-A value presented on channel B -> rejected.
        assert!(
            !verify_presented(&token, &exporter_b, &presented_on_a),
            "a captured token must not replay on a different channel"
        );
        // A wrong-length / garbage presentation is rejected (no panic).
        assert!(!verify_presented(&token, &exporter_a, b"short"));
        // And the two channels yield distinct bindings.
        assert_ne!(
            channel_bind(&token, &exporter_a),
            channel_bind(&token, &exporter_b)
        );
        // A DIFFERENT token cannot forge channel A's presentation (HMAC key).
        let other = EdgeToken::generate();
        assert!(!verify_presented(&other, &exporter_a, &presented_on_a));
    }

    #[test]
    fn capability_handshake_grants_valid_and_denies_unknown_replay_and_forgery() {
        // One channel exporter shared by both ends; a DIFFERENT one stands in for
        // a separate channel (the replay target).
        let exporter = b"tls-exporter-live".to_vec();
        let other_exporter = b"tls-exporter-other".to_vec();
        let token = EdgeToken::generate(); // EdgeToken is Copy

        // Helper: run one dialer presentation against a listener with `lookup`,
        // returning (dialer_result, listener_result).
        let run = |present_token: EdgeToken,
                   present_exporter: Vec<u8>,
                   verify_exporter: Vec<u8>,
                   known: Option<EdgeToken>|
         -> (io::Result<()>, io::Result<Granted>) {
            let (mut c, mut s) = CtlStream::pair().unwrap();
            let srv = std::thread::spawn(move || {
                verify_capability(&mut s, &verify_exporter, |src, op| {
                    if src == "driver-1" && op == "drive" {
                        known
                    } else {
                        None
                    }
                })
            });
            let cres = present_capability(
                &mut c,
                &present_exporter,
                "driver-1",
                "drive",
                &present_token,
            );
            let sres = srv.join().unwrap();
            (cres, sres)
        };

        // 1) Valid: same exporter, known (src, op), real token -> granted.
        let (c, s) = run(token, exporter.clone(), exporter.clone(), Some(token));
        assert!(c.is_ok(), "valid presentation accepted by dialer");
        assert_eq!(
            s.unwrap(),
            Granted {
                src: "driver-1".into(),
                op: "drive".into()
            }
        );

        // 2) Unknown (src, op): lookup returns None -> denied.
        let (c, s) = run(token, exporter.clone(), exporter.clone(), None);
        assert!(c.is_err() && s.is_err(), "unknown grant denied");

        // 3) Replay: dialer computed its tag over `other_exporter`, listener is on
        //    `exporter` -> the channel binding differs -> denied.
        let (c, s) = run(token, other_exporter, exporter.clone(), Some(token));
        assert!(c.is_err() && s.is_err(), "cross-channel replay denied");

        // 4) Forgery: dialer holds a DIFFERENT token than the one the listener
        //    minted -> HMAC mismatch -> denied.
        let (c, s) = run(
            EdgeToken::generate(),
            exporter.clone(),
            exporter,
            Some(token),
        );
        assert!(c.is_err() && s.is_err(), "wrong-token forgery denied");
    }

    #[test]
    fn endpoint_pin_rejects_relaunch_and_host_swap() {
        let ep = RemoteEndpoint {
            host: "host-1:7000".into(),
            sid: "s-abc".into(),
            nonce: "nonce-1".into(),
            fingerprint: "fp-1".into(),
        };
        assert!(ep.matches("fp-1", "nonce-1"), "exact match dials");
        assert!(
            !ep.matches("fp-1", "nonce-2"),
            "relaunched session (fresh nonce) refused"
        );
        assert!(
            !ep.matches("fp-2", "nonce-1"),
            "swapped host (fresh fingerprint) refused"
        );
    }

    #[test]
    fn dial_remote_is_scoped_to_one_endpoint() {
        let ep1 = RemoteEndpoint {
            host: "h1".into(),
            sid: "s1".into(),
            nonce: "n1".into(),
            fingerprint: "f1".into(),
        };
        let cap = RemoteOp::DialRemote(ep1.clone());
        // The capability authorizes exactly ep1, not a different endpoint.
        match cap {
            RemoteOp::DialRemote(e) => assert_eq!(e, ep1),
        }
    }
}
