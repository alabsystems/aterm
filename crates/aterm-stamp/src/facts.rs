// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable compile facts, derived from the same [`Stamp`] the running binary installs.
//! Identity rewriting never writes this record. Offline readers use its named object section,
//! never a matching string elsewhere in an executable.

use crate::Stamp;

#[path = "facts_macho.rs"]
mod macho;
pub use macho::macho_records;

/// The exact early front-door request; it performs no application startup.
pub const ARG: &str = "--build-facts";
/// Length of the immutable section and of the early reply, including zero padding.
pub const LEN: usize = 512;
const HEADER: &str = "aterm-compile-facts 1\n";
/// The Mach-O section name (under `__TEXT`).
pub const SECTION: &[u8] = b"__aterm_facts";

/// Facts of one compiled architecture, read without executing the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts<'a> {
    pub profile: &'a str,
    pub debug_assertions: bool,
    pub target_arch: &'a str,
    pub compiler_commit: &'a str,
    pub compiler_host: &'a str,
    pub compiler_flavor: &'a str,
    pub trust_verify: &'a str,
}

/// Encode the binary's compile facts at compile time. Oversized records fail compilation;
/// unknown defaults remain unreadable to admission ([`decode`]).
/// # Panics
/// The encoded record exceeds its fixed section.
#[must_use]
pub const fn encode(stamp: Stamp) -> [u8; LEN] {
    let fields = [
        ("profile", stamp.build_profile),
        (
            "debug_assertions",
            match stamp.debug_assertions {
                Some(true) => "true",
                Some(false) => "false",
                None => "unknown",
            },
        ),
        ("target_arch", stamp.target_arch),
        ("compiler_commit", stamp.compiler_commit),
        ("compiler_host", stamp.compiler_host),
        ("compiler_flavor", stamp.compiler_flavor),
        ("trust_verify", stamp.trust_verify),
    ];
    let mut out = [0; LEN];
    let mut at = append(&mut out, 0, HEADER);
    let mut i = 0;
    while i < fields.len() {
        at = append(&mut out, at, fields[i].0);
        at = append(&mut out, at, "=");
        at = append(&mut out, at, fields[i].1);
        at = append(&mut out, at, "\n");
        i += 1;
    }
    out
}

const fn append(out: &mut [u8; LEN], mut at: usize, value: &str) -> usize {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        assert!(at < LEN, "compile facts exceed their fixed section");
        out[at] = bytes[i];
        at += 1;
        i += 1;
    }
    at
}

/// Decode exactly one record. Unknown versions, missing/repeated fields, unknown facts,
/// nonzero padding and malformed values refuse admission.
/// # Errors
/// The record has an unknown version, wrong length, unknown/malformed field or nonzero padding.
pub fn decode(record: &[u8]) -> Result<Facts<'_>, String> {
    if record.len() != LEN {
        return Err("compile facts have the wrong section length".into());
    }
    let used = record.iter().position(|byte| *byte == 0).unwrap_or(LEN);
    if record[used..].iter().any(|byte| *byte != 0) {
        return Err("compile facts have nonzero padding".into());
    }
    let text = std::str::from_utf8(&record[..used]).map_err(|_| "compile facts are not UTF-8")?;
    let mut lines = text
        .strip_prefix(HEADER)
        .ok_or("unknown compile-facts version")?
        .split_terminator('\n');
    let mut field = |name: &str| -> Result<&str, String> {
        let value = lines
            .next()
            .and_then(|line| line.strip_prefix(name))
            .and_then(|rest| rest.strip_prefix('='))
            .ok_or_else(|| format!("compile facts need {name} in order"))?;
        if value.is_empty()
            || value == "unknown"
            || value
                .bytes()
                .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        {
            return Err(format!("compile fact {name} is unknown or malformed"));
        }
        Ok(value)
    };
    let profile = field("profile")?;
    let debug_assertions = match field("debug_assertions")? {
        "true" => true,
        "false" => false,
        _ => return Err("compile debug_assertions is not a boolean".into()),
    };
    let target_arch = field("target_arch")?;
    let compiler_commit = field("compiler_commit")?;
    let compiler_host = field("compiler_host")?;
    let compiler_flavor = field("compiler_flavor")?;
    let trust_verify = field("trust_verify")?;
    if lines.next().is_some()
        || !text.ends_with('\n')
        || !matches!(compiler_flavor, "r" | "t")
        || !matches!(trust_verify, "on" | "off")
        || compiler_commit.len() != 40
        || !compiler_commit.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("compile facts have extra or malformed fields".into());
    }
    Ok(Facts {
        profile,
        debug_assertions,
        target_arch,
        compiler_commit,
        compiler_host,
        compiler_flavor,
        trust_verify,
    })
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod tests;
