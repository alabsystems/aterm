// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fleet handshake probes ([`with_proved`], [`wrong_secret`], [`raw_dial`],
//! [`reflected_proof_refused`]).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use aterm_net::fleet::{FleetError, FleetOp, FleetScope, FleetSecret, client_handshake};

/// How long one dial may take before it is read as a hang (AGENTS.md: a wait
/// that must end is a hang detector, never a budget).
const WITHIN: Duration = Duration::from_secs(60);

fn at(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// A proved fleet stream, read and written as one.
pub trait Rw: Read + Write {}
impl<T: Read + Write> Rw for T {}

/// Dial `port`, prove `secret` for `op`, and run `f` on the proved stream.
///
/// # Errors
/// The connect or the handshake's own error.
pub fn with_proved<R>(
    port: u16,
    secret: &FleetSecret,
    op: FleetOp,
    f: impl FnOnce(&mut dyn Rw, FleetScope) -> R,
) -> Result<R, String> {
    let tcp = TcpStream::connect_timeout(&at(port), Duration::from_secs(5))
        .map_err(|e| format!("connect 127.0.0.1:{port}: {e}"))?;
    let (mut t, scope) =
        client_handshake(tcp, secret, op, WITHIN).map_err(|e| format!("handshake: {e}"))?;
    t.stream()
        .get_ref()
        .set_read_timeout(Some(WITHIN))
        .map_err(|e| e.to_string())?;
    Ok(f(t.stream(), scope))
}

/// Dial `port` with `secret`, another fleet's.
#[must_use]
pub fn wrong_secret(port: u16, secret: &FleetSecret) -> Option<FleetError> {
    let tcp = TcpStream::connect_timeout(&at(port), Duration::from_secs(5)).ok()?;
    client_handshake(tcp, secret, FleetOp::Survey, WITHIN).err()
}

/// Send `line` and read one reply line (its newline dropped).
///
/// # Errors
/// The write or the read failed.
pub fn ask(stream: &mut dyn Rw, line: &str) -> Result<String, String> {
    send_raw(stream, format!("{line}\n").as_bytes())?;
    read_line(stream).map_err(|e| format!("read: {e}"))
}

/// Send `bytes` as they are (a frame's header and body).
///
/// # Errors
/// The write failed.
pub fn send_raw(stream: &mut dyn Rw, bytes: &[u8]) -> Result<(), String> {
    stream
        .write_all(bytes)
        .and_then(|()| stream.flush())
        .map_err(|e| format!("send: {e}"))
}

/// One line, its newline dropped (empty at the end of the stream).
///
/// # Errors
/// A read failed with no byte of the line read.
pub fn read_line(stream: &mut dyn Rw) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0]),
            Err(e) if line.is_empty() => return Err(e),
            Err(_) => break,
        }
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// Confirm a live listener rejects its OWN proof line echoed back.
///
/// # Errors
/// The connect or the TLS handshake failed, or no proof line came.
pub fn reflected_proof_refused(port: u16) -> Result<String, String> {
    let tcp = TcpStream::connect_timeout(&at(port), Duration::from_secs(5))
        .map_err(|e| format!("connect 127.0.0.1:{port}: {e}"))?;
    tcp.set_read_timeout(Some(WITHIN))
        .and_then(|()| tcp.set_write_timeout(Some(WITHIN)))
        .map_err(|e| e.to_string())?;
    let name = rustls::pki_types::ServerName::try_from("aterm-fleet")
        .map_err(|e| e.to_string())?
        .to_owned();
    let conn = rustls::ClientConnection::new(aterm_net::fleet::client_config(), name)
        .map_err(|e| e.to_string())?;
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    let proof = read_line(&mut tls).map_err(|e| format!("no proof line: {e}"))?;
    if proof.split(' ').nth(1).is_none_or(|t| t.len() != 64) {
        return Err(format!("the listener's first line is no proof: {proof:?}"));
    }
    send_raw(&mut tls, format!("{proof}\n").as_bytes())?;
    read_line(&mut tls).map_err(|e| format!("read the answer: {e}"))
}

