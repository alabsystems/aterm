//! Rung 7 — the online key-agreement handshake: forward secrecy on top of the
//! pre-shared key.
//!
//! The Rung-6 sealed wire keys the transport directly from the PSK, so a
//! recorded session is decryptable forever by anyone who later learns the PSK.
//! This handshake fixes that: each connection derives a FRESH per-session key
//! from an ephemeral X25519 exchange, and the ephemeral secrets are discarded
//! afterwards — so a later PSK compromise cannot decrypt past traffic (forward
//! secrecy), while the PSK still authenticates (a peer without it derives a
//! different key and its first sealed record fails to open).
//!
//! # Construction (the NNpsk pattern) — WE own it
//! We own the whole protocol: the message framing, the transcript binding, and
//! HMAC-SHA256 / HKDF-SHA256, composed here over the workspace's SHA-256
//! (`astream-digest`) exactly as `astream-cap` owns its HMAC. The ONE primitive we delegate is the raw X25519
//! scalar multiplication — hand-rolling constant-time curve25519 field arithmetic
//! is where DIY crypto breaks — to the vetted `x25519-dalek`.
//!
//! ```text
//! client ── magic|ver|e_client_pub ─▶ server      (1-RTT, client speaks first)
//! client ◀─ magic|ver|e_server_pub ── server
//! both: dh = X25519(e_local_priv, e_remote_pub)             // shared, commutative
//!       key = HKDF-SHA256(salt = PSK,                       // PSK authenticates
//!                         ikm  = dh,                        // ephemeral ⇒ forward secrecy
//!                         info = "astream-handshake-v1" ‖ e_client_pub ‖ e_server_pub)
//!       transport = SealedStream(stream, key)               // the Rung-6 record layer
//! ```
//!
//! `info` fixes the two ephemeral keys in ROLE order (client then server), so a
//! reflected message derives a different key. The DH output is checked
//! contributory (a low-order remote point that would force a zero shared secret
//! is rejected). Mutual authentication is by key confirmation: the first sealed
//! record each side opens proves the peer derived the same key, i.e. holds the
//! PSK — no explicit confirmation message needed.

use crate::{SealedStream, KEY_LEN};
use astream_digest::Sha256;
use std::io::{self, Read, Write};
use x25519_dalek::{PublicKey, SharedSecret, StaticSecret};

#[cfg(clean_islands)]
#[path = "handshake_island.rs"]
mod handshake_island;
#[cfg(not(clean_islands))]
pub use crate::twins::handshake_clamp_info_len;
#[cfg(clean_islands)]
pub use handshake_island::handshake_clamp_info_len;

contract_fn! {
    /// Verified check that a handshake message carries version 1 (`ver == 1`),
    /// linked via `clean-kernel-defeq` to `handshake_version_valid_isl`.
    #[must_use]
    pub fn handshake_version_valid(ver: u8) -> bool
    clauses [ensures result == (ver == 1)]
    {
        ver == 1
    }
}

const SHA256_BLOCK: usize = 64;
const SHA256_OUT: usize = 32;
const MAX_HKDF_INFO: usize = 1024;

