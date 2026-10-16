// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fabric body (§4.1) — one ASCII line of `key=value` tokens, optionally
//! followed by `len=<n>` raw bytes.
//!
//! ```text
//! v=1 t=<ms> [from=<sid>] [re=<offset>] [dl=<ms>] [verdict=<handled|refused|deferred>]
//!            [epoch=<hex32>] [gen=<seq>:<fp16>] [via=<p>[,<p>…]] [text=<pct>] [len=<n>]
//! ["\n" ‖ <n> raw bytes]
//! ```
//!
//! What is NOT in it is the point: no sender-chosen `from=`, no `to=`, no
//! `kind=` (except a broadcast's, see [`Body::kind`]), no `id=`, no `trust=`.
//! Those are the subject's segments, the record's offset, and a label the
//! receiver computes. The one attested exception is `from=<sid>`: a node adds it
//! for a session it hosts, and the node's `<src>` segment is cap-forced, so it
//! could type as that session anyway (§8.3).
//!
//! The parser is TOTAL over arbitrary bytes: an unknown token is ignored, a
//! malformed number is `None`, and nothing here can panic on a hostile body.

use std::collections::BTreeMap;

#[cfg(test)]
thread_local! {
    static DECODE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn decode_calls() -> usize {
    DECODE_CALLS.with(std::cell::Cell::get)
}

// The closed word sets a body is held to (§4.2 kinds, R8 verdicts, the `via=`
// bounds) are the fabric's, defined once in `aterm-types` because the endpoint
// holds its `deliver` and `post` lines to them too.
//
// `via_ok` is NOT applied in [`Body::decode`], deliberately: `via.is_some()` is
// what makes [`crate::bridge::Bridge::classify_kind`] demote a `task` and
// `trust_of` answer `relayed`, so dropping an over-long chain at the door would
// hand a stranger the undemoted kind. It is bounded where used, and a failing
// record earns a verdict.
pub use aterm_types::fabric_grammar::{
    is_kind, is_verdict, via_ok, KINDS, VERDICTS, VIA_MAX_BYTES, VIA_MAX_HOPS,
};

// No demotion list here: the policy lives in `bridge::DEMOTE_UNLESS_ACCEPTED`,
// beside the code that applies it.

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
    /// The receipt's verdict (R8): on a `kind=ack` record the RECIPIENT's bridge
    /// publishes when its session ran `inbox seen <id> handled|refused|deferred`
    /// on the `ask`/`task` at `re=`. One of [`VERDICTS`]; anything else is left
    /// in `unknown` by the decoder and never reaches a `deliver` line.
    pub verdict: Option<String>,
    /// The target session's public launch nonce (§7). Carried losslessly and
    /// rendered by `ls`; nothing in this crate checks it.
    pub epoch: Option<String>,
    /// The `<seq>:<fp16>` freshness fence §6.6 defined for `term/in`. Carried
    /// and rendered, never checked.
    pub gen: Option<String>,
    /// The relay chain. Any `via=` at all makes the message `trust=relayed` and
    /// `kind=note demoted=<k>`, whatever the recipient's allowlist says (§6.7).
    pub via: Option<String>,
    /// A BROADCAST's kind, carried IN THE BODY.
    ///
    /// A directed message's kind is the subject's last segment; a broadcast's
    /// last segment is the topic (`say/<topic>/<kind>` would multiply topics by
    /// kinds on the subject budget), so its kind rides here. Closed to
    /// [`KINDS`] at the decoder, like `verdict`.
    pub kind: Option<String>,
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

    /// Render the body: the one line, its message in `text=`.
    ///
    /// The `len=` raw-tail form has no writer; [`Body::decode`] still clamps a
    /// `len=` a stranger puts on the bus.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut line = format!("v={} t={}", self.v, self.t);
        for (key, value) in [
            ("from", self.from.clone()),
            ("epoch", self.epoch.clone()),
            ("gen", self.gen.clone()),
            ("via", self.via.clone()),
            ("kind", self.kind.clone()),
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
        if let Some(v) = &self.verdict {
            line.push_str(&format!(" verdict={v}"));
        }
        for (k, v) in &self.unknown {
            line.push_str(&format!(" {k}={v}"));
        }
        line.push_str(&format!(" text={}", crate::pct::encode(&self.text)));
        line.into_bytes()
    }

    /// Parse a body. Total: never fails, never panics; non-UTF-8 decodes lossily
    /// so a peer's encoding bug never loses a message.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> (Self, Option<Vec<u8>>) {
        #[cfg(test)]
        DECODE_CALLS.with(|calls| calls.set(calls.get() + 1));
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
                // CLOSED at the decoder: a token outside the set is an unknown
                // field, not a verdict, so nothing downstream has to re-check.
                "verdict" if is_verdict(val) => body.verdict = Some(val.to_string()),
                "epoch" => body.epoch = Some(val.to_string()),
                "gen" => body.gen = Some(val.to_string()),
                "via" => body.via = Some(val.to_string()),
                // Closed at the decoder, as `verdict` is.
                "kind" if KINDS.contains(&val) => {
                    body.kind = Some(val.to_string());
                }
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

    /// A broadcast's kind round-trips in the body and is closed to [`KINDS`].
    #[test]
    fn a_broadcast_kind_round_trips_and_is_closed() {
        let mut b = Body::new(7);
        b.kind = Some("task".to_string());
        b.text = "go".to_string();
        let wire = b.encode();
        let line = String::from_utf8_lossy(&wire).into_owned();
        assert!(line.contains(" kind=task"), "{line}");
        let (back, _) = Body::decode(&wire);
        assert_eq!(back.kind.as_deref(), Some("task"));

        let (invented, _) = Body::decode(b"v=1 t=1 kind=shout text=hi");
        assert_eq!(invented.kind, None, "an unknown kind is not a kind");
        assert_eq!(
            invented.unknown.get("kind").map(String::as_str),
            Some("shout"),
            "it is kept as an unknown field, losslessly"
        );
        // A directed message carries no body kind at all: its kind is the
        // subject's last segment.
        let (plain, _) = Body::decode(b"v=1 t=1 text=hi");
        assert_eq!(plain.kind, None);
    }

    /// The line round-trips, and the forbidden fields stay absent.
    #[test]
    fn a_text_body_round_trips_and_names_no_forbidden_field() {
        let mut b = Body::new(1_756_389_000_123);
        b.from = Some("s-abc".into());
        b.re = Some(90_312);
        b.dl = Some(240_000);
        b.text = "which branch has the fixture?".into();
        let wire = b.encode();
        let line = String::from_utf8(wire.clone()).expect("ascii line");
        assert!(line.starts_with("v=1 t=1756389000123 from=s-abc re=90312 dl=240000 text="));
        for forbidden in [" to=", " kind=", " id=", " trust=", " demoted="] {
            assert!(!line.contains(forbidden), "{line} carries {forbidden}");
        }
        let (got, raw) = Body::decode(&wire);
        assert_eq!(raw, None);
        assert_eq!(got, b);
    }

    /// A receipt's `verdict=` (R8) round-trips for the three words; anything
    /// else decodes as an unknown token.
    #[test]
    fn a_verdict_round_trips_and_an_unknown_one_is_not_a_verdict() {
        let mut b = Body::new(9);
        b.re = Some(41);
        b.verdict = Some("refused".into());
        b.text = "ack re=41 verdict=refused".into();
        let wire = b.encode();
        let line = String::from_utf8(wire.clone()).expect("ascii line");
        assert!(line.contains(" re=41 verdict=refused text="), "{line}");
        let (got, _) = Body::decode(&wire);
        assert_eq!(got.verdict.as_deref(), Some("refused"));
        assert_eq!(got, b);
        for word in ["handled", "deferred"] {
            assert!(is_verdict(word));
        }
        let (hostile, _) = Body::decode(b"v=1 t=1 re=41 verdict=handled%20now text=x");
        assert_eq!(hostile.verdict, None, "not one of the three words");
        assert_eq!(
            hostile.unknown.get("verdict").map(String::as_str),
            Some("handled%20now"),
            "kept as an unknown token, never as a verdict"
        );
    }

    /// Decoding is total over hostile bytes.
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

    /// An unknown token survives the round trip.
    #[test]
    fn an_unknown_token_survives() {
        let (body, _) = Body::decode(b"v=1 t=3 lang=en text=hi");
        assert_eq!(body.unknown.get("lang").map(String::as_str), Some("en"));
        let line = String::from_utf8(body.encode()).expect("ascii");
        assert!(line.contains(" lang=en"), "{line}");
    }
}
