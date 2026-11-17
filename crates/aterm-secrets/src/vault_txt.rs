// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `_alab-vault._tcp` service a vault server advertises and the TXT hint its advert carries
//! ([`VaultTxt`]): what a server registers (`server`) and what a browse reads back
//! ([`crate::discover`]), kept apart from the server so a member that serves nothing reads it too.

use crate::envelope;

/// The mDNS service type advertised.
pub const SERVICE_TYPE: &str = "_alab-vault._tcp";

/// What a server's `_alab-vault._tcp` TXT record says of the vault it advertises: hints, never a
/// trust grant (envelopes still verify against the pinned signer, records against their keys).
///
/// It is also a suppression signal: a vault-tend round pulls a peer's envelope only while its TXT
/// names a `seq` above the one held, and its records only while it names a `rec` this server
/// neither holds nor proved by a pull it finished (the server's `pull_due`). Anyone on the LAN can send a
/// TXT, so one that copies an instance's name with an equal `seq` and this server's own `rec`
/// (both public) keeps those gated pulls from that instance. What bounds it:
/// - each server, the holder's included, is swept (pulled whatever its TXT's seq, rec and label
///   say) by the first round that resolves it once its `ANTI_ENTROPY_EVERY` (±20%) passed since a
///   round last swept it whole, or since a catch-up (this server's first round, a wake, a change
///   of its addresses, sharing back on) came after that sweep (`PullGate::sweep_due`); a round
///   resolves a rotating batch of at most [`crate::discover::MAX_CANDIDATES`] names, so a forged
///   or lost TXT holds a version or record back at most that period plus the rounds a batch
///   takes to reach that server, and a sweep its pull did not finish is made again by the next
///   round past its `PULL_SPACING`;
/// - peers are told apart, and this server from them, by the addresses they resolve to, never by
///   a TXT's `label`, and an instance only adds to what its endpoints are pulled for, but for one
///   naming a `rec` on the host an `alab-secrets` instance's `label` names, which stands for the
///   records of that instance, whose TXT names none, where both resolve to the same endpoints,
///   in its round and those up to the server's `NAMED_FOR` after it until a catch-up (the
///   server's `pull_due`, `PullGate::names_rec`), so a forged one holds them back within the
///   bound above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultTxt {
    /// Vault name (`[a-z0-9][a-z0-9-]{0,62}`).
    pub vault: String,
    /// Signer fingerprint (`xxxx-xxxx-xxxx-xxxx-xxxx` lowercase hex, [`envelope::fingerprint`]).
    pub signer: String,
    /// Held sequence number (`>= 1`).
    pub seq: u64,
    /// SHA-256 digest of the held envelope bytes ([`envelope::Envelope::digest`]).
    pub head: [u8; 32],
    /// Optional canonical label of the server advertising this instance (`label=<mN>`, F7): the
    /// name it registers its own instance under, named on that instance and on its
    /// `alab-secrets` one (the server's `advertised_vault`), where holders contesting
    /// `alab-secrets` rank one another by it (the server's `AlabNameWatcher`). A pull reads it on
    /// an `alab-secrets` TXT to tell which instance may stand for that TXT's records, the one on
    /// the host `<label>.local` (`PullGate::names_rec`), never as a sign that one does: the
    /// instance under that name carries the TXT of whichever vault its server advertises there,
    /// which can be another than the `alab-secrets` one names, so only that instance's own TXT,
    /// as a round resolved it, shows it named the records (the server's `pull_due`).
    pub label: Option<String>,
    /// The digest of the records it holds of the vault (`rec=<64 hex>`,
    /// [`crate::membership_log::Store::records_digest`], the empty log's while its store holds no
    /// genesis): on a server's own-label instance alone, never on `alab-secrets`
    /// (the server's `advertised_vault`), since a TXT changes only by ending its registration and starting
    /// another, and records grow with every secret written. `None` from a server before it, or
    /// from the `alab-secrets` instance: it says nothing of what that server holds.
    pub rec: Option<[u8; 32]>,
}

impl VaultTxt {
    /// The `key=value` TXT attributes registered after `path=/v1`.
    #[must_use]
    pub fn txt_pairs(&self) -> Vec<String> {
        let mut pairs = vec![
            format!("vault={}", self.vault),
            format!("signer={}", self.signer),
            format!("seq={}", self.seq),
            format!("head={}", aterm_codec::hex::encode_lower(&self.head)),
        ];
        if let Some(label) = &self.label {
            pairs.push(format!("label={label}"));
        }
        if let Some(rec) = &self.rec {
            pairs.push(format!("rec={}", aterm_codec::hex::encode_lower(rec)));
        }
        pairs
    }

    /// Parse a `_alab-vault._tcp` TXT record's `key=value` strings (RFC 6763 §6.4: keys are
    /// ASCII case-insensitive and the first occurrence of a key wins). Requires `path=/v1` and a
    /// well-formed `vault`, `signer`, `seq` (`>= 1`) and `head`; an older server's `path=/v1`
    /// alone yields `None`. A `rec` that is not 64 lowercase hex digits is none, as an older
    /// server's TXT without one is.
    pub fn from_txt<'a>(pairs: impl IntoIterator<Item = &'a str>) -> Option<Self> {
        let mut path = None;
        let mut vault = None;
        let mut signer = None;
        let mut seq = None;
        let mut head = None;
        let mut label = None;
        let mut rec = None;
        for pair in pairs {
            let Some((raw_key, value)) = pair.split_once('=') else {
                continue;
            };
            if raw_key.eq_ignore_ascii_case("path") {
                path.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("vault") {
                vault.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("signer") {
                signer.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("seq") {
                seq.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("head") {
                head.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("label") {
                label.get_or_insert(value);
            } else if raw_key.eq_ignore_ascii_case("rec") {
                rec.get_or_insert(value);
            }
        }
        if path != Some("/v1") {
            return None;
        }
        let vault = vault.filter(|name| envelope::valid_name(name))?;
        let signer = signer.filter(|fp| valid_fingerprint(fp))?;
        let seq = parse_positive_seq(seq?)?;
        let head = hex32_decode(head?)?;
        let label = label
            .filter(|name| {
                *name != crate::alab_advertised() && crate::text::valid_machine_name(name)
            })
            .map(str::to_owned);
        Some(Self {
            vault: vault.to_owned(),
            signer: signer.to_owned(),
            seq,
            head,
            label,
            rec: rec.and_then(hex32_decode),
        })
    }
}

fn valid_fingerprint(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 24
        && bytes.iter().enumerate().all(|(idx, &b)| {
            if matches!(idx, 4 | 9 | 14 | 19) {
                b == b'-'
            } else {
                matches!(b, b'0'..=b'9' | b'a'..=b'f')
            }
        })
}

fn parse_positive_seq(text: &str) -> Option<u64> {
    if text.is_empty() || text.starts_with('0') || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>().ok().filter(|&n| n >= 1)
}

fn hex32_decode(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let bytes = text.as_bytes();
    let mut out = [0u8; 32];
    for (idx, slot) in out.iter_mut().enumerate() {
        let hi = nibble(bytes[idx * 2])?;
        let lo = nibble(bytes[idx * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}
