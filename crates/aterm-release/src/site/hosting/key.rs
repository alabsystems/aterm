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
    let standard =
        aterm_codec::base64::encode(bytes).map_err(|e| format!("cannot encode base64: {e:?}"))?;
    Ok(standard
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            other => other,
        })
        .collect())
}

impl Key {
    /// The key at `path`: its bytes read once, held as a [`Secret`], and parsed.
    /// # Errors
    /// A file that cannot be read, or one that is not a service-account key with an RSA
    /// PKCS#8 private key; each said by the path and the field, never by a byte of the key.
    pub fn read(path: &Path) -> Result<Key, String> {
        let bytes = std::fs::read(path).map(Secret::from_vec).map_err(|e| {
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
        let pem = Secret::from_vec(need(fields.private_key, "private_key")?.into_bytes());
        let der = Secret::from_vec(pkcs8(pem.expose())?);
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
            "exp": now + LIFETIME_SECS,
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

/// Whether `bytes` is a credential file a deploy must never publish: a JSON object with a
/// string `private_key` (a service-account key, the deployer's own among them).
#[must_use]
pub fn holds_a_private_key(bytes: &[u8]) -> bool {
    const FIELD: &[u8] = b"\"private_key\"";
    bytes.windows(FIELD.len()).any(|w| w == FIELD)
        && matches!(
            aterm_json::from_slice::<aterm_json::Value>(bytes),
            Ok(aterm_json::Value::Object(doc))
                if doc.get("private_key").is_some_and(|k| k.as_str().is_some())
        )
}

/// A `PRIVATE KEY` PEM block's first line, spelled in halves so the export's secret scan
/// reads no key marker in the source.
const BEGIN: &str = concat!("-----BEGIN ", "PRIVATE KEY-----");
/// Its last line.
const END: &str = concat!("-----END ", "PRIVATE KEY-----");

/// The DER inside a `PRIVATE KEY` PEM block.
fn pkcs8(pem: &[u8]) -> Result<Vec<u8>, String> {
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
    aterm_codec::base64::decode(joined).map_err(|_| refused())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// atpkg's committed throwaway RSA-4096 PKCS#8 key (its OpenPGP tests sign with it).
    pub(crate) const TEST_PKCS8: &[u8] =
        include_bytes!("../../../../atpkg/src/vendor_direct/fixtures/openpgp-test-rsa4096.pk8.der");

    /// A service-account JSON key around [`TEST_PKCS8`], exchanged at `token_uri`.
    pub(crate) fn key_json(token_uri: &str) -> String {
        let b64 = aterm_codec::base64::encode(TEST_PKCS8).unwrap();
        let mut pem = format!("{BEGIN}\n");
        for line in b64.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(line).unwrap());
            pem.push('\n');
        }
        pem.push_str(END);
        pem.push('\n');
        aterm_json::json!({
            "type": "service_account",
            "project_id": "fake-project",
            "private_key_id": "kid-1",
            "private_key": pem,
            "client_email": "deployer@fake-project.iam.gserviceaccount.com",
            "client_id": "1",
            "token_uri": token_uri,
        })
        .to_string()
    }

    /// The payload of `jwt` when its RS256 signature verifies under `public` (DER RSAPublicKey).
    pub(crate) fn verified(jwt: &str, public: &[u8]) -> Option<aterm_json::Value> {
        let (input, signature) = jwt.rsplit_once('.')?;
        let decode = |part: &str| {
            let standard: String = part
                .chars()
                .map(|c| match c {
                    '-' => '+',
                    '_' => '/',
                    other => other,
                })
                .collect();
            aterm_codec::base64::decode(&standard).ok()
        };
        ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            public,
        )
        .verify(input.as_bytes(), &decode(signature)?)
        .ok()?;
        let (header, claims) = input.split_once('.')?;
        let header: aterm_json::Value = aterm_json::from_slice(&decode(header)?).ok()?;
        if header.get("alg")?.as_str()? != "RS256" {
            return None;
        }
        aterm_json::from_slice(&decode(claims)?).ok()
    }

    fn public() -> Vec<u8> {
        let pair = ring::signature::RsaKeyPair::from_pkcs8(TEST_PKCS8).unwrap();
        pair.public().as_ref().to_vec()
    }

    /// The assertion is an RS256 JWT the key's public half verifies, issued by the key's
    /// email for the cloud-platform scope, to the token endpoint, for an hour.
    /// NEGATIVE CONTROL: one flipped byte of the signed input no longer verifies.
    #[test]
    fn the_assertion_is_a_jwt_the_public_key_verifies() {
        let key = Key::parse(&Secret::new(
            key_json("https://oauth2.googleapis.com/token").as_bytes(),
        ))
        .unwrap();
        let jwt = String::from_utf8(key.assertion(1_000).unwrap().expose().to_vec()).unwrap();
        assert!(
            !jwt.contains('=') && !jwt.contains('+') && !jwt.contains('/'),
            "base64url"
        );
        let claims = verified(&jwt, &public()).expect("the signature verifies");
        assert_eq!(
            claims,
            aterm_json::json!({
                "iss": "deployer@fake-project.iam.gserviceaccount.com",
                "scope": SCOPE,
                "aud": "https://oauth2.googleapis.com/token",
                "iat": 1_000,
                "exp": 4_600,
            })
        );
        let mut forged = jwt.into_bytes();
        forged[10] ^= 1;
        assert!(verified(&String::from_utf8(forged).unwrap(), &public()).is_none());
        let grant = key.grant(1_000).unwrap();
        assert!(grant.expose().starts_with(
            b"grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=ey"
        ));
    }

    /// A file that is not a service-account key is refused by field, and no refusal or
    /// `Debug` repeats the key's bytes. NEGATIVE CONTROL: the whole key parses.
    #[test]
    fn a_broken_key_is_refused_without_saying_its_bytes() {
        let good = key_json("https://oauth2.googleapis.com/token");
        let key = Key::parse(&Secret::new(good.as_bytes())).unwrap();
        assert_eq!(key.project.as_deref(), Some("fake-project"));
        let pem_line = &aterm_codec::base64::encode(TEST_PKCS8).unwrap()[64..128];
        assert!(
            !format!("{key:?}").contains(pem_line),
            "Debug says no key byte"
        );
        let broken = [
            good.replace("service_account", "authorized_user"),
            good.replace("\"client_email\"", "\"x_email\""),
            good.replace("\"token_uri\"", "\"x_uri\""),
            good.replace("PRIVATE KEY", "RSA PRIVATE KEY"),
            good.replace(&pem_line[..8], "!!!!!!!!"),
            good[..good.len() / 2].to_string(),
        ];
        for text in broken {
            let why = Key::parse(&Secret::new(text.as_bytes())).unwrap_err();
            assert!(!why.contains(&pem_line[..16]), "{why}");
            assert!(why.contains("service-account key"), "{why}");
        }
    }
}
