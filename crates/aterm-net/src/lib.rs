// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The anchor attributes are `macro_rules!` ones where Cargo resolved no proc macro
// under them (crates/aterm-spec-anchor/anchor_rules.rs).
#![cfg_attr(anchor_rules, feature(macro_attr))]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! **aterm-net** — how one aterm reaches another over the network: the fleet
//! (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5), whose members read
//! each other's survey, and drive a member only where its person lets them
//! (§5.1, the step-in).
//!
//! The listener is `aterm-gui`'s `fleet_listen`, off by default: it binds only
//! when aterm.toml sets `[fleet] listen` and the machine holds a fleet key.
//! What it admits is a fleet peer, never the Owner, but for a drive: a node
//! that `[fleet] drive_from` grants, proved by the key it was granted under,
//! whose admitted lines the drive gate dispatches as the Owner's. The reader
//! is `aterm ctl survey --fleet`, the driver `aterm ctl --host`. The pieces:
//!
//! 1. [`fleet`] — the fleet handshake: TLS 1.3 with ALPN `atf/2` (or an older
//!    aterm's `atf/1`) and one certificate every aterm embeds, then a mutual
//!    proof of one fleet secret — the listener first, each role with its own
//!    HKDF key, each tag over the channel's exporter — and under `atf/2` each
//!    end's proof of its own node key over that exporter; and the fleet
//!    listener's accept loop, source fence and key file.
//! 2. [`tls`] — the TLS 1.3 transport: the pinned certificate check, the RFC
//!    5705 exporter a proof is taken over, and [`tls::relay_answers`], which
//!    bridges a proved stream to the control server that answers it (TLS has no
//!    `UnixStream::try_clone`, so it owns both halves).
//! 3. `slots` — the listener's slots and the handshake's wall-clock deadline:
//!    a connection that has proved nothing gives its slot up to a newcomer
//!    (r13 NET-1), and a peer that dribbles bytes is still cut off at its
//!    deadline. Modelled as `NetSlotDisplacement` (`aterm-models-core`).
//! 4. [`fleet_member`] — membership (`aterm fleet init|join|peers|leave`): the
//!    join code, this machine's node id and node key, the issuer's book of
//!    peers and the node keys it binds, the registration a member makes with
//!    it and the member's read of that book, and the fleet client `aterm ctl
//!    survey --fleet` reads every peer through.
//!
//! The network drive that stood here before the fleet (`[net]`, `dial`, an
//! edge token presented to an operator-certified listener that relayed it to
//! the Owner) was removed on 2026-10-15; design §7 says what replaced it.

use aterm_codec::hex;

/// TLS 1.3 transport (rustls) with the RFC 5705 keying-material exporter a
/// fleet proof is taken over, and the relay that bridges a proved stream.
pub mod tls;

/// The listener's slots and the handshake's deadline, which the fleet's
/// listener and dialer share.
mod slots;

/// The fleet handshake — the mutual, role-keyed, exporter-bound proof of one
/// fleet secret, over TLS 1.3 with ALPN `atf/2` or `atf/1` and the one
/// embedded certificate, with each end's node proof under `atf/2`
/// ([`fleet::server_handshake_as`] / [`fleet::client_handshake_as`]) — and
/// the listener that serves it ([`fleet::serve`], [`fleet::read_key_file`]).
pub mod fleet;

/// Fleet membership: the join code, this machine's node id, the issuer's book
/// of peers and the one exchange `aterm fleet join` makes with the issuer
/// ([`fleet_member::serve_join`] / [`fleet_member::register`]), the issuer's
/// book read by a member ([`fleet_member::serve_book`] /
/// [`fleet_member::fetch_book`]), reaching a peer ([`fleet_member::dial`]) and
/// reading every peer's survey ([`fleet_member::survey_all`], and a member's
/// whole fleet through its issuer's book, [`fleet_member::read_whole`]). Unix
/// only, as the key file's reader is.
#[cfg(unix)]
pub mod fleet_member;

/// The longest hex text [`hex_decode`] takes: far past anything that reaches
/// it — a fleet proof's 64-digit tag, a key file's at most 64 digits — so it
/// bounds the allocation without ever refusing a real input.
const MAX_HEX: usize = 4096;

/// [`hex::decode_vec`] bounded by [`MAX_HEX`]. The loop stays here rather
/// than calling it: it is written for the Trust verifier, and a cross-crate call
/// would take its proved obligations out of this crate's ratchet floor.
fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let len = b.len();
    // Every string that reaches us is a fleet proof's tag or a key file's
    // digits, each read under a bound far below `MAX_HEX`, so a longer input
    // can never occur; rejecting it is a no-op that also bounds the
    // allocation below.
    if len > MAX_HEX {
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