const fn hmac_ipad(k0: &[u8; SHA256_BLOCK]) -> [u8; SHA256_BLOCK] {
    [
        k0[0] ^ 0x36,
        k0[1] ^ 0x36,
        k0[2] ^ 0x36,
        k0[3] ^ 0x36,
        k0[4] ^ 0x36,
        k0[5] ^ 0x36,
        k0[6] ^ 0x36,
        k0[7] ^ 0x36,
        k0[8] ^ 0x36,
        k0[9] ^ 0x36,
        k0[10] ^ 0x36,
        k0[11] ^ 0x36,
        k0[12] ^ 0x36,
        k0[13] ^ 0x36,
        k0[14] ^ 0x36,
        k0[15] ^ 0x36,
        k0[16] ^ 0x36,
        k0[17] ^ 0x36,
        k0[18] ^ 0x36,
        k0[19] ^ 0x36,
        k0[20] ^ 0x36,
        k0[21] ^ 0x36,
        k0[22] ^ 0x36,
        k0[23] ^ 0x36,
        k0[24] ^ 0x36,
        k0[25] ^ 0x36,
        k0[26] ^ 0x36,
        k0[27] ^ 0x36,
        k0[28] ^ 0x36,
        k0[29] ^ 0x36,
        k0[30] ^ 0x36,
        k0[31] ^ 0x36,
        k0[32] ^ 0x36,
        k0[33] ^ 0x36,
        k0[34] ^ 0x36,
        k0[35] ^ 0x36,
        k0[36] ^ 0x36,
        k0[37] ^ 0x36,
        k0[38] ^ 0x36,
        k0[39] ^ 0x36,
        k0[40] ^ 0x36,
        k0[41] ^ 0x36,
        k0[42] ^ 0x36,
        k0[43] ^ 0x36,
        k0[44] ^ 0x36,
        k0[45] ^ 0x36,
        k0[46] ^ 0x36,
        k0[47] ^ 0x36,
        k0[48] ^ 0x36,
        k0[49] ^ 0x36,
        k0[50] ^ 0x36,
        k0[51] ^ 0x36,
        k0[52] ^ 0x36,
        k0[53] ^ 0x36,
        k0[54] ^ 0x36,
        k0[55] ^ 0x36,
        k0[56] ^ 0x36,
        k0[57] ^ 0x36,
        k0[58] ^ 0x36,
        k0[59] ^ 0x36,
        k0[60] ^ 0x36,
        k0[61] ^ 0x36,
        k0[62] ^ 0x36,
        k0[63] ^ 0x36,
    ]
}

const fn hmac_opad(k0: &[u8; SHA256_BLOCK]) -> [u8; SHA256_BLOCK] {
    [
        k0[0] ^ 0x5c,
        k0[1] ^ 0x5c,
        k0[2] ^ 0x5c,
        k0[3] ^ 0x5c,
        k0[4] ^ 0x5c,
        k0[5] ^ 0x5c,
        k0[6] ^ 0x5c,
        k0[7] ^ 0x5c,
        k0[8] ^ 0x5c,
        k0[9] ^ 0x5c,
        k0[10] ^ 0x5c,
        k0[11] ^ 0x5c,
        k0[12] ^ 0x5c,
        k0[13] ^ 0x5c,
        k0[14] ^ 0x5c,
        k0[15] ^ 0x5c,
        k0[16] ^ 0x5c,
        k0[17] ^ 0x5c,
        k0[18] ^ 0x5c,
        k0[19] ^ 0x5c,
        k0[20] ^ 0x5c,
        k0[21] ^ 0x5c,
        k0[22] ^ 0x5c,
        k0[23] ^ 0x5c,
        k0[24] ^ 0x5c,
        k0[25] ^ 0x5c,
        k0[26] ^ 0x5c,
        k0[27] ^ 0x5c,
        k0[28] ^ 0x5c,
        k0[29] ^ 0x5c,
        k0[30] ^ 0x5c,
        k0[31] ^ 0x5c,
        k0[32] ^ 0x5c,
        k0[33] ^ 0x5c,
        k0[34] ^ 0x5c,
        k0[35] ^ 0x5c,
        k0[36] ^ 0x5c,
        k0[37] ^ 0x5c,
        k0[38] ^ 0x5c,
        k0[39] ^ 0x5c,
        k0[40] ^ 0x5c,
        k0[41] ^ 0x5c,
        k0[42] ^ 0x5c,
        k0[43] ^ 0x5c,
        k0[44] ^ 0x5c,
        k0[45] ^ 0x5c,
        k0[46] ^ 0x5c,
        k0[47] ^ 0x5c,
        k0[48] ^ 0x5c,
        k0[49] ^ 0x5c,
        k0[50] ^ 0x5c,
        k0[51] ^ 0x5c,
        k0[52] ^ 0x5c,
        k0[53] ^ 0x5c,
        k0[54] ^ 0x5c,
        k0[55] ^ 0x5c,
        k0[56] ^ 0x5c,
        k0[57] ^ 0x5c,
        k0[58] ^ 0x5c,
        k0[59] ^ 0x5c,
        k0[60] ^ 0x5c,
        k0[61] ^ 0x5c,
        k0[62] ^ 0x5c,
        k0[63] ^ 0x5c,
    ]
}

