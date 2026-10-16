// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! **aterm-net** — layer L3 of the RFC "The Reactive Surface": the control
//! protocol (read + drive + `await`) over a network boundary, so one aterm
//! drives another on a different host exactly as it would locally.
//!
//! Predicates run on the **authoritative** surface (the host that owns the
//! engine), never on a remote fold, so driving a remote aterm is relaying the
//! control protocol: no astream fold is on the critical path.
//!
//! The listener is `aterm-gui`'s `net_listen`, off by default: it binds only
//! when aterm.toml sets `[net] listen`/`cert`/`key`, and only in a top-level
//! aterm. The driver is the `dial <name>` control verb
//! ([`dial_and_relay_pinned`](drive::dial_and_relay_pinned)), over which
//! `aterm_agent::RelayClient` drives a `Turn`. The pieces:
//!
//! 1. [`tls`] — the TLS 1.3 transport ([`tls::relay`] owns both halves: TLS has no
//!    `UnixStream::try_clone`), and [`drive`] composes both ends of the network
//!    drive over it.
//! 2. [`channel_bind`] / [`verify_presented`] — the capability binding that
//!    replaces the local same-uid `SO_PEERCRED` check, which has **no network
//!    analog**. The presented secret is `HMAC-SHA256(edge_token, channel_exporter)`
//!    (the token is the MAC key), verified in constant time, so a token captured
//!    on one connection **cannot replay** on another (a different exporter → a
//!    different tag the verifier rejects) and cannot be forged without the token.
//!    Over the TLS 1.3 exporter ([`TlsTransport`](tls::TlsTransport), RFC 5705) it
//!    resists even an active MITM. Proven by `channel_bind_model` +
//!    `net_capability_grant_model` (`aterm-spec`).
//! 3. [`RemoteEndpoint`] — the pinned `(host, sid, nonce, fingerprint)` a dialer
//!    checks before presenting the token (the cert fingerprint is TLS-enforced; the
//!    nonce rebind guard is the dialer's responsibility — see [`RemoteEndpoint`]).

use std::io::{self, Read, Write};

use aterm_codec::hex;
use aterm_digest::HmacSha256;
use aterm_session::EdgeToken;

/// TLS 1.3 transport (rustls) with the RFC 5705 keying-material exporter that
/// binds the capability to the channel.
pub mod tls;

/// The network drive — both ends ([`serve`](drive::serve)/[`dial_and_relay_pinned`]
/// (drive::dial_and_relay_pinned)) composed from the transport, the channel-bound
/// capability, and the relay. Secure-default-OFF: nothing binds without an
/// explicit operator-stood-up listener.
pub mod drive;

/// The channel-bound presented secret: `HMAC-SHA256(edge_token, channel_exporter)`.
/// The token is the key and the exporter (RFC 5705 keying material, see
/// [`TlsTransport`]) the message, so the tag cannot be produced without the token
/// and a tag captured on one channel is rejected on any other — including by an
/// active MITM terminating one TLS leg, who holds a different exporter.
#[must_use]
pub fn channel_bind(token: &EdgeToken, exporter: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(token.as_bytes())
        .expect("HMAC-SHA256 accepts a key of any length");
    mac.update(exporter);
    mac.finalize()
}

/// Verify a `presented` secret against `token` on the CURRENT channel's
/// `exporter`, in constant time ([`HmacSha256::verify_slice`]). A tag bound to a
/// different channel (a captured replay) fails.
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

/// [`hex::decode_vec`] bounded by [`MAX_AUTH_LINE`]. The loop stays here rather
/// than calling it: it is written for the Trust verifier, and a cross-crate call
/// would take its proved obligations out of this crate's ratchet floor.
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
        let hi = hex::nibble(b[i])? & 0x0f;
        let lo = hex::nibble(b[i + 1])? & 0x0f;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

/// Read one `\n`-terminated line (the `\n` is consumed, not returned), bounded by
/// `max` bytes. Errors on EOF before the newline, on overflow, or on non-UTF-8.
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
/// the capability: a `PermissionDenied` error carrying [`TokenRefused`]
/// ([`is_token_refused`] tells it apart).
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
    let line = format!("AUTH {src} {op} {}\n", hex::encode_lower(&tag));
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    // The dialer has SENT its capability line; an end of stream here is the
    // listener hanging up instead of answering it.
    let verdict = read_line(stream, MAX_AUTH_LINE).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the remote hung up without answering the drive token",
            )
        } else {
            e
        }
    })?;
    if verdict == "OK" {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            TokenRefused,
        ))
    }
}

