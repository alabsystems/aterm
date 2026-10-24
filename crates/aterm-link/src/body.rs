// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Total ASCII `key=value` fabric body parser and serializer ([`Body`]).

use std::collections::BTreeMap;

#[cfg(test)]
thread_local! {
    static DECODE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn decode_calls() -> usize {
    DECODE_CALLS.with(std::cell::Cell::get)
}

pub use aterm_types::fabric_grammar::{
    is_kind, is_verdict, via_ok, KINDS, VERDICTS, VIA_MAX_BYTES, VIA_MAX_HOPS,
};

/// One decoded fabric body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Body {
    /// The fold-rule version.
    pub v: u32,
    /// The publisher's wall clock in ms.
    pub t: u64,
    /// The session a node published on behalf of.
    pub from: Option<String>,
    /// The offset this record answers.
    pub re: Option<u64>,
    /// An advisory deadline in ms.
    pub dl: Option<u64>,
    /// The receipt's verdict ([`VERDICTS`]).
    pub verdict: Option<String>,
    /// The target session's public launch nonce.
    pub epoch: Option<String>,
    /// The `<seq>:<fp16>` freshness fence for `term/in`.
    pub gen: Option<String>,
    /// The relay chain.
    pub via: Option<String>,
    /// A broadcast's kind ([`KINDS`]).
    pub kind: Option<String>,
    /// The percent-decoded message text.
    pub text: String,
    /// Tokens this build does not know.
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

    /// Render the body ([`Body::decode`]).
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

    /// Parse a body.
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
                "verdict" if is_verdict(val) => body.verdict = Some(val.to_string()),
                "epoch" => body.epoch = Some(val.to_string()),
                "gen" => body.gen = Some(val.to_string()),
                "via" => body.via = Some(val.to_string()),
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
        let (plain, _) = Body::decode(b"v=1 t=1 text=hi");
        assert_eq!(plain.kind, None);
    }

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
            if let Some(raw) = raw {
                assert!(raw.len() <= bytes.len());
            }
            let _ = body.v;
        }
    }

    #[test]
    fn an_unknown_token_survives() {
        let (body, _) = Body::decode(b"v=1 t=3 lang=en text=hi");
        assert_eq!(body.unknown.get("lang").map(String::as_str), Some("en"));
        let line = String::from_utf8(body.encode()).expect("ascii");
        assert!(line.contains(" lang=en"), "{line}");
    }
}