/// HMAC-SHA256 (RFC 2104), composed over `astream-digest`. Ours, not a
/// dependency — the same stance `astream-cap` takes.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; SHA256_OUT] {
    // Derive the block-sized key K0: hash an over-long key, else right-zero-pad.
    let mut k0 = [0u8; SHA256_BLOCK];
    if key.len() > SHA256_BLOCK {
        k0[..SHA256_OUT].copy_from_slice(&Sha256::digest(key));
    } else {
        k0[..key.len()].copy_from_slice(key);
    }
    let mut ipad = hmac_ipad(&k0);
    let mut opad = hmac_opad(&k0);
    let inner = {
        let mut h = Sha256::new();
        h.update(ipad);
        h.update(msg);
        h.finalize()
    };
    let mut h = Sha256::new();
    h.update(opad);
    h.update(inner);
    k0.fill(0);
    ipad.fill(0);
    opad.fill(0);
    core::hint::black_box((&mut k0, &mut ipad, &mut opad));
    h.finalize()
}

/// HKDF-SHA256 (RFC 5869) producing exactly 32 bytes (a single expand block,
/// L = 32 ≤ 255·32). Extract then Expand, ours over `hmac_sha256`.
pub(crate) fn hkdf_sha256_32(salt: &[u8], ikm: &[u8], info: &[u8]) -> [u8; 32] {
    // Extract.
    let mut prk = hmac_sha256(salt, ikm);
    // Expand, N = 1: T(1) = HMAC(PRK, T(0)=<empty> ‖ info ‖ 0x01).
    let clamped = handshake_clamp_info_len(info.len() as u64, MAX_HKDF_INFO as u64) as usize;
    let info_len = if clamped <= MAX_HKDF_INFO {
        clamped
    } else {
        MAX_HKDF_INFO
    };
    let mut t1_input = Vec::with_capacity(info_len + 1);
    t1_input.extend_from_slice(info);
    t1_input.push(0x01);
    let okm = hmac_sha256(&prk, &t1_input);
    prk.fill(0); // wipe the intermediate pseudo-random key
    core::hint::black_box(&mut prk);
    okm
}

const HS_MAGIC: [u8; 4] = *b"aSH1";
const HS_VERSION: u8 = 1;
const HS_MSG_LEN: usize = 4 + 1 + 32; // magic ‖ version ‖ ephemeral pubkey
const HS_LABEL: &[u8] = b"astream-handshake-v1";

const fn build_hs_msg(pubkey: &[u8; 32]) -> [u8; HS_MSG_LEN] {
    [
        HS_MAGIC[0],
        HS_MAGIC[1],
        HS_MAGIC[2],
        HS_MAGIC[3],
        HS_VERSION,
        pubkey[0],
        pubkey[1],
        pubkey[2],
        pubkey[3],
        pubkey[4],
        pubkey[5],
        pubkey[6],
        pubkey[7],
        pubkey[8],
        pubkey[9],
        pubkey[10],
        pubkey[11],
        pubkey[12],
        pubkey[13],
        pubkey[14],
        pubkey[15],
        pubkey[16],
        pubkey[17],
        pubkey[18],
        pubkey[19],
        pubkey[20],
        pubkey[21],
        pubkey[22],
        pubkey[23],
        pubkey[24],
        pubkey[25],
        pubkey[26],
        pubkey[27],
        pubkey[28],
        pubkey[29],
        pubkey[30],
        pubkey[31],
    ]
}

const fn has_hs_magic(msg: &[u8; HS_MSG_LEN]) -> bool {
    msg[0] == HS_MAGIC[0] && msg[1] == HS_MAGIC[1] && msg[2] == HS_MAGIC[2] && msg[3] == HS_MAGIC[3]
}

const fn hs_msg_version(msg: &[u8; HS_MSG_LEN]) -> u8 {
    msg[4]
}

