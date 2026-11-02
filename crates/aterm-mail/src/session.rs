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
    #[cfg(unix)]
    {
        let path = location(home, email).with_extension("lock");
        let parent = path.parent().ok_or("Invalid mail session path")?;
        aterm_secrets::fsx::ensure_private_dir(parent).map_err(|e| e.to_string())?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Invalid mail session path")?;
        let guard = aterm_uds::private_file::PrivateLock::take(
            parent,
            name,
            std::time::Duration::ZERO,
            "This mail account is already connected in another aterm instance",
        )
        .map_err(|e| e.to_string())?;
        Ok(Lease { _guard: guard })
    }
    #[cfg(not(unix))]
    {
        let _ = (home, email);
        Err("Mail session locking is unavailable on this platform".into())
    }
}

#[cfg(any(unix, test))]
fn encode(email: &str, credentials: &Credentials) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut bytes = Zeroizing::new(b"aterm-mail-session-v1\0".to_vec());
    bytes.extend_from_slice(&credentials.expires.to_le_bytes());
    for field in [
        email,
        credentials.uid.expose(),
        credentials.access.expose(),
        credentials.refresh.expose(),
    ] {
        if field.is_empty() || field.len() > 8192 || field.chars().any(char::is_control) {
            return Err("Invalid mail session field".into());
        }
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
    let mut field = || -> Result<Secret, String> {
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
        Ok(Secret::new(value.into()))
    };
    if !field()?.expose().eq_ignore_ascii_case(email) {
        return Err("Mail session belongs to another account".into());
    }
    let credentials = Credentials {
        expires,
        uid: field()?,
        access: field()?,
        refresh: field()?,
    };
    if !rest.is_empty() {
        return Err("Trailing mail session data".into());
    }
    Ok(credentials)
}

#[cfg(unix)]
fn location(home: &Path, email: &str) -> std::path::PathBuf {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(email.trim().to_ascii_lowercase().as_bytes());
    let key: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    home.join(".config/aterm/mail/sessions")
        .join(format!("{key}.age"))
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
mod tests {
    use super::*;
    #[test]
    fn session_is_account_bound_and_rejects_truncation_or_trailing_bytes() {
        let original = Credentials {
            uid: Secret::new("uid".into()),
            access: Secret::new("access".into()),
            refresh: Secret::new("refresh".into()),
            expires: 42,
        };
        let mut bytes = encode("a@example.test", &original).unwrap();
        let decoded = decode("A@example.test", &bytes).unwrap();
        assert_eq!(decoded.refresh.expose(), "refresh");
        assert_eq!(decoded.expires, 42);
        assert!(decode("b@example.test", &bytes).is_err());
        for end in 0..bytes.len() {
            assert!(decode("a@example.test", &bytes[..end]).is_err());
        }
        bytes.push(0);
        assert!(decode("a@example.test", &bytes).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn stored_session_is_encrypted_private_and_integrity_checked() {
        use std::os::unix::fs::PermissionsExt;
        let mut nonce = [0; 8];
        aterm_uds::rand::fill(&mut nonce).unwrap();
        let home = std::env::temp_dir().join(format!(
            "aterm-mail-session-{}-{:x}",
            std::process::id(),
            u64::from_le_bytes(nonce)
        ));
        let credentials = Credentials {
            uid: Secret::new("uid".into()),
            access: Secret::new("sensitive-access-token".into()),
            refresh: Secret::new("sensitive-refresh-token".into()),
            expires: 123,
        };
        save(&home, "a@example.test", &credentials).unwrap();
        let path = location(&home, "a@example.test");
        let mut encrypted = std::fs::read(&path).unwrap();
        assert!(!encrypted.windows(9).any(|bytes| bytes == b"sensitive"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let restored = load(&home, "a@example.test").unwrap().unwrap();
        assert_eq!(restored.refresh.expose(), credentials.refresh.expose());
        let last = encrypted.len() - 1;
        encrypted[last] ^= 1;
        std::fs::write(&path, encrypted).unwrap();
        assert!(load(&home, "a@example.test").is_err());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn only_one_connection_can_rotate_an_accounts_tokens() {
        let mut nonce = [0; 8];
        aterm_uds::rand::fill(&mut nonce).unwrap();
        let home = std::env::temp_dir().join(format!(
            "aterm-mail-lease-{}-{:x}",
            std::process::id(),
            u64::from_le_bytes(nonce)
        ));
        let first = acquire(&home, "a@example.test").unwrap();
        assert!(acquire(&home, "A@example.test").is_err());
        let different = acquire(&home, "b@example.test").unwrap();
        drop(first);
        let reopened = acquire(&home, "a@example.test").unwrap();
        drop((different, reopened));
        std::fs::remove_dir_all(home).unwrap();
    }
}
