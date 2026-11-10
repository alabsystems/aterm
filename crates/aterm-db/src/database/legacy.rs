// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Portable validation of the retained legacy fleet record format. The live
//! replica is Unix-only; decoding historical commands has the same rules on
//! every platform.

use crate::{Error, Result};
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::{Map, Value};

/// SHA-256 digests of the 3 personal email addresses that must always remain `protected: true`
/// and never store a password in any operation.
const PROTECTED_EMAIL_DIGESTS: [[u8; 32]; 3] = [
    [
        0x45, 0x1d, 0xba, 0x8f, 0x26, 0x17, 0x58, 0xf5, 0x48, 0x6f, 0x3e, 0x97, 0x35, 0xec, 0xe9,
        0x08, 0xe6, 0xe7, 0xcb, 0xcf, 0xd6, 0x7d, 0x79, 0x55, 0xbd, 0x31, 0x99, 0x04, 0xa4, 0xb9,
        0x18, 0x3e,
    ],
    [
        0x69, 0x77, 0x11, 0xa4, 0x89, 0x55, 0xca, 0xfd, 0x0a, 0xad, 0x70, 0xe2, 0xb4, 0x04, 0xd6,
        0x0c, 0xf3, 0xb9, 0x94, 0xe2, 0xe3, 0x8b, 0xd2, 0x22, 0x1b, 0xbd, 0xeb, 0x78, 0xbf, 0xd5,
        0xc2, 0x0b,
    ],
    [
        0x6a, 0xe3, 0x1e, 0xb5, 0x38, 0xf4, 0x7c, 0x79, 0xac, 0xcd, 0x2a, 0x37, 0x58, 0x3c, 0x7c,
        0xb1, 0xc7, 0xa5, 0x49, 0x29, 0x57, 0x5a, 0x5e, 0x3f, 0x9b, 0x49, 0x71, 0xac, 0x1d, 0xbd,
        0xb4, 0x8f,
    ],
];

pub(crate) fn with_normalized_phone_key<R>(raw: &str, f: impl FnOnce(&str) -> R) -> R {
    let trimmed = raw.trim();
    let mut digits = [0u8; 12];
    let mut len = 0;
    for b in trimmed.bytes().filter(u8::is_ascii_digit) {
        if len == digits.len() {
            return f(trimmed);
        }
        digits[len] = b;
        len += 1;
    }
    let ten = if len == 10 {
        Some(&digits[..10])
    } else if len == 11 && digits[0] == b'1' {
        Some(&digits[1..11])
    } else {
        None
    };
    if let Some(d) = ten {
        let buf = [
            d[0], d[1], d[2], b'-', d[3], d[4], d[5], b'-', d[6], d[7], d[8], d[9],
        ];
        let s = std::str::from_utf8(&buf).expect("ASCII digits and hyphens");
        f(s)
    } else {
        f(trimmed)
    }
}

pub(crate) fn sha256_ascii_lowercase(bytes: &[u8]) -> [u8; 32] {
    if !bytes.iter().any(u8::is_ascii_uppercase) {
        return Sha256::digest(bytes);
    }
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 256];
    for chunk in bytes.chunks(buf.len()) {
        for (dst, &src) in buf[..chunk.len()].iter_mut().zip(chunk) {
            *dst = src.to_ascii_lowercase();
        }
        hasher.update(&buf[..chunk.len()]);
    }
    hasher.finalize()
}

/// Convert a natural primary key (`m104`, `root@example.com`, `+1-555-234-5678`,
/// `acc-001`) into a uniform `crypto::name`-safe storage key (`<prefix>-<24 hex chars>`).
pub fn storage_key(logical_table: &str, natural_key: &str) -> Result<String> {
    let trimmed = natural_key.trim();
    if trimmed.is_empty() {
        return Err(Error::new("table primary key must not be empty"));
    }
    let prefix = match logical_table {
        "computers" => "mc",
        "emails" => "em",
        "phones" => "ph",
        "ai_accounts" => "ac",
        _ => "rk",
    };
    let digest = if logical_table == "phones" {
        with_normalized_phone_key(trimmed, |s| sha256_ascii_lowercase(s.as_bytes()))
    } else {
        sha256_ascii_lowercase(trimmed.as_bytes())
    };
    let mut out = String::with_capacity(prefix.len() + 1 + 24);
    out.push_str(prefix);
    out.push('-');
    hex::push_lower(&mut out, &digest[..12]);
    Ok(out)
}