const fn extract_hs_pubkey(msg: &[u8; HS_MSG_LEN]) -> [u8; 32] {
    [
        msg[5], msg[6], msg[7], msg[8], msg[9], msg[10], msg[11], msg[12], msg[13], msg[14],
        msg[15], msg[16], msg[17], msg[18], msg[19], msg[20], msg[21], msg[22], msg[23], msg[24],
        msg[25], msg[26], msg[27], msg[28], msg[29], msg[30], msg[31], msg[32], msg[33], msg[34],
        msg[35], msg[36],
    ]
}

pub(crate) fn write_hs_msg<W: Write>(w: &mut W, pubkey: &[u8; 32]) -> io::Result<()> {
    let msg = build_hs_msg(pubkey);
    w.write_all(&msg)?;
    w.flush()
}

pub(crate) fn read_hs_msg<R: Read>(r: &mut R) -> io::Result<[u8; 32]> {
    let mut msg = [0u8; HS_MSG_LEN];
    r.read_exact(&mut msg)?;
    if !has_hs_magic(&msg) || !handshake_version_valid(hs_msg_version(&msg)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "astream handshake: bad magic/version (peer not speaking the sealed handshake)",
        ));
    }
    Ok(extract_hs_pubkey(&msg))
}

/// A fresh ephemeral X25519 keypair, seeded from the OS CSPRNG (`getrandom`, which
/// we already depend on). We use `StaticSecret` (byte-seedable) as a one-shot
/// ephemeral and drop it after the single DH — `x25519-dalek`'s `zeroize` wipes it
/// on drop, and we wipe our transient seed too.
pub(crate) fn ephemeral() -> (StaticSecret, [u8; 32]) {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).expect("OS CSPRNG unavailable");
    let secret = StaticSecret::from(seed);
    seed.fill(0);
    core::hint::black_box(&mut seed);
    let public = *PublicKey::from(&secret).as_bytes();
    (secret, public)
}

/// The session key: HKDF over the PSK (salt/authenticator) and the DH secret
/// (IKM/forward-secrecy), transcript-bound to both ephemeral keys in ROLE order.
fn derive_session_key(
    psk: &[u8; KEY_LEN],
    dh: &[u8; 32],
    client_pub: &[u8; 32],
    server_pub: &[u8; 32],
) -> [u8; KEY_LEN] {
    let clamped = handshake_clamp_info_len(HS_LABEL.len() as u64, 64) as usize;
    let label_len = if clamped <= 64 { clamped } else { 64 };
    let mut info = Vec::with_capacity(label_len + 64);
    info.extend_from_slice(HS_LABEL);
    info.extend_from_slice(client_pub);
    info.extend_from_slice(server_pub);
    hkdf_sha256_32(psk, dh, &info)
}

/// The raw ephemeral X25519 shared secret against `their_pub`, rejecting a
/// non-contributory (low-order) point that would force a zero output. Shared by
/// the PSK handshake and the identity handshake.
///
/// Returned as `x25519-dalek`'s own type, which wipes itself on drop: a copy
/// into a plain array would outlive the handshake in memory, and this value
/// (with the PSK, if any) is all a later reader needs to decrypt the session.
pub(crate) fn x25519_shared(
    secret: &StaticSecret,
    their_pub: &[u8; 32],
) -> io::Result<SharedSecret> {
    let shared = secret.diffie_hellman(&PublicKey::from(*their_pub));
    if !shared.was_contributory() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "astream handshake: non-contributory X25519 (low-order point)",
        ));
    }
    Ok(shared)
}

/// Complete the DH and derive the PSK-authenticated session key.
fn agree(
    secret: &StaticSecret,
    psk: &[u8; KEY_LEN],
    their_pub: &[u8; 32],
    client_pub: &[u8; 32],
    server_pub: &[u8; 32],
) -> io::Result<[u8; KEY_LEN]> {
    let dh = x25519_shared(secret, their_pub)?;
    Ok(derive_session_key(
        psk,
        dh.as_bytes(),
        client_pub,
        server_pub,
    ))
}

