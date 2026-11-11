// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Encrypted Proton session credentials, bound to an account and this machine.
use crate::Secret;
use std::path::Path;
#[cfg(any(unix, test))]
use zeroize::Zeroizing;

pub struct Credentials {
    pub uid: Secret,
    pub access: Secret,
    pub refresh: Secret,
    pub expires: u64,
}

#[cfg(unix)]
const LIMIT: usize = 65536;

/// Held for an account's connected lifetime to serialize rotating credentials.
pub struct Lease {
    #[cfg(unix)]
    _guard: aterm_uds::private_file::PrivateLock,
}

pub fn acquire(home: &Path, email: &str) -> Result<Lease, String> {
    acquire_prefixed(home, "", email)
}

pub(crate) fn acquire_prefixed(home: &Path, prefix: &str, email: &str) -> Result<Lease, String> {
    #[cfg(unix)]
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = email_digest_prefixed(prefix, email);
        let mut name = [0u8; 69];
        for (i, &b) in digest.iter().enumerate() {
            name[i * 2] = HEX[usize::from(b >> 4)];
            name[i * 2 + 1] = HEX[usize::from(b & 0x0f)];
        }
        name[64..].copy_from_slice(b".lock");
        let file = std::str::from_utf8(&name).unwrap_or("");
        let parent = home.join(".config/aterm/mail/sessions");
        aterm_secrets::fsx::ensure_private_dir(&parent).map_err(|e| e.to_string())?;
        let guard = aterm_uds::private_file::PrivateLock::take(
            &parent,
            file,
            std::time::Duration::ZERO,
            "This mail account is already connected in another aterm instance",
        )
        .map_err(|e| e.to_string())?;
        Ok(Lease { _guard: guard })
    }
    #[cfg(not(unix))]
    {
        let _ = (home, prefix, email);
        Err("Mail session locking is unavailable on this platform".into())
    }
}

#[cfg(any(unix, test))]
fn encode(email: &str, credentials: &Credentials) -> Result<Zeroizing<Vec<u8>>, String> {
    let fields = [
        email,
        credentials.uid.expose(),
        credentials.access.expose(),
        credentials.refresh.expose(),
    ];
    let mut cap = 22 + 8 + 4 * 4;
    for field in fields {
        if field.is_empty() || field.len() > 8192 || field.chars().any(char::is_control) {
            return Err("Invalid mail session field".into());
        }
        cap += field.len();
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(cap));
    bytes.extend_from_slice(b"aterm-mail-session-v1\0");
    bytes.extend_from_slice(&credentials.expires.to_le_bytes());
    for field in fields {
        bytes.extend_from_slice(&(field.len() as u32).to_le_bytes());
        bytes.extend_from_slice(field.as_bytes());
    }
    Ok(bytes)
}

#[cfg(any(unix, test))]
fn decode(email: &str, bytes: &[u8]) -> Result<Credentials, String> {
    let mut rest = bytes
        .strip_prefix(b"aterm-mail-session-v1\0")
        .ok_or("Unknown mail session format")?;
    let expires = u64::from_le_bytes(
        rest.get(..8)
            .ok_or("Truncated mail session")?
            .try_into()
            .map_err(|_| "Invalid mail session")?,
    );
    rest = &rest[8..];
    let mut field = || -> Result<&str, String> {
        let len = u32::from_le_bytes(
            rest.get(..4)
                .ok_or("Truncated mail session")?
                .try_into()
                .map_err(|_| "Invalid mail session")?,
        ) as usize;
        if len == 0 || len > 8192 {
            return Err("Invalid mail session field".into());
        }
        rest = &rest[4..];
        let value = std::str::from_utf8(rest.get(..len).ok_or("Truncated mail session")?)
            .map_err(|_| "Invalid mail session field")?;
        if value.chars().any(char::is_control) {
            return Err("Invalid mail session field".into());
        }
        rest = &rest[len..];
        Ok(value)
    };
    if !field()?.eq_ignore_ascii_case(email) {
        return Err("Mail session belongs to another account".into());
    }
    let credentials = Credentials {
        expires,
        uid: Secret::new(field()?.into()),
        access: Secret::new(field()?.into()),
        refresh: Secret::new(field()?.into()),
    };
    if !rest.is_empty() {
        return Err("Trailing mail session data".into());
    }
    Ok(credentials)
}

#[cfg(unix)]
fn email_digest(email: &str) -> [u8; 32] {
    email_digest_prefixed("", email)
}

#[cfg(unix)]
fn email_digest_prefixed(prefix: &str, email: &str) -> [u8; 32] {
    let trimmed = email.trim().as_bytes();
    if prefix.is_empty() && !trimmed.iter().any(u8::is_ascii_uppercase) {
        return aterm_digest::Sha256::digest(trimmed);
    }
    let mut hasher = aterm_digest::Sha256::new();
    for slice in [prefix.as_bytes(), trimmed] {
        if !slice.iter().any(u8::is_ascii_uppercase) {
            hasher.update(slice);
            continue;
        }
        for chunk in slice.chunks(64) {
            let mut buf = [0u8; 64];
            for (dst, &src) in buf.iter_mut().zip(chunk) {
                *dst = src.to_ascii_lowercase();
            }
            hasher.update(&buf[..chunk.len()]);
        }
    }
    hasher.finalize()
}

#[cfg(unix)]
fn location(home: &Path, email: &str) -> std::path::PathBuf {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = email_digest(email);
    let mut name = [0u8; 68];
    for (i, &b) in digest.iter().enumerate() {
        name[i * 2] = HEX[usize::from(b >> 4)];
        name[i * 2 + 1] = HEX[usize::from(b & 0x0f)];
    }
    name[64..].copy_from_slice(b".age");
    let file = std::str::from_utf8(&name).unwrap_or("");
    home.join(".config/aterm/mail/sessions").join(file)
}

#[cfg(unix)]
pub fn load(home: &Path, email: &str) -> Result<Option<Credentials>, String> {
    let Some(bytes) = aterm_secrets::fsx::read_private(&location(home, email), LIMIT)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let paths = aterm_secrets::client::Paths::for_home(home);
    let identity = aterm_secrets::client::read_machine_identity(&paths)
        .map_err(|e| e.to_string())?
        .ok_or("Mail session encryption key is unavailable")?;
    let plaintext = aterm_secrets::age::decrypt(&[identity], bytes.as_slice())
        .map_err(|_| "Mail session could not be decrypted")?;
    decode(email, plaintext.as_slice()).map(Some)
}

#[cfg(unix)]
pub fn save(home: &Path, email: &str, credentials: &Credentials) -> Result<(), String> {
    let plaintext = encode(email, credentials)?;
    let paths = aterm_secrets::client::Paths::for_home(home);
    let identity = aterm_secrets::client::machine_identity(&paths).map_err(|e| e.to_string())?;
    let encrypted = aterm_secrets::age::encrypt(&[identity.recipient()], &plaintext)
        .map_err(|_| "Could not encrypt mail session")?;
    let path = location(home, email);
    aterm_secrets::fsx::ensure_private_dir(path.parent().ok_or("Invalid mail session path")?)
        .map_err(|e| e.to_string())?;
    aterm_secrets::fsx::write_atomic(&path, &encrypted, 0o600).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
pub fn load(_: &Path, _: &str) -> Result<Option<Credentials>, String> {
    Ok(None)
}
#[cfg(not(unix))]
pub fn save(_: &Path, _: &str, _: &Credentials) -> Result<(), String> {
    Err("Encrypted mail session storage is unavailable on this platform".into())
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
