// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FABRIC BODY (§4.1) — one ASCII line of `key=value` tokens, optionally
//! followed by `len=<n>` raw bytes.
//!
//! ```text
//! v=1 t=<ms> [from=<sid>] [re=<offset>] [dl=<ms>] [epoch=<hex32>] [gen=<seq>:<fp16>]
//!            [via=<p>[,<p>…]] [text=<pct>] [len=<n>]
//! ["\n" ‖ <n> raw bytes]
//! ```
//!
//! WHAT IS NOT IN IT is the design's point: no `from=` a sender chooses, no
//! `to=`, no `kind=`, no `id=`, no `trust=`. Those are the subject's segments,
//! the record's offset, and a label the RECEIVER computes. The one attested
//! exception is `from=<sid>`, which a NODE adds on behalf of a session it hosts —
//! and it is attested precisely because the node's `<src>` segment is cap-forced,
//! so the node could type as that session anyway (§8.3).
//!
//! The parser is TOTAL over arbitrary bytes: an unknown token is ignored, a
//! malformed number is `None`, and nothing here can panic on a hostile body.

use std::collections::BTreeMap;

/// The kinds an `in` record may carry (§4.2). CLOSED: a kind outside this set is
/// refused at the subject, so nothing an agent reads carries a token the bridge
/// never classified. This is the same list the endpoint's `deliver` accepts.
pub const KINDS: [&str; 9] = [
    "ask",
    "answer",
    "task",
    "report",
    "note",
    "control",
    "ack",
    "expired",
    "undeliverable",
];

/// Whether `k` is one of [`KINDS`].
#[must_use]
pub fn is_kind(k: &str) -> bool {
    KINDS.contains(&k)
}

/// The most relay hops a `via=` chain may name, and the most bytes it may spend
/// on them.
///
/// ## THE FIELD THAT EVERY COMPONENT BOUNDED DIFFERENTLY
///
/// `via=` is decoded here with no length, count or shape check — deliberately,
/// because [`Body::decode`] is TOTAL and a parser that dropped a record on a
/// hostile token would lose a message on a policy. Every consumer therefore has
/// to bound it, and three of them each had their own idea of the limit: the
/// endpoint checks each comma element against its principal grammar and the
/// element COUNT against nothing; aterm's control server drops any request line
/// at 64 KiB; and the bridge built a `deliver` line carrying the chain verbatim.
/// A record head runs to the broker's 16 MiB ceiling, so a stranger's chain went
/// past the request-line bound, the writer refused the line, and the record was
/// lost with no verdict at all. That is the same shape as the round-1 wound, one
/// field over — three implicit limits for one value and no place where they met.
///
/// THIS IS THAT PLACE. It lives beside the parser rather than beside any one
/// consumer, so the inbound side ([`crate::bridge`]'s `deliver` line) and the
/// outbound side ([`crate::mirror`]'s `post` line) are bounding the same field by
/// the same number. The byte cap is COMPUTED from the hop cap and
/// [`crate::subject::PRINCIPAL_MAX`], so those two cannot drift either.
///
/// Sixteen is past any relay §6.7 describes, and the whole is three orders of
/// magnitude under the request-line bound: a relay chain is a path through a
/// fleet, not a payload. It buys a sender nothing — a chain is a CLAIM on the
/// relayer's word, never authority, and `via.is_some()` alone already demotes
/// the record's kind.
pub const VIA_MAX_HOPS: usize = 16;

/// The byte cap [`VIA_MAX_HOPS`] implies, separators included.
pub const VIA_MAX_BYTES: usize = VIA_MAX_HOPS * (crate::subject::PRINCIPAL_MAX + 1);

/// Whether a `via=` chain may be put on a wire line.
///
/// The endpoint's own rule — every comma element a principal — plus the two
/// bounds the endpoint does not have. An EMPTY chain is refused too: it is a
/// relay claim naming no relay, and it still demotes the record's kind.
///
/// IT IS NOT APPLIED IN [`Body::decode`], and that is deliberate. `via.is_some()`
/// is what makes [`crate::bridge::Bridge::classify_kind`] demote a `task` to a
/// `note` and what makes `trust_of` answer `relayed`; a parser that dropped an
/// over-long chain would turn a relayed `task` into a DIRECT one, so bounding the
/// field at the door would hand a stranger the undemoted kind that bounding it
/// was meant to deny them. The chain is bounded where it is USED, and a record
/// that fails this earns a verdict rather than a quieter delivery.
#[must_use]
pub fn via_ok(via: &str) -> bool {
    !via.is_empty()
        && via.len() <= VIA_MAX_BYTES
        && via.split(',').count() <= VIA_MAX_HOPS
        && via.split(',').all(crate::subject::is_principal)
}