/// The listener answered the dialer's capability with anything but `OK`. An aterm
/// listener grants every dialer that asks to `drive`, so this means the drive
/// token did not verify: the dialer holds a stale or wrong one.
#[derive(Debug)]
pub struct TokenRefused;

impl std::fmt::Display for TokenRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the remote refused the drive token")
    }
}

impl std::error::Error for TokenRefused {}

/// Whether `e` is [`present_capability`]'s refusal ([`TokenRefused`]).
#[must_use]
pub fn is_token_refused(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<TokenRefused>())
}

/// Listener side: read and verify the dialer's presentation over `stream`,
/// returning the [`Granted`] `(src, op)` on success. `exporter` is THIS channel's
/// exporter; `lookup(src, op)` yields the [`EdgeToken`] the listener minted for
/// that driver+op (`None` ⇒ no such grant). The HMAC is verified in **constant
/// time** ([`verify_presented`]); a captured tag from another channel fails
/// because its exporter differs. Writes `OK\n` on success. A complete UTF-8
/// line with invalid AUTH syntax, no grant or a wrong tag gets a best-effort
/// `DENIED\n`; read and framing errors return without writing a verdict.
///
/// # Errors
/// On I/O or framing failure, or if a complete presentation is malformed,
/// unknown or fails verification (after attempting to write `DENIED`).
pub fn verify_capability<S, F>(stream: &mut S, exporter: &[u8], lookup: F) -> io::Result<Granted>
where
    S: Read + Write,
    F: FnOnce(&str, &str) -> Option<EdgeToken>,
{
    let deny = |stream: &mut S, why: &'static str| -> io::Result<Granted> {
        let _ = stream.write_all(b"DENIED\n");
        let _ = stream.flush();
        Err(io::Error::new(io::ErrorKind::PermissionDenied, why))
    };
    let line = read_line(stream, MAX_AUTH_LINE)?;
    let Some((src, op, tag)) = parse_auth(&line) else {
        return deny(stream, "malformed capability line");
    };
    let Some(token) = lookup(&src, &op) else {
        return deny(stream, "no capability for the requested op");
    };
    if verify_presented(&token, exporter, &tag) {
        stream.write_all(b"OK\n")?;
        stream.flush()?;
        Ok(Granted { src, op })
    } else {
        deny(stream, "wrong drive token")
    }
}

/// A pinned remote identity. The dial path checks the presented TLS certificate
/// against `fingerprint` BEFORE presenting the bound token, so a redirected
/// endpoint is rejected before authentication. The TLS configuration also checks
/// certificate identity and key possession ([`tls::client_config`]). The `nonce`
/// rebind half is enforced on the dial path by
/// [`dial_and_relay_pinned`](drive::dial_and_relay_pinned) BEFORE any relay, against
/// the live nonce the remote's `sessions bridge` roster reports for `sid` (or, with
/// no `sid`, for any session carrying the pinned nonce). A pinned session the remote
/// does not report FAILS CLOSED (nothing is relayed) rather than relay unverified — an
/// operator-requested guard is never silently skipped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEndpoint {
    /// Host/address the endpoint was published at.
    pub host: String,
    /// The remote session id being driven.
    pub sid: String,
    /// The remote session's launch nonce (rebind guard).
    pub nonce: String,
    /// The pinned transport identity. The TLS dial path accepts SHA-256 hex,
    /// optionally `sha256:`-prefixed, with surrounding whitespace ignored.
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

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_uds::CtlStream;
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
        // Each end says what happened in its own words: the dialer that its token
        // was refused (a typed error its caller words with the remedy), the
        // listener that the token was wrong — no echoed `DENIED` verdict.
        let dialer = c.unwrap_err();
        assert!(is_token_refused(&dialer), "{dialer}");
        assert_eq!(dialer.to_string(), "the remote refused the drive token");
        assert_eq!(s.unwrap_err().to_string(), "wrong drive token");
    }

    /// A listener that hangs up after the capability line, answering neither `OK`
    /// nor `DENIED`: the dialer says so, not "closed before the capability line"
    /// (it had already sent that line).
    #[test]
    fn a_listener_that_hangs_up_unanswered_is_named_as_such() {
        let (mut c, s) = CtlStream::pair().unwrap();
        let srv = std::thread::spawn(move || {
            let mut s = s;
            let _ = read_line(&mut s, MAX_AUTH_LINE);
            drop(s);
        });
        let err = present_capability(&mut c, b"x", "driver-1", "drive", &EdgeToken::generate())
            .unwrap_err();
        srv.join().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(!is_token_refused(&err));
        assert_eq!(
            err.to_string(),
            "the remote hung up without answering the drive token"
        );
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
}
