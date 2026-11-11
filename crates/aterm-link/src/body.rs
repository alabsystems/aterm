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
    is_verdict, via_ok, KINDS, VERDICTS, VIA_MAX_BYTES, VIA_MAX_HOPS,
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
                "v" => body.v = crate::parse_uint(val).unwrap_or(0),
                "t" => body.t = crate::parse_uint(val).unwrap_or(0),
                "from" => body.from = Some(val.to_string()),
                "re" => body.re = crate::parse_uint(val),
                "dl" => body.dl = crate::parse_uint(val),
                "verdict" if is_verdict(val) => body.verdict = Some(val.to_string()),
                "epoch" => body.epoch = Some(val.to_string()),
                "gen" => body.gen = Some(val.to_string()),
                "via" => body.via = Some(val.to_string()),
                "kind" if KINDS.contains(&val) => {
                    body.kind = Some(val.to_string());
                }
                "text" => body.text = crate::pct::decode(val),
                "len" => declared_len = crate::parse_uint(val),
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
#[path = "body_tests.rs"]
mod tests;