/// The bytes both sides bind into every record's AAD: the ephemeral public keys in
/// a fixed order (client, then server), as THIS side observed them. An active MITM
/// that relays different ephemerals to each leg produces different transcripts, so
/// its records fail to open on the far side. The identity handshake binds the same
/// bytes, which are also what its peers sign.
pub(crate) const fn transcript(client_pub: &[u8; 32], server_pub: &[u8; 32]) -> [u8; 64] {
    [
        client_pub[0],
        client_pub[1],
        client_pub[2],
        client_pub[3],
        client_pub[4],
        client_pub[5],
        client_pub[6],
        client_pub[7],
        client_pub[8],
        client_pub[9],
        client_pub[10],
        client_pub[11],
        client_pub[12],
        client_pub[13],
        client_pub[14],
        client_pub[15],
        client_pub[16],
        client_pub[17],
        client_pub[18],
        client_pub[19],
        client_pub[20],
        client_pub[21],
        client_pub[22],
        client_pub[23],
        client_pub[24],
        client_pub[25],
        client_pub[26],
        client_pub[27],
        client_pub[28],
        client_pub[29],
        client_pub[30],
        client_pub[31],
        server_pub[0],
        server_pub[1],
        server_pub[2],
        server_pub[3],
        server_pub[4],
        server_pub[5],
        server_pub[6],
        server_pub[7],
        server_pub[8],
        server_pub[9],
        server_pub[10],
        server_pub[11],
        server_pub[12],
        server_pub[13],
        server_pub[14],
        server_pub[15],
        server_pub[16],
        server_pub[17],
        server_pub[18],
        server_pub[19],
        server_pub[20],
        server_pub[21],
        server_pub[22],
        server_pub[23],
        server_pub[24],
        server_pub[25],
        server_pub[26],
        server_pub[27],
        server_pub[28],
        server_pub[29],
        server_pub[30],
        server_pub[31],
    ]
}

/// Run the client (initiator) side of the handshake over `stream` and return the
/// forward-secret [`SealedStream`]. Sends our ephemeral key first, then reads the
/// peer's.
///
/// # Errors
/// A socket error, a peer not speaking the handshake (bad magic/version), or a
/// non-contributory DH.
pub fn client_handshake<S: Read + Write>(
    mut stream: S,
    psk: &[u8; KEY_LEN],
) -> io::Result<SealedStream<S>> {
    let (secret, our_pub) = ephemeral();
    write_hs_msg(&mut stream, &our_pub)?;
    let their_pub = read_hs_msg(&mut stream)?;
    // We are the client: our key is client_pub, theirs is server_pub.
    let key = agree(&secret, psk, &their_pub, &our_pub, &their_pub)?;
    // The record layer binds the TRANSCRIPT — the two ephemeral public keys as we
    // saw them — into every record's AAD, so a record is valid only for this
    // session and this direction. No hello exchange and no confirm record: the
    // agreement above already supplied the freshness and the key confirmation
    // those exist to provide for a bare PSK.
    Ok(SealedStream::with_transcript(
        stream,
        key,
        &transcript(&our_pub, &their_pub),
        true,
    ))
}