// THERE IS NO DEMOTION LIST HERE, DELIBERATELY.
//
// This module used to export `ACCEPTED_ONLY_KINDS = ["task", "control",
// "answer"]`, documented as the kinds an unlisted principal may not speak as.
// Nothing read it, and it disagreed with the list that IS read —
// `bridge::DEMOTE_UNLESS_ACCEPTED = ["task", "control"]`, which is §8.4's rule
// and whose own doc argues the opposite about the third entry: an `answer`'s
// authority is the `re=` the receiver itself minted, so it is answerable by
// whoever the asker asked. Two `pub` constants stating two policies is a
// dead-code lint that never fires and a reader who believes the wrong one; the
// demotion policy lives in one place, beside the code that applies it.

/// One decoded fabric body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Body {
    /// The fold-rule version. `1` today; a reader that does not know a version
    /// treats the record as opaque.
    pub v: u32,
    /// The publisher's wall clock in ms — INFORMATIONAL. Records carry no
    /// timestamp of their own, and a broker-stamped time is a named seed.
    pub t: u64,
    /// The session a NODE published on behalf of. Attested, never a claim: the
    /// node's `<src>` is cap-forced.
    pub from: Option<String>,
    /// The offset this record answers.
    pub re: Option<u64>,
    /// An advisory deadline in ms.
    pub dl: Option<u64>,
    /// The target session's public launch nonce — MANDATORY on `term/in` and
    /// `control` (§7). A freshness fence, not a secret.
    pub epoch: Option<String>,
    /// An optional `<seq>:<fp16>` freshness fence on `term/in` only.
    pub gen: Option<String>,
    /// The relay chain. Any `via=` at all makes the message `trust=relayed` and
    /// `kind=note demoted=<k>`, whatever the recipient's allowlist says (§6.7).
    pub via: Option<String>,
    /// The message text, already pct-DECODED.
    pub text: String,
    /// Tokens this build does not know, kept so a round trip is lossless and a
    /// newer publisher's fields are visible in a diagnostic rather than erased.
    pub unknown: BTreeMap<String, String>,
}

impl Body {
    /// A `v=1` body stamped with `t`.
    #[must_use]
    pub fn new(t_ms: u64) -> Self {
        Self {
            v: 1,
            t: t_ms,
            ..Self::default()
        }
    }

    /// Render the body: the one line, then — when `raw` is `Some` — a `len=` and
    /// that many bytes after a newline.
    ///
    /// `text=` and the raw tail are alternatives, not both: the design gives
    /// `len=` its own meaning (raw bytes follow) and a body that set both would
    /// leave a reader to guess which one is the message.
    #[must_use]
    pub fn encode(&self, raw: Option<&[u8]>) -> Vec<u8> {
        let mut line = format!("v={} t={}", self.v, self.t);
        for (key, value) in [
            ("from", self.from.clone()),
            ("epoch", self.epoch.clone()),
            ("gen", self.gen.clone()),
            ("via", self.via.clone()),
        ] {
            if let Some(v) = value {
                line.push_str(&format!(" {key}={v}"));
            }
        }
        if let Some(re) = self.re {
            line.push_str(&format!(" re={re}"));
        }
        if let Some(dl) = self.dl {
            line.push_str(&format!(" dl={dl}"));
        }
        for (k, v) in &self.unknown {
            line.push_str(&format!(" {k}={v}"));
        }
        match raw {
            Some(bytes) => {
                line.push_str(&format!(" len={}", bytes.len()));
                let mut out = line.into_bytes();
                out.push(b'\n');
                out.extend_from_slice(bytes);
                out
            }
            None => {
                line.push_str(&format!(" text={}", crate::pct::encode(&self.text)));
                line.into_bytes()
            }
        }
    }

