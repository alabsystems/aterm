// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared `api-key-v1` envelope. Parsing establishes format and provider,
//! never authority, account identity, permission to execute, or a spending cap.

use aterm_uds::secret::Secret;
use serde::de::{self, MapAccess, Visitor};
use std::fmt;

const INPUT_MAX: usize = 64 * 1024;
const KEY_MAX: usize = 16 * 1024;

/// Encode the same closed envelope native preparation and the request broker
/// consume. The caller remains responsible for its original raw key buffer.
/// # Errors
/// Invalid UTF-8, empty/non-header-safe/oversize key, or envelope encoding errors.
pub fn encode(provider: &str, key: &[u8]) -> Result<Secret, String> {
    if !valid_provider(provider) || !valid_key(key) {
        return Err("mismatched or non-header-safe API key input".into());
    }
    let key = std::str::from_utf8(key).map_err(|_| "API key is not text")?;
    let key = zeroize::Zeroizing::new(
        aterm_json::quoted_string(key).map_err(|_| "API key could not be encoded")?,
    );
    let provider_json = zeroize::Zeroizing::new(
        aterm_json::quoted_string(provider).map_err(|_| "API key provider could not be encoded")?,
    );
    let mut bytes = Vec::with_capacity(key.len() + provider_json.len() + 64);
    bytes.extend_from_slice(br#"{"schema":1,"kind":"api-key","provider":"#);
    bytes.extend_from_slice(&provider_json);
    bytes.extend_from_slice(br#", "key":"#);
    bytes.extend_from_slice(&key);
    bytes.push(b'}');
    let encoded = Secret::from_vec(bytes);
    Ok(encoded)
}

fn valid_provider(provider: &str) -> bool {
    !provider.is_empty()
        && provider.len() <= 64
        && provider
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_key(key: &[u8]) -> bool {
    !key.is_empty() && key.len() <= KEY_MAX && key.iter().all(|b| (0x21..=0x7e).contains(b))
}

/// Read exactly four non-duplicate fields and return a protected key copy.
/// The expected provider comes from authenticated account configuration.
/// # Errors
/// Oversize, malformed, missing, extra or duplicate fields; schema/provider
/// mismatch; or a key that cannot be put in one HTTP header value.
pub fn decode(bytes: &[u8], expected: &str) -> Result<Secret, String> {
    if bytes.len() > INPUT_MAX || !valid_provider(expected) {
        return Err("API key input exceeds its bound".into());
    }
    let parsed: Envelope =
        aterm_json::from_slice(bytes).map_err(|_| "invalid API key input envelope")?;
    if parsed.provider.expose() != expected.as_bytes() || !valid_key(parsed.key.expose()) {
        return Err("mismatched or non-header-safe API key input".into());
    }
    Ok(parsed.key)
}

struct Envelope {
    provider: Secret,
    key: Secret,
}

impl<'de> serde::Deserialize<'de> for Envelope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EnvelopeVisitor;
        impl<'de> Visitor<'de> for EnvelopeVisitor {
            type Value = Envelope;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a closed API key envelope")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Envelope, M::Error> {
                let (mut schema, mut kind, mut provider, mut key) = (false, false, None, None);
                while let Some(field) = map.next_key::<String>()? {
                    match field.as_str() {
                        "schema" if !schema => {
                            if map.next_value::<u64>()? != 1 {
                                return Err(de::Error::custom("schema"));
                            }
                            schema = true;
                        }
                        "kind" if !kind => {
                            let value = Secret::from_vec(map.next_value::<String>()?.into_bytes());
                            if value.expose() != b"api-key" {
                                return Err(de::Error::custom("kind"));
                            }
                            kind = true;
                        }
                        "provider" if provider.is_none() => {
                            provider =
                                Some(Secret::from_vec(map.next_value::<String>()?.into_bytes()))
                        }
                        "key" if key.is_none() => {
                            key = Some(Secret::from_vec(map.next_value::<String>()?.into_bytes()))
                        }
                        _ => return Err(de::Error::custom("extra or duplicate field")),
                    }
                }
                if !schema || !kind {
                    return Err(de::Error::custom("missing field"));
                }
                Ok(Envelope {
                    provider: provider.ok_or_else(|| de::Error::custom("provider"))?,
                    key: key.ok_or_else(|| de::Error::custom("key"))?,
                })
            }
        }
        deserializer.deserialize_map(EnvelopeVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trip_and_provider_binding() {
        for provider in ["google", "meta"] {
            let encoded = encode(provider, b"synthetic-key").unwrap();
            let key = decode(encoded.expose(), provider).unwrap();
            assert_eq!(key.expose(), b"synthetic-key");
            assert!(!format!("{key:?}").contains("synthetic-key"));
            assert!(decode(encoded.expose(), "another-provider").is_err());
        }
    }

    #[test]
    fn ambiguous_and_unsafe_key_envelopes_are_refused() {
        for extra in [
            r#","schema":1"#,
            r#","kind":"api-key""#,
            r#","provider":"google""#,
            r#","key":"second""#,
            r#","extra":true"#,
        ] {
            let encoded = format!(
                r#"{{"schema":1,"kind":"api-key","provider":"google","key":"first"{extra}}}"#
            );
            assert!(decode(encoded.as_bytes(), "google").is_err());
        }
        for key in [b"".as_slice(), b"line\nfeed", b"white space", &[0x80]] {
            assert!(encode("google", key).is_err());
        }
        assert!(encode("google", &vec![b'x'; KEY_MAX + 1]).is_err());
        assert!(decode(&vec![b' '; INPUT_MAX + 1], "google").is_err());
        assert!(decode(br#"{"schema":1,"kind":"api-key","key":"first"}"#, "google").is_err());
    }
}