/// Run the server (responder) side of the handshake over `stream`: read the
/// client's ephemeral key, reply with ours, and return the forward-secret
/// [`SealedStream`].
///
/// # Errors
/// A socket error, a peer not speaking the handshake (bad magic/version), or a
/// non-contributory DH.
pub fn server_handshake<S: Read + Write>(
    mut stream: S,
    psk: &[u8; KEY_LEN],
) -> io::Result<SealedStream<S>> {
    // The client speaks first.
    let their_pub = read_hs_msg(&mut stream)?;
    let (secret, our_pub) = ephemeral();
    write_hs_msg(&mut stream, &our_pub)?;
    // They are the client, we are the server.
    let key = agree(&secret, psk, &their_pub, &their_pub, &our_pub)?;
    Ok(SealedStream::with_transcript(
        stream,
        key,
        &transcript(&their_pub, &our_pub),
        false,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn hmac_sha256_matches_rfc4231_case2() {
        // RFC 4231 Test Case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let expected = hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        assert_eq!(mac.as_slice(), expected.as_slice());
    }

    #[test]
    fn hkdf_sha256_matches_rfc5869_a1() {
        // RFC 5869 Appendix A.1 (SHA-256); we emit the first 32 bytes of the OKM.
        let ikm = [0x0bu8; 22];
        let salt = hex("000102030405060708090a0b0c");
        let info = hex("f0f1f2f3f4f5f6f7f8f9");
        let okm = hkdf_sha256_32(&salt, &ikm, &info);
        let expected = hex("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf3");
        assert_eq!(okm.as_slice(), expected.as_slice());
    }

    #[test]
    fn hkdf_sha256_matches_rfc5869_a2() {
        // RFC 5869 Appendix A.2: an 80-byte salt is longer than the HMAC block, so
        // Extract hashes it first — the path A.1's 13-byte salt never reaches.
        let salt: Vec<u8> = (0x60u8..=0xaf).collect();
        let ikm: Vec<u8> = (0x00u8..=0x4f).collect();
        let info: Vec<u8> = (0xb0u8..=0xff).collect();
        let okm = hkdf_sha256_32(&salt, &ikm, &info);
        let expected = hex("b11e398dc80327a1c8e7f78c596a49344f012eda2d4efad8a050cc4c19afa97c");
        assert_eq!(okm.as_slice(), expected.as_slice());
    }

    #[test]
    fn the_session_key_is_the_recorded_answer() {
        // Recorded from this crate over the `sha2` crate (0.10.9) on 2026-10-15,
        // before it moved to `astream-digest`, and unchanged over it: both ends of a handshake must derive
        // this key, so the hash under it may change only if the key does not.
        let key = derive_session_key(&[9u8; KEY_LEN], &[7u8; 32], &[1u8; 32], &[2u8; 32]);
        let expected = hex("0d5a777ebe43ec2044188fa05d226c609e80df383b6cfce2b3cfa2bacbc64c24");
        assert_eq!(key.as_slice(), expected.as_slice());
    }

    #[test]
    fn transcript_role_order_changes_the_key() {
        // Reflection resistance at the KDF: swapping client/server pub derives a
        // different key, so a reflected transcript cannot agree.
        let psk = [9u8; KEY_LEN];
        let dh = [7u8; 32];
        let a = [1u8; 32];
        let b = [2u8; 32];
        assert_ne!(
            derive_session_key(&psk, &dh, &a, &b),
            derive_session_key(&psk, &dh, &b, &a)
        );
    }

    // Drive both handshake halves over a real loopback socket pair, then exchange a
    // sealed frame each way. Returns Ok(()) iff the whole path works.
    fn run_handshake(client_psk: [u8; KEY_LEN], server_psk: [u8; KEY_LEN]) -> io::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let server = std::thread::spawn(move || -> io::Result<()> {
            let (sock, _) = listener.accept()?;
            let mut s = server_handshake(sock, &server_psk)?;
            // Read the client's sealed frame, reply with our own.
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf)?;
            assert_eq!(&buf, b"hello");
            s.write_all(b"world")?;
            s.flush()
        });
        let sock = TcpStream::connect(addr)?;
        let mut c = client_handshake(sock, &client_psk)?;
        c.write_all(b"hello")?;
        c.flush()?;
        let mut buf = [0u8; 5];
        c.read_exact(&mut buf)?;
        assert_eq!(&buf, b"world");
        server.join().unwrap()
    }

    #[test]
    fn matching_psk_agrees_and_talks() {
        run_handshake([0x42; KEY_LEN], [0x42; KEY_LEN]).expect("matching PSK must agree");
    }

    #[test]
    fn mismatched_psk_cannot_talk() {
        // The DH succeeds but the derived keys differ, so the first sealed frame
        // fails to open — the sealed exchange errors on one or both sides.
        let mut wrong = [0x42; KEY_LEN];
        wrong[0] ^= 0xff;
        assert!(
            run_handshake([0x42; KEY_LEN], wrong).is_err(),
            "a mismatched PSK must not establish a working session"
        );
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }
}