/// Return true if `email` is one of the 3 personal email addresses that must always be `protected: true`.
pub fn is_always_protected_email(email: &str) -> bool {
    let digest = sha256_ascii_lowercase(email.trim().as_bytes());
    PROTECTED_EMAIL_DIGESTS.contains(&digest)
}

/// Validate a decoded `"emails"` record inside `Command::decode` so neither local writes nor
/// remote peers can ever bypass the `protected: true` no-password invariant.
pub fn validate_email_record(stored_key: &str, value: &Value) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| Error::new("emails record must be a JSON object"))?;
    if let Some(del) = obj.get("_deleted") {
        if del.as_bool() == Some(true) && obj.len() == 1 {
            return Ok(());
        }
        return Err(Error::new(
            "emails tombstone must contain only \"_deleted\": true",
        ));
    }
    let email = obj
        .get("email")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new("emails record requires string \"email\" field"))?;
    let trimmed = email.trim();
    if trimmed.is_empty()
        || !trimmed.contains('@')
        || trimmed.len() != email.len()
        || trimmed.bytes().any(|b| b.is_ascii_uppercase())
    {
        return Err(Error::new(
            "emails record requires lowercase email address containing '@'",
        ));
    }
    let expected_skey = storage_key("emails", trimmed)?;
    if stored_key != expected_skey {
        return Err(Error::new(
            "emails storage key does not match canonical email address",
        ));
    }
    let protected = obj
        .get("protected")
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::new("emails record requires boolean \"protected\" field"))?;
    if is_always_protected_email(trimmed) && !protected {
        return Err(Error::new(
            "protected personal email address cannot have protected set to false",
        ));
    }
    if let Some(pw) = obj.get("password")
        && !pw.is_null()
        && pw.as_str().is_none()
    {
        return Err(Error::new(
            "emails \"password\" field must be a string or null",
        ));
    }
    if protected && credential_fields(obj) {
        return Err(Error::new(
            "protected real email rows must not store credential fields",
        ));
    }
    Ok(())
}

/// Whether a field carries credentials rather than an account identifier or secret reference.
fn credential_field(key: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "password",
        "passwords",
        "passwd",
        "passwds",
        "passphrase",
        "passphrases",
        "secret",
        "secrets",
        "token",
        "tokens",
        "apikey",
        "apikeys",
        "privatekey",
        "privatekeys",
        "credential",
        "credentials",
        "authorization",
        "authorizations",
        "authheader",
        "authheaders",
        "cookie",
        "cookies",
        "totp",
        "otp",
        "seed",
        "seeds",
        "authenticator",
        "recoverycode",
        "recoverycodes",
        "backupcode",
        "backupcodes",
    ];
    let mut buf = [0u8; 16];
    let mut start = buf.len();
    for b in key.bytes().rev().filter(u8::is_ascii_alphanumeric).take(16) {
        start -= 1;
        buf[start] = b.to_ascii_lowercase();
    }
    let tail = &buf[start..];
    SUFFIXES
        .iter()
        .any(|suffix| tail.ends_with(suffix.as_bytes()))
}

pub(crate) fn sensitive_field(key: &str, value: &Value) -> bool {
    if value.is_null() || (key == "has_password" && value.as_bool().is_some()) {
        return false;
    }
    credential_field(key)
}

pub(crate) fn credential_fields(object: &Map) -> bool {
    object
        .iter()
        .any(|(key, value)| sensitive_field(key, value) || contains_credentials(value))
}

pub(crate) fn contains_credentials(value: &Value) -> bool {
    match value {
        Value::Object(object) => credential_fields(object),
        Value::Array(values) => values.iter().any(contains_credentials),
        _ => false,
    }
}
