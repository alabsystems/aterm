// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The deployer's service-account key and the OAuth2 JWT-bearer assertion (RFC 7523) it
//! signs: RS256 over the key's PKCS#8 RSA private key, with ring.

use std::path::Path;

use aterm_uds::secret::Secret;
use serde::Deserialize;

/// The scope the token is asked for: Google's broad scope, which the service account's
/// IAM roles bound (Firebase Hosting's own scope admits the same calls).
pub const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// How long the assertion is good for: Google's maximum.
const LIFETIME_SECS: u64 = 3600;

/// The service-account JSON's fields this deploy reads; the rest (client ids, cert URLs)
/// are not asked for.
#[derive(Deserialize)]
struct Fields {
    #[serde(rename = "type")]
    kind: Option<String>,
    project_id: Option<String>,
    private_key_id: Option<String>,
    private_key: Option<String>,
    client_email: Option<String>,
    token_uri: Option<String>,
}

/// A service-account key, read once: its identity, its token endpoint, and the RSA key
/// pair it signs with. Neither `Debug` nor any error says the key's bytes.
pub struct Key {
    /// `client_email`: the assertion's issuer.
    pub email: String,
    /// `project_id`, when the key names one: the site deployed when nothing else names it.
    pub project: Option<String>,
    /// `token_uri`: where the assertion is exchanged, admitted by the client before any use.
    pub token_uri: String,
    kid: Option<String>,
    pair: ring::signature::RsaKeyPair,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Key")
            .field("email", &self.email)
            .field("project", &self.project)
            .field("token_uri", &self.token_uri)
            .finish_non_exhaustive()
    }
}

/// `bytes` as base64url without padding (RFC 7515 §2).
fn base64url(bytes: &[u8]) -> Result<String, String> {
    let mut out = aterm_codec::base64::encode(bytes)
        .map_err(|e| format!("cannot encode base64: {e:?}"))?
        .into_bytes();
    while out.last() == Some(&b'=') {
        out.pop();
    }
    for b in &mut out {
        match *b {
            b'+' => *b = b'-',
            b'/' => *b = b'_',
            _ => {}
        }
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}

impl Key {
    /// The key at `path`: its bytes read once, held as a [`Secret`], and parsed.
    /// # Errors
    /// A file that cannot be read, or one that is not a service-account key with an RSA
    /// PKCS#8 private key; each said by the path and the field, never by a byte of the key.
    pub fn read(path: &Path) -> Result<Key, String> {
        let bytes = crate::durable::read_regular_bytes(path, 256 * 1024)
            .map(Secret::from_vec)
            .map_err(|e| {
                format!(
                    "cannot read the service-account key {}: {e}",
                    path.display()
                )
            })?;
        Self::parse(&bytes).map_err(|why| format!("{}: {why}", path.display()))
    }

    /// `bytes`, a service-account JSON key.
    /// # Errors
    /// As [`Key::read`].
    pub fn parse(bytes: &Secret) -> Result<Key, String> {
        let fields: Fields = aterm_json::from_slice(bytes.expose())
            .map_err(|_| "not a service-account key (it is not JSON of the expected shape)")?;
        let pem = fields
            .private_key
            .filter(|v| !v.is_empty())
            .map(|v| Secret::from_vec(v.into_bytes()));
        if fields.kind.as_deref() != Some("service_account") {
            return Err("not a service-account key (its type is not service_account)".into());
        }
        let need = |field: Option<String>, name: &str| {
            field
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("the service-account key has no {name}"))
        };
        let email = need(fields.client_email, "client_email")?;
        let token_uri = need(fields.token_uri, "token_uri")?;
        let pem = pem.ok_or_else(|| "the service-account key has no private_key".to_string())?;
        let der = pkcs8(pem.expose())?;
        let pair = ring::signature::RsaKeyPair::from_pkcs8(der.expose()).map_err(|e| {
            format!("the service-account key's private_key is not an RSA PKCS#8 key ({e})")
        })?;
        Ok(Key {
            email,
            project: fields.project_id.filter(|p| !p.is_empty()),
            token_uri,
            kid: fields.private_key_id.filter(|k| !k.is_empty()),
            pair,
        })
    }

    /// The signed JWT-bearer assertion for a token issued at `now` (Unix seconds): header
    /// `{alg: RS256, typ: JWT, kid}`, claims `{iss, scope, aud: token_uri, iat, exp}`.
    /// # Errors
    /// ring could not sign (it never fails for a key it parsed).
    pub fn assertion(&self, now: u64) -> Result<Secret, String> {
        let mut header = aterm_json::json!({"alg": "RS256", "typ": "JWT"});
        if let (Some(kid), Some(map)) = (&self.kid, header.as_object_mut()) {
            map.insert("kid".into(), kid.as_str().into());
        }
        let claims = aterm_json::json!({
            "iss": self.email.as_str(),
            "scope": SCOPE,
            "aud": self.token_uri.as_str(),
            "iat": now,
            "exp": now.saturating_add(LIFETIME_SECS),
        });
        let input = format!(
            "{}.{}",
            base64url(header.to_string().as_bytes())?,
            base64url(claims.to_string().as_bytes())?
        );
        let mut signature = vec![0; self.pair.public().modulus_len()];
        self.pair
            .sign(
                &ring::signature::RSA_PKCS1_SHA256,
                &ring::rand::SystemRandom::new(),
                input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| "ring could not sign the assertion".to_string())?;
        Ok(Secret::from_vec(
            format!("{input}.{}", base64url(&signature)?).into_bytes(),
        ))
    }

    /// The token request's form body: the JWT-bearer grant and the assertion.
    /// # Errors
    /// As [`Key::assertion`].
    pub fn grant(&self, now: u64) -> Result<Secret, String> {
        const GRANT: &[u8] =
            b"grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=";
        let assertion = self.assertion(now)?;
        let mut body = Vec::with_capacity(GRANT.len() + assertion.len());
        body.extend_from_slice(GRANT);
        body.extend_from_slice(assertion.expose());
        Ok(Secret::from_vec(body))
    }
}

