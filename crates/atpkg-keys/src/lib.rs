// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Owner-only POSIX tool (0600 key files; its consumer atpkg is unix-gated too): the crate
// compiles empty on Windows and the binary prints an "unsupported" stub.
#![cfg(unix)]

//! Owner-side Ed25519 signing for atpkg manifests: the producer half of the signature
//! contract whose verifier is `atpkg::sig`.
//!
//! See `docs/RELEASE-KEYS.md`.

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

pub mod fsio;
pub mod master;
pub mod pins_edit;
pub mod provision;
pub mod roster_ops;
#[cfg(test)]
mod testdir;

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Generate a fresh Ed25519 keypair: `(pkcs8 private-key bytes, base64 raw 32-byte public
/// key)`. The pkcs8 bytes are the secret: write them `0600` and keep them offline.
pub fn generate() -> Result<(Vec<u8>, String), String> {
    let rng = SystemRandom::new();
    let pkcs8 =
        Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| "key generation failed".to_string())?;
    let pub_b64 = pubkey_b64(pkcs8.as_ref())?;
    Ok((pkcs8.as_ref().to_vec(), pub_b64))
}

/// The base64 raw public key of a pkcs8 private key.
pub fn pubkey_b64(pkcs8: &[u8]) -> Result<String, String> {
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| "invalid pkcs8 key".to_string())?;
    aterm_codec::base64::encode(kp.public_key().as_ref())
        .map_err(|_| "public key too large to encode".to_string())
}

/// Detached-sign `msg`'s exact bytes (the raw manifest asset) with the pkcs8 key, giving the
/// 64-byte Ed25519 signature the client verifies.
pub fn sign(pkcs8: &[u8], msg: &[u8]) -> Result<Vec<u8>, String> {
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| "invalid pkcs8 key".to_string())?;
    Ok(kp.sign(msg).as_ref().to_vec())
}

/// Who signed a file, as [`verify_signed`] proved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    /// The roster id of the machine whose key verified.
    pub machine_id: String,
    /// The roster generation that authorized it.
    pub roster_seq: u64,
}

/// Most tokens read after the verb (`setup` and `join` take up to 8), held in a fixed
/// [`Argv`] array.
pub const MAX_ARGS: usize = 14;

/// Bounded argument vector (`None` from the first unset slot onwards).
pub type Argv = [Option<String>; MAX_ARGS];

/// Flag names that would carry the master key on argv, refused with a compromise warning.
pub const MASTER_FLAGS: &[&str] = &[
    "master",
    "master-file",
    "phrase",
    "phrase-file",
    "seed",
    "paper",
    "secret",
];

/// Value following `--<name>`, or `None` (two-token `--name value` form; [`vet_args`] runs first).
#[must_use]
pub fn flag<'a>(argv: &'a Argv, name: &str) -> Option<&'a str> {
    for i in 0..MAX_ARGS {
        if argv[i].as_deref().and_then(|a| a.strip_prefix("--")) == Some(name) && i + 1 < MAX_ARGS {
            return argv[i + 1].as_deref();
        }
    }
    None
}

/// The `want`th positional argument (skipping `--flag value` pairs), 0-based after the verb.
#[must_use]
pub fn positional(argv: &Argv, want: usize) -> Option<&str> {
    let mut seen = 0usize;
    let mut i = 0usize;
    while i < MAX_ARGS {
        match argv[i].as_deref() {
            None => return None,
            Some(a) if a.starts_with("--") => i += 2,
            Some(a) => {
                if seen == want {
                    return Some(a);
                }
                seen += 1;
                i += 1;
            }
        }
    }
    None
}

