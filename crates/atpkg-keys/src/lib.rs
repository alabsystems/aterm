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
mod tests {
    use super::*;

    /// A master-signed one-machine roster, with optional revoked ids and a deadline.
    fn signed_roster(
        master: &[u8],
        machine_pub: &str,
        revoked: &[&str],
        valid_until: &str,
    ) -> (Vec<u8>, Vec<u8>) {
        use aterm_update_core::roster::{Machine, Roster};
        let roster = Roster {
            schema: 1,
            roster_seq: 4,
            valid_until: valid_until.into(),
            machines: vec![Machine {
                id: "m2".into(),
                pubkey: machine_pub.into(),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            }],
            revoked: revoked.iter().map(|r| (*r).to_string()).collect(),
        };
        let bytes = roster
            .to_toml()
            .expect("a valid roster serializes")
            .into_bytes();
        let sig = sign(master, &bytes).expect("the master signs");
        (bytes, sig)
    }

    #[test]
    fn verify_signed_accepts_a_rostered_signature_and_nothing_else() {
        use aterm_update_core::roster::RosterReject;
        let now = 1_785_801_600i64; // 2026-08-04
        let (master, master_pub) = generate().unwrap();
        let (machine, machine_pub) = generate().unwrap();
        let file = b"# alab-release SHA256SUMS \xe2\x80\x94 schema 1\n# tag: v0.25.0\n";
        let sig = sign(&machine, file).unwrap();
        let (roster, roster_sig) =
            signed_roster(&master, &machine_pub, &[], "2099-01-01T00:00:00Z");

        let who = verify_signed(&[&master_pub], roster.clone(), &roster_sig, file, &sig, now)
            .expect("a rostered machine's signature verifies");
        assert_eq!(
            who,
            Signed {
                machine_id: "m2".into(),
                roster_seq: 4
            }
        );

        let mut bad = file.to_vec();
        bad[2] ^= 0x01;
        assert_eq!(
            verify_signed(&[&master_pub], roster.clone(), &roster_sig, &bad, &sig, now),
            Err(RosterReject::Verify)
        );
        let (stranger, _) = generate().unwrap();
        let stranger_sig = sign(&stranger, file).unwrap();
        assert_eq!(
            verify_signed(
                &[&master_pub],
                roster.clone(),
                &roster_sig,
                file,
                &stranger_sig,
                now
            ),
            Err(RosterReject::Verify)
        );
        let (other_master, _) = generate().unwrap();
        let forged_sig = sign(&other_master, &roster).unwrap();
        assert_eq!(
            verify_signed(&[&master_pub], roster.clone(), &forged_sig, file, &sig, now),
            Err(RosterReject::Verify)
        );
        let (revoked, revoked_sig) =
            signed_roster(&master, &machine_pub, &["m2"], "2099-01-01T00:00:00Z");
        assert!(verify_signed(&[&master_pub], revoked, &revoked_sig, file, &sig, now).is_err());
        let (stale, stale_sig) = signed_roster(&master, &machine_pub, &[], "2026-01-01T00:00:00Z");
        assert_eq!(
            verify_signed(&[&master_pub], stale, &stale_sig, file, &sig, now),
            Err(RosterReject::Stale)
        );
    }

    #[test]
    fn produces_signatures_the_client_verifier_accepts() {
        use aterm_update_core::roster::{Machine, Roster, RosterReject};

        let (key, pub_b64) = generate().unwrap();
        let manifest = b"schema = 2\nindex_build = 7\nvalid_until = \"2099-01-01T00:00:00Z\"\n";
        let sig = sign(&key, manifest).unwrap();
        let roster = Roster {
            schema: 1,
            roster_seq: 1,
            valid_until: "2099-01-01T00:00:00Z".into(),
            machines: vec![Machine {
                id: "m3".into(),
                pubkey: pub_b64.clone(),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            }],
            revoked: vec![],
        };
        let now = 1_785_801_600i64;

        let who = roster
            .authorize_appcast(manifest, &sig, now)
            .expect("the client must accept a signature this tool produced");
        assert_eq!(who.machine_id, "m3");
        assert_eq!(who.pubkey_b64, pub_b64);
        let mut bad = manifest.to_vec();
        bad[0] ^= 0x01;
        assert_eq!(
            roster.authorize_appcast(&bad, &sig, now).err(),
            Some(RosterReject::Verify)
        );
        let (other, _) = generate().unwrap();
        let other_sig = sign(&other, manifest).unwrap();
        assert_eq!(
            roster.authorize_appcast(manifest, &other_sig, now).err(),
            Some(RosterReject::Verify)
        );
    }