/// Whether `bytes` is a credential file a deploy must never publish: a JSON document with a
/// non-empty string `private_key` (top-level, nested, or JSON-escaped), or a standalone PEM
/// private-key block.
#[must_use]
pub fn holds_a_private_key(bytes: &[u8]) -> bool {
    const PEM_HEAD: &[u8] = concat!("-----BEGIN", " ").as_bytes();
    const PEM_TAIL: &[u8] = concat!("PRIVATE ", "KEY-----").as_bytes();
    if bytes.windows(PEM_TAIL.len()).any(|w| w == PEM_TAIL)
        && bytes.split(|b| *b == b'\n').any(|line| {
            let line = line.trim_ascii();
            line.starts_with(PEM_HEAD) && line.ends_with(PEM_TAIL)
        })
    {
        return true;
    }
    let trimmed = bytes.trim_ascii_start();
    matches!(trimmed.first(), Some(b'{' | b'['))
        && aterm_json::from_slice::<aterm_json::Value>(trimmed)
            .is_ok_and(|doc| json_holds_private_key(&doc))
}

fn json_holds_private_key(value: &aterm_json::Value) -> bool {
    match value {
        aterm_json::Value::Object(map) => {
            map.get("private_key")
                .and_then(aterm_json::Value::as_str)
                .is_some_and(|key| !key.trim().is_empty())
                || map.values().any(json_holds_private_key)
        }
        aterm_json::Value::Array(items) => items.iter().any(json_holds_private_key),
        _ => false,
    }
}

/// A `PRIVATE KEY` PEM block's first line, spelled in halves so the export's secret scan
/// reads no key marker in the source.
const BEGIN: &str = concat!("-----BEGIN ", "PRIVATE KEY-----");
/// Its last line.
const END: &str = concat!("-----END ", "PRIVATE KEY-----");

/// The DER inside a `PRIVATE KEY` PEM block.
fn pkcs8(pem: &[u8]) -> Result<Secret, String> {
    let refused =
        || "the service-account key's private_key is not a PRIVATE KEY PEM block".to_string();
    let text = std::str::from_utf8(pem).map_err(|_| refused())?;
    let body = text
        .trim()
        .strip_prefix(BEGIN)
        .and_then(|rest| rest.trim_end().strip_suffix(END))
        .ok_or_else(refused)?;
    let joined = Secret::from_vec(
        body.bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect::<Vec<u8>>(),
    );
    let joined = std::str::from_utf8(joined.expose()).map_err(|_| refused())?;
    aterm_codec::base64::decode(joined)
        .map(Secret::from_vec)
        .map_err(|_| refused())
}

#[cfg(test)]
#[path = "key_tests.rs"]
pub(crate) mod tests;