/// Whether the peer has closed: the next read ends with nothing more, never by the read's timeout.
pub fn closed(stream: &mut dyn Rw) -> bool {
    let mut rest = Vec::new();
    match stream.read_to_end(&mut rest) {
        Ok(_) => rest.is_empty(),
        Err(e) => {
            rest.is_empty()
                && !matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
        }
    }
}

/// The fleet's RFC 5705 exporter label (`aterm_net::fleet`'s module doc).
const EXPORTER_LABEL: &[u8] = b"EXPORTER-aterm-fleet-v1";

/// The dialer's proof line for `op` on the channel whose exporter is `exporter`.
#[must_use]
pub fn dialer_proof(secret: &[u8], exporter: &[u8], op: &str) -> String {
    let prk = aterm_digest::hkdf_extract(&[], secret);
    let kc = aterm_digest::hkdf_expand_32(&prk, b"aterm-fleet/v1 client");
    let tag = aterm_digest::hmac_sha256_parts(&kc, &[exporter, op.as_bytes()]);
    format!("{op} {}\n", aterm_codec::hex::encode_lower(&tag))
}

/// What a [`raw_dial`] saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawDial {
    /// The scope word the listener proved first (what any dialer learns).
    pub scope: String,
    /// The listener's answer to the proof sent (`OK`, `NO proof`, …).
    pub answer: String,
    /// Whether the listener closed the connection after its answer.
    pub closed: bool,
}

/// Dial `port` and send a `survey` proof under `secret` without verifying the listener first.
///
/// # Errors
/// The connect or the TLS handshake failed, or no proof line came.
pub fn raw_dial(port: u16, secret: &[u8]) -> Result<RawDial, String> {
    let tcp = TcpStream::connect_timeout(&at(port), Duration::from_secs(5))
        .map_err(|e| format!("connect 127.0.0.1:{port}: {e}"))?;
    tcp.set_read_timeout(Some(WITHIN))
        .and_then(|()| tcp.set_write_timeout(Some(WITHIN)))
        .map_err(|e| e.to_string())?;
    let name = rustls::pki_types::ServerName::try_from("aterm-fleet")
        .map_err(|e| e.to_string())?
        .to_owned();
    let conn = rustls::ClientConnection::new(aterm_net::fleet::client_config(), name)
        .map_err(|e| e.to_string())?;
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    let proof = read_line(&mut tls).map_err(|e| format!("no proof line: {e}"))?;
    let scope = proof.split(' ').next().unwrap_or_default().to_owned();
    let exporter = tls
        .conn
        .export_keying_material([0u8; 32], EXPORTER_LABEL, None)
        .map_err(|e| format!("the exporter: {e}"))?;
    let mut said = dialer_proof(secret, &exporter, "survey");
    if tls.conn.alpn_protocol() == Some(aterm_net::fleet::ALPN_NODES) {
        said.push_str("node -\n");
    }
    send_raw(&mut tls, said.as_bytes())?;
    let answer = read_line(&mut tls).map_err(|e| format!("read the answer: {e}"))?;
    let closed = answer != "OK" && closed(&mut tls);
    Ok(RawDial {
        scope,
        answer,
        closed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The proof is the documented line: a word, a space, 64 lowercase hex
    /// digits; another secret, exporter or op makes another tag.
    #[test]
    fn the_raw_proof_is_one_documented_line_bound_to_secret_exporter_and_op() {
        assert_eq!(
            dialer_proof(&[0x0b; 22], &[0xa5; 32], "survey"),
            "survey f92e33cf27fc4091ec8577e69a3afae1985bfca7b81d9d98738e89272125c732\n"
        );
        let a = dialer_proof(&[0x11; 20], &[0xa5; 32], "survey");
        let (word, tag) = a.trim_end().split_once(' ').expect("a word and a tag");
        assert_eq!(word, "survey");
        assert!(aterm_codec::hex::is_lower(tag, 64), "{tag}");
        assert!(a.ends_with('\n'));
        assert_ne!(a, dialer_proof(&[0x22; 20], &[0xa5; 32], "survey"));
        assert_ne!(a, dialer_proof(&[0x11; 20], &[0x5a; 32], "survey"));
        assert_ne!(a, dialer_proof(&[0x11; 20], &[0xa5; 32], "join"));
    }
}