/// Refuse any argument shape outside `allowed` flags and `max_positional` bare arguments.
pub fn vet_args(
    verb: &str,
    argv: &Argv,
    allowed: &[&str],
    max_positional: usize,
) -> Result<(), String> {
    let mut positional = 0usize;
    let mut seen_flags = [""; MAX_ARGS];
    let mut n_seen = 0usize;
    let mut i = 0usize;
    while i < MAX_ARGS {
        let Some(tok) = argv[i].as_deref() else {
            return Ok(());
        };
        if !tok.starts_with("--") {
            positional += 1;
            if positional > max_positional {
                return Err(fsio::concat(&[
                    "unexpected argument '",
                    tok,
                    "' for `atpkg-keys ",
                    verb,
                    "`",
                ]));
            }
            i += 1;
            continue;
        }
        let name = tok.get(2..).unwrap_or("");
        if let Some(eq) = name.find('=') {
            return Err(fsio::concat(&[
                "'",
                tok,
                "' uses the `--name=value` form; write it as two tokens: `--",
                name.get(..eq).unwrap_or(""),
                " <value>`",
            ]));
        }
        if MASTER_FLAGS.contains(&name) {
            return Err(fsio::concat(&[
                "'",
                tok,
                "' does not exist: the master phrase is read only from /dev/tty. If you \
                 typed your real phrase on this command line, TREAT THAT MASTER AS \
                 COMPROMISED (it may be in shell history and was visible to `ps`): destroy \
                 the paper and generate a new master on a tree whose anchor is not armed.",
            ]));
        }
        if !allowed.contains(&name) {
            let mut msg = fsio::concat(&["unknown flag '", tok, "' for `atpkg-keys ", verb, "`"]);
            for (n, a) in allowed.iter().enumerate() {
                msg.push_str(if n == 0 { " (it accepts " } else { ", " });
                msg.push_str("--");
                msg.push_str(a);
            }
            if !allowed.is_empty() {
                msg.push(')');
            }
            return Err(msg);
        }
        if seen_flags[..n_seen].contains(&name) {
            return Err(fsio::concat(&["'", tok, "' was given more than once"]));
        }
        seen_flags[n_seen] = name;
        n_seen += 1;
        match argv.get(i + 1).and_then(|v| v.as_deref()) {
            None => {
                return Err(fsio::concat(&["'", tok, "' needs a value after it"]));
            }
            Some(v) if v.starts_with("--") => {
                return Err(fsio::concat(&[
                    "'",
                    tok,
                    "' was given '",
                    v,
                    "' as its value, which is another flag",
                ]));
            }
            Some(_) => {}
        }
        i += 2;
    }
    Ok(())
}

/// Verify a detached Ed25519 signature over `file`'s exact bytes by a machine the
/// master-signed roster authorizes, by the client's own appcast rule: the roster verifies
/// under one of `master_pubkeys`, parses, is fresh at `now_unix`, and one of its live machines
/// (listed, not revoked, not expired) signed the file.
///
/// The ALab lane (`atpkg-publish lane run alab`) runs this over a source release's
/// `SHA256SUMS` before building it, so a release signed by no rostered machine, or by one
/// since revoked, is never built.
///
/// # Errors
///
/// The [`aterm_update_core::roster::RosterReject`] of the first step that refused.
pub fn verify_signed(
    master_pubkeys: &[&str],
    roster: Vec<u8>,
    roster_sig: &[u8],
    file: &[u8],
    sig: &[u8],
    now_unix: i64,
) -> Result<Signed, aterm_update_core::roster::RosterReject> {
    use aterm_update_core::roster::{Roster, verify_roster};
    let verified = verify_roster(master_pubkeys, roster, roster_sig)?;
    let parsed = Roster::parse(&verified)?;
    verify_signed_by_roster(&parsed, file, sig, now_unix)
}

/// Verify a detached Ed25519 signature over `file`'s exact bytes against an already
/// master-verified [`aterm_update_core::roster::Roster`].
///
/// # Errors
///
/// The [`aterm_update_core::roster::RosterReject`] of the first step that refused.
pub fn verify_signed_by_roster(
    roster: &aterm_update_core::roster::Roster,
    file: &[u8],
    sig: &[u8],
    now_unix: i64,
) -> Result<Signed, aterm_update_core::roster::RosterReject> {
    roster.admit(0, now_unix)?;
    let who = roster.authorize_appcast(file, sig, now_unix)?;
    Ok(Signed {
        machine_id: who.machine_id,
        roster_seq: who.roster_seq,
    })
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