    #[test]
    fn pubkey_b64_recovers_the_published_key() {
        let (key, pub_b64) = generate().unwrap();
        assert_eq!(pubkey_b64(&key).unwrap(), pub_b64);
        assert_eq!(
            aterm_codec::base64::decode_strict(pub_b64.as_bytes())
                .unwrap()
                .len(),
            32
        );
    }

    #[test]
    fn invalid_key_fails_closed() {
        assert!(sign(b"not a pkcs8 key", b"x").is_err());
        assert!(pubkey_b64(b"not a pkcs8 key").is_err());
    }

    fn argv(tokens: &[&str]) -> Argv {
        let mut out: Argv = Default::default();
        for (slot, tok) in out.iter_mut().zip(tokens.iter()) {
            *slot = Some((*tok).to_string());
        }
        out
    }

    #[test]
    fn vet_args_accepts_allowed_flags_and_positionals_and_refuses_bad_shapes() {
        let a = argv(&["--id", "m3", "--pins", "/tmp/pins.rs"]);
        assert_eq!(vet_args("setup", &a, &["id", "pins"], 0), Ok(()));
        assert_eq!(flag(&a, "id"), Some("m3"));
        assert_eq!(flag(&a, "pins"), Some("/tmp/pins.rs"));
        assert_eq!(flag(&a, "roster"), None);

        let pos = argv(&["key.pkcs8", "manifest.toml", "manifest.toml.sig"]);
        assert_eq!(vet_args("sign", &pos, &[], 3), Ok(()));
        assert_eq!(positional(&pos, 0), Some("key.pkcs8"));
        assert_eq!(positional(&pos, 1), Some("manifest.toml"));
        assert_eq!(positional(&pos, 2), Some("manifest.toml.sig"));
        assert_eq!(positional(&pos, 3), None);

        assert_eq!(
            vet_args("setup", &argv(&["--id=m3"]), &["id"], 0),
            Err(
                "'--id=m3' uses the `--name=value` form; write it as two tokens: `--id <value>`"
                    .to_string()
            )
        );
        for &m in MASTER_FLAGS {
            let tok = format!("--{m}");
            let err = vet_args("setup", &argv(&[&tok, "x"]), &["id"], 0).unwrap_err();
            assert!(err.contains("TREAT THAT MASTER AS COMPROMISED"), "{err}");
        }
        assert_eq!(
            vet_args("setup", &argv(&["--bogus", "x"]), &["id", "pins"], 0),
            Err(
                "unknown flag '--bogus' for `atpkg-keys setup` (it accepts --id, --pins)"
                    .to_string()
            )
        );
        assert_eq!(
            vet_args("master-check", &argv(&["--bogus", "x"]), &[], 0),
            Err("unknown flag '--bogus' for `atpkg-keys master-check`".to_string())
        );
        assert_eq!(
            vet_args("setup", &argv(&["--id"]), &["id"], 0),
            Err("'--id' needs a value after it".to_string())
        );
        assert_eq!(
            vet_args("setup", &argv(&["--id", "--pins"]), &["id", "pins"], 0),
            Err("'--id' was given '--pins' as its value, which is another flag".to_string())
        );
        assert_eq!(
            vet_args(
                "setup",
                &argv(&["--id", "m1", "--id", "m2"]),
                &["id", "pins"],
                0
            ),
            Err("'--id' was given more than once".to_string())
        );
        assert_eq!(
            vet_args("setup", &argv(&["extra"]), &["id"], 0),
            Err("unexpected argument 'extra' for `atpkg-keys setup`".to_string())
        );
    }
}