    /// Parse a body. TOTAL: never fails, never panics. A body that is not even
    /// UTF-8 decodes lossily and yields whatever tokens survive, because the
    /// alternative — dropping the record — would lose a message on a peer's
    /// encoding bug rather than on a policy.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> (Self, Option<Vec<u8>>) {
        let split = bytes.iter().position(|b| *b == b'\n');
        let (head, tail) = match split {
            Some(i) => (&bytes[..i], Some(&bytes[i + 1..])),
            None => (bytes, None),
        };
        let head = String::from_utf8_lossy(head);
        let mut body = Body::default();
        let mut declared_len: Option<usize> = None;
        for tok in head.split(' ').filter(|t| !t.is_empty()) {
            let Some((k, val)) = tok.split_once('=') else {
                continue;
            };
            match k {
                "v" => body.v = val.parse().unwrap_or(0),
                "t" => body.t = val.parse().unwrap_or(0),
                "from" => body.from = Some(val.to_string()),
                "re" => body.re = val.parse().ok(),
                "dl" => body.dl = val.parse().ok(),
                "epoch" => body.epoch = Some(val.to_string()),
                "gen" => body.gen = Some(val.to_string()),
                "via" => body.via = Some(val.to_string()),
                "text" => body.text = crate::pct::decode(val),
                "len" => declared_len = val.parse().ok(),
                _ => {
                    body.unknown.insert(k.to_string(), val.to_string());
                }
            }
        }
        // The raw tail exists only when `len=` announced it, and it is CLAMPED to
        // what actually arrived: a `len=` larger than the record is a lie the
        // parser must not turn into a panic or a read past the buffer.
        let raw = match (declared_len, tail) {
            (Some(n), Some(t)) => Some(t[..n.min(t.len())].to_vec()),
            _ => None,
        };
        (body, raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line round-trips, and the fields the design forbids stay absent: a
    /// body has no `to=`, no `kind=`, no `id=`, no `trust=`, because every one of
    /// those is either the address, the offset, or the receiver's own verdict.
    #[test]
    fn a_text_body_round_trips_and_names_no_forbidden_field() {
        let mut b = Body::new(1_756_389_000_123);
        b.from = Some("s-abc".into());
        b.re = Some(90_312);
        b.dl = Some(240_000);
        b.text = "which branch has the fixture?".into();
        let wire = b.encode(None);
        let line = String::from_utf8(wire.clone()).expect("ascii line");
        assert!(line.starts_with("v=1 t=1756389000123 from=s-abc re=90312 dl=240000 text="));
        for forbidden in [" to=", " kind=", " id=", " trust=", " demoted="] {
            assert!(!line.contains(forbidden), "{line} carries {forbidden}");
        }
        let (got, raw) = Body::decode(&wire);
        assert_eq!(raw, None);
        assert_eq!(got, b);
    }

    /// The `len=` form carries arbitrary bytes — newlines, NULs, invalid UTF-8 —
    /// which is what the drive face needs and what `text=` cannot express.
    #[test]
    fn a_raw_body_carries_any_byte() {
        let mut b = Body::new(7);
        b.epoch = Some("0123456789abcdef0123456789abcdef".into());
        b.gen = Some("42:beefbeefbeefbeef".into());
        let raw: Vec<u8> = vec![0x00, b'\n', 0xff, b'y', 0x1b, b'[', b'A'];
        let wire = b.encode(Some(&raw));
        let (got, back) = Body::decode(&wire);
        assert_eq!(back.as_deref(), Some(&raw[..]));
        assert_eq!(got.epoch, b.epoch);
        assert_eq!(got.gen, b.gen);
    }

    /// TOTAL over hostile input: a truncated tail, a lying `len=`, a body that is
    /// not UTF-8, an empty record. None of these may panic, because every one of
    /// them is a byte string a stranger can put on the bus.
    #[test]
    fn decoding_is_total_over_hostile_bytes() {
        for bytes in [
            &b""[..],
            b"\n",
            b"v=1 len=999999\nshort",
            b"v=1 t=notanumber re=also-not text=%",
            b"=====",
            &[0xff, 0xfe, b'\n', 0xff],
        ] {
            let (body, raw) = Body::decode(bytes);
            // A lying `len=` is clamped to what arrived rather than read past.
            if let Some(raw) = raw {
                assert!(raw.len() <= bytes.len());
            }
            let _ = body.v;
        }
    }

    /// ONE DEMOTION LIST IN THE CRATE, and it is the one the bridge reads.
    ///
    /// This module exported a second, wider one (`ACCEPTED_ONLY_KINDS`, with
    /// `answer` in it) that nothing consumed. A reader taking it at its word —
    /// or a later rung reaching for the `pub` constant to reimplement the policy
    /// — would demote an unlisted principal's `answer` and break request/reply
    /// for every peer not on the allowlist. The guard is over the source because
    /// the defect was the EXISTENCE of the second spelling, not its value.
    #[test]
    fn the_demotion_policy_is_stated_exactly_once_in_this_crate() {
        // The needle is SPLIT so this file's own prose about the deleted
        // constant is not a counterexample: `concat!` joins at compile time and
        // the source text read back never holds the whole token.
        let declared = concat!("const ACCEPTED_ONLY", "_KINDS");
        assert!(
            !include_str!("body.rs").contains(declared),
            "body.rs must not declare a second demotion list beside \
             bridge::DEMOTE_UNLESS_ACCEPTED"
        );
        let one = concat!("const DEMOTE_UNLESS", "_ACCEPTED: [&str; 2]");
        assert_eq!(
            include_str!("bridge.rs").matches(one).count(),
            1,
            "§8.4's list is {{task, control}} and it is declared once"
        );
    }

    /// An unknown token survives the round trip. A newer publisher's field must
    /// be visible in a diagnostic, not silently erased by an older bridge.
    #[test]
    fn an_unknown_token_survives() {
        let (body, _) = Body::decode(b"v=1 t=3 lang=en text=hi");
        assert_eq!(body.unknown.get("lang").map(String::as_str), Some("en"));
        let line = String::from_utf8(body.encode(None)).expect("ascii");
        assert!(line.contains(" lang=en"), "{line}");
    }
}
