// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Machine-encrypted Google mail grants and serialized unattended token renewal.

use super::{Error, ErrorKind, Tokens};
use crate::{Authentication, Secret};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
#[cfg(any(unix, test))]
use zeroize::Zeroizing;

struct Grant {
    email: String,
    client_id: String,
    client_secret: Option<Secret>,
    refresh: Secret,
}

pub struct Session {
    home: PathBuf,
    grant: Grant,
    access: Option<Secret>,
    expires: u64,
    pending: Option<(Tokens, u64)>,
    dirty: bool,
    generation: u64,
    rejected: Option<Error>,
    _lease: Option<crate::session::Lease>,
}

fn email(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    let (local, domain) = value.split_once('@').ok_or("Invalid Google mail account")?;
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || value.len() > 320
        || value.chars().any(char::is_whitespace)
        || value.chars().any(char::is_control)
    {
        return Err("Invalid Google mail account".into());
    }
    Ok(value)
}

fn location(home: &Path, email: &str) -> PathBuf {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = aterm_digest::Sha256::digest(email.as_bytes());
    let mut name = [0u8; 68];
    for (i, &b) in digest.iter().enumerate() {
        name[i * 2] = HEX[usize::from(b >> 4)];
        name[i * 2 + 1] = HEX[usize::from(b & 0x0f)];
    }
    name[64..].copy_from_slice(b".age");
    let file = std::str::from_utf8(&name).unwrap_or("");
    home.join(".config/aterm/mail/google").join(file)
}

#[cfg(any(unix, test))]
fn encode(grant: &Grant) -> Result<Zeroizing<Vec<u8>>, String> {
    let fields = [
        grant.email.as_str(),
        grant.client_id.as_str(),
        grant.client_secret.as_ref().map_or("", Secret::expose),
        grant.refresh.expose(),
    ];
    let mut cap = 21 + 4 * 4;
    for (index, field) in fields.into_iter().enumerate() {
        if (field.is_empty() && index != 2)
            || field.len() > 8192
            || field.chars().any(char::is_control)
        {
            return Err("Invalid Google mail credential".into());
        }
        cap += field.len();
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(cap));
    bytes.extend_from_slice(b"aterm-google-mail-v1\0");
    for field in fields {
        bytes.extend_from_slice(&(field.len() as u32).to_le_bytes());
        bytes.extend_from_slice(field.as_bytes());
    }
    Ok(bytes)
}

#[cfg(any(unix, test))]
fn decode(bytes: &[u8]) -> Result<Grant, String> {
    let mut rest = bytes
        .strip_prefix(b"aterm-google-mail-v1\0")
        .ok_or("Unknown Google mail credential format")?;
    let mut field = |empty: bool| -> Result<&str, String> {
        let length = u32::from_le_bytes(
            rest.get(..4)
                .ok_or("Truncated Google mail credential")?
                .try_into()
                .map_err(|_| "Invalid Google mail credential")?,
        ) as usize;
        rest = &rest[4..];
        if (length == 0 && !empty) || length > 8192 {
            return Err("Invalid Google mail credential".into());
        }
        let text = std::str::from_utf8(
            rest.get(..length)
                .ok_or("Truncated Google mail credential")?,
        )
        .map_err(|_| "Invalid Google mail credential")?;
        if text.chars().any(char::is_control) {
            return Err("Invalid Google mail credential".into());
        }
        rest = &rest[length..];
        Ok(text)
    };
    let account = email(field(false)?)?;
    let client_id = field(false)?.to_owned();
    let client_secret_raw = field(true)?;
    let refresh = Secret::new(field(false)?.into());
    if !rest.is_empty() {
        return Err("Trailing Google mail credential data".into());
    }
    Ok(Grant {
        email: account,
        client_id,
        client_secret: (!client_secret_raw.is_empty())
            .then(|| Secret::new(client_secret_raw.into())),
        refresh,
    })
}

#[cfg(unix)]
fn save(home: &Path, grant: &Grant) -> Result<(), String> {
    let plaintext = encode(grant)?;
    let paths = aterm_secrets::client::Paths::for_home(home);
    let identity =
        aterm_secrets::client::machine_identity(&paths).map_err(|error| error.to_string())?;
    let encrypted = aterm_secrets::age::encrypt(&[identity.recipient()], &plaintext)
        .map_err(|_| "Could not encrypt Google mail credentials")?;
    let path = location(home, &grant.email);
    aterm_secrets::fsx::ensure_private_dir(
        path.parent().ok_or("Invalid Google mail credential path")?,
    )
    .map_err(|error| error.to_string())?;
    aterm_secrets::fsx::write_atomic(&path, &encrypted, 0o600).map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn save(_: &Path, _: &Grant) -> Result<(), String> {
    Err("Encrypted Google mail storage is unavailable on this platform".into())
}

#[cfg(unix)]
fn decrypt_grant(
    home: &Path,
    path: &Path,
    encrypted: &[u8],
    identity: &aterm_secrets::age::Identity,
) -> Result<Grant, String> {
    let plaintext = aterm_secrets::age::decrypt(std::slice::from_ref(identity), encrypted)
        .map_err(|_| "Google mail credentials could not be decrypted")?;
    let grant = decode(plaintext.as_slice())?;
    if location(home, &grant.email) != path {
        return Err("Google mail credentials belong to another account".into());
    }
    Ok(grant)
}

#[cfg(unix)]
fn load_grant(home: &Path, path: &Path) -> Result<Option<Grant>, String> {
    let Some(encrypted) =
        aterm_secrets::fsx::read_private(path, 65536).map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let paths = aterm_secrets::client::Paths::for_home(home);
    let identity = aterm_secrets::client::read_machine_identity(&paths)
        .map_err(|error| error.to_string())?
        .ok_or("Google mail encryption key is unavailable")?;
    decrypt_grant(home, path, encrypted.as_slice(), &identity).map(Some)
}

#[cfg(not(unix))]
fn load_grant(_: &Path, _: &Path) -> Result<Option<Grant>, String> {
    Ok(None)
}

/// Store an already authorized grant. No browser session or other app's OAuth identity is used.
pub fn register(
    home: &Path,
    account: &str,
    client_id: &str,
    client_secret: Option<Secret>,
    refresh: Secret,
) -> Result<(), String> {
    let account = email(account)?;
    let _lease = crate::session::acquire(home, &format!("google:{account}"))?;
    save(
        home,
        &Grant {
            email: account,
            client_id: client_id.into(),
            client_secret,
            refresh,
        },
    )
}

/// Discover locally stored, authenticated-encryption-checked account identities.
pub fn accounts(home: &Path) -> Vec<String> {
    let directory = home.join(".config/aterm/mail/google");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let age_entries: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .as_os_str()
                .as_encoded_bytes()
                .ends_with(b".age")
                && entry.file_type().is_ok_and(|ft| ft.is_file())
        })
        .take(128)
        .collect();
    if age_entries.is_empty() {
        return Vec::new();
    }
    #[cfg(unix)]
    let Some(identity) =
        aterm_secrets::client::read_machine_identity(&aterm_secrets::client::Paths::for_home(home))
            .ok()
            .flatten()
    else {
        return Vec::new();
    };
    let mut accounts: Vec<_> = age_entries
        .into_iter()
        .filter_map(|entry| {
            let path = entry.path();
            #[cfg(unix)]
            let loaded = aterm_secrets::fsx::read_private(&path, 65536)
                .ok()
                .flatten()
                .and_then(|enc| decrypt_grant(home, &path, enc.as_slice(), &identity).ok());
            #[cfg(not(unix))]
            let loaded = load_grant(home, &path).ok().flatten();
            loaded.map(|grant| grant.email)
        })
        .collect();
    accounts.sort();
    accounts.dedup();
    accounts
}

pub type Shared = std::sync::Arc<std::sync::Mutex<Session>>;

/// Reuse one renewing session inside this process.
pub fn shared(home: &Path, account: &str) -> Result<Option<Shared>, String> {
    type Sessions = std::collections::BTreeMap<PathBuf, std::sync::Weak<std::sync::Mutex<Session>>>;
    static SESSIONS: std::sync::OnceLock<std::sync::Mutex<Sessions>> = std::sync::OnceLock::new();
    let account = email(account)?;
    let key = location(home, &account);
    let registry = SESSIONS.get_or_init(Default::default);
    let mut sessions = registry
        .lock()
        .map_err(|_| "Google mail session registry lock failed")?;
    sessions.retain(|_, session| session.strong_count() != 0);
    if let Some(session) = sessions.get(&key).and_then(std::sync::Weak::upgrade) {
        return Ok(Some(session));
    }
    let Some(session) = Session::load(home, &account)? else {
        return Ok(None);
    };
    let session = std::sync::Arc::new(std::sync::Mutex::new(session));
    sessions.insert(key, std::sync::Arc::downgrade(&session));
    Ok(Some(session))
}

impl Session {
    /// Load a grant without taking the renewal lease or making a network request.
    pub fn load(home: &Path, account: &str) -> Result<Option<Self>, String> {
        let account = email(account)?;
        let path = location(home, &account);
        let Some(grant) = load_grant(home, &path)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            home: home.into(),
            grant,
            access: None,
            expires: 0,
            pending: None,
            dirty: false,
            generation: 0,
            rejected: None,
            _lease: None,
        }))
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn email(&self) -> &str {
        &self.grant.email
    }

    pub fn authentication(&self) -> Option<Authentication> {
        self.access
            .as_ref()
            .map(|access_token| Authentication::OAuth {
                username: self.grant.email.clone(),
                access_token: access_token.clone(),
            })
    }

    /// Renew once when needed, persisting a rotated grant before using its access token.
    pub fn renew(&mut self) -> Result<bool, Error> {
        self.renew_until(Instant::now() + Duration::from_secs(30))
    }

    pub fn renew_until(&mut self, end: Instant) -> Result<bool, Error> {
        loop {
            if Instant::now() >= end {
                return Err(Error {
                    kind: ErrorKind::Transport,
                });
            }
            let result = self.renew_with(|grant| {
                super::refresh_until(
                    &grant.client_id,
                    grant.client_secret.as_ref().map(Secret::expose),
                    &grant.refresh,
                    end,
                )
            });
            if matches!(&result, Err(error) if error.kind == ErrorKind::Storage) {
                let remaining = end.saturating_duration_since(Instant::now());
                if !remaining.is_zero() {
                    std::thread::sleep(remaining.min(Duration::from_millis(250)));
                }
                continue;
            }
            return result;
        }
    }

    fn renew_with(
        &mut self,
        request: impl FnMut(&Grant) -> Result<Tokens, Error>,
    ) -> Result<bool, Error> {
        self.renew_with_clock(aterm_time::unix_secs, request)
    }

    fn renew_with_clock(
        &mut self,
        now_unix: impl Fn() -> Option<u64>,
        mut request: impl FnMut(&Grant) -> Result<Tokens, Error>,
    ) -> Result<bool, Error> {
        let mut grant_path = None;
        if let Some(error) = &self.rejected {
            // A new explicit enrollment may replace this grant from another process.
            // A rejection suppresses retries only while the persisted credential is unchanged.
            let path = location(&self.home, &self.grant.email);
            let fresh = load_grant(&self.home, &path)
                .map_err(|_| Error {
                    kind: ErrorKind::Storage,
                })?
                .ok_or(Error {
                    kind: ErrorKind::Storage,
                })?;
            if fresh.client_id == self.grant.client_id
                && fresh.client_secret.as_ref().map(Secret::expose)
                    == self.grant.client_secret.as_ref().map(Secret::expose)
                && fresh.refresh.expose() == self.grant.refresh.expose()
            {
                return Err(error.clone());
            }
            grant_path = Some(path);
            self.grant = fresh;
            self.rejected = None;
            self.access = None;
            self.expires = 0;
        }
        let mut changed = self.commit_pending()?;
        let now = now_unix().ok_or(Error {
            kind: ErrorKind::Storage,
        })?;
        if self.access.is_some() && now < self.expires {
            return Ok(changed);
        }
        let grant_path = grant_path.unwrap_or_else(|| location(&self.home, &self.grant.email));
        let lease = crate::session::acquire(&self.home, &format!("google:{}", self.grant.email))
            .map_err(|_| Error {
                kind: ErrorKind::Storage,
            })?;
        // Another process may have renewed since this session loaded its grant.
        let grant = load_grant(&self.home, &grant_path)
            .map_err(|_| Error {
                kind: ErrorKind::Storage,
            })?
            .ok_or(Error {
                kind: ErrorKind::Storage,
            })?;
        self.grant = grant;
        self._lease = Some(lease);
        let tokens = request(&self.grant).inspect_err(|error| {
            self._lease = None;
            if !error.retryable() {
                self.rejected = Some(error.clone());
            }
        })?;
        if tokens.expires_in == 0 || tokens.expires_in > 86_400 {
            self._lease = None;
            return Err(Error {
                kind: ErrorKind::InvalidResponse,
            });
        }
        let within = tokens
            .expires_in
            .saturating_sub((tokens.expires_in / 10).min(60));
        // Request time may shorten cache use; a backward clock step must shorten it too.
        // An unavailable response clock cannot discard the rotated refresh token.
        let base = now_unix().map_or(now, |observed| observed.min(now));
        let expires = base.saturating_add(within);
        if let Some(refresh) = &tokens.refresh_token {
            self.grant.refresh = refresh.clone();
            self.dirty = true;
        }
        self.pending = Some((tokens, expires));
        changed |= self.commit_pending()?;
        Ok(changed)
    }

    fn commit_pending(&mut self) -> Result<bool, Error> {
        if self.dirty {
            save(&self.home, &self.grant).map_err(|_| Error {
                kind: ErrorKind::Storage,
            })?;
            self.dirty = false;
        }
        if let Some((tokens, expires)) = self.pending.take() {
            self.access = Some(tokens.access_token);
            self.expires = expires;
            self.generation = self.generation.wrapping_add(1);
            self._lease = None;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> Grant {
        Grant {
            email: "me@example.test".into(),
            client_id: "client".into(),
            client_secret: None,
            refresh: Secret::new("sensitive-refresh".into()),
        }
    }

    #[test]
    fn codec_rejects_truncation_trailing_data_and_bad_accounts() {
        let bytes = encode(&grant()).unwrap();
        assert_eq!(
            decode(&bytes).unwrap().refresh.expose(),
            "sensitive-refresh"
        );
        for end in 0..bytes.len() {
            assert!(decode(&bytes[..end]).is_err());
        }
        let mut extra = bytes.to_vec();
        extra.push(0);
        assert!(decode(&extra).is_err());
        assert!(email("a@b@c").is_err());
        assert_eq!(email(" Me@Example.Test ").unwrap(), "me@example.test");
    }

    #[cfg(unix)]
    struct Home(PathBuf);
    #[cfg(unix)]
    impl Home {
        fn new() -> Self {
            let mut nonce = [0; 8];
            aterm_uds::rand::fill(&mut nonce).unwrap();
            let home = std::env::temp_dir().join(format!(
                "google-mail-{}-{:x}.noindex",
                std::process::id(),
                u64::from_le_bytes(nonce)
            ));
            Self(home)
        }
        fn session(&self) -> Session {
            save(&self.0, &grant()).unwrap();
            Session::load(&self.0, "me@example.test").unwrap().unwrap()
        }
    }
    #[cfg(unix)]
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn tokens() -> Tokens {
        Tokens {
            access_token: Secret::new("access".into()),
            refresh_token: Some(Secret::new("rotated".into())),
            expires_in: 3600,
        }
    }

    #[cfg(unix)]
    #[test]
    fn renewal_caches_access_and_persists_rotation_privately() {
        use std::os::unix::fs::PermissionsExt;
        let home = Home::new();
        let mut session = home.session();
        assert!(session.renew_with(|_| Ok(tokens())).unwrap());
        assert!(
            !session
                .renew_with(|_| panic!("valid token must be reused"))
                .unwrap()
        );
        assert_eq!(session.generation(), 1);
        let path = location(&home.0, "me@example.test");
        assert_eq!(
            load_grant(&home.0, &path)
                .unwrap()
                .unwrap()
                .refresh
                .expose(),
            "rotated"
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(7).any(|part| part == b"rotated"));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(accounts(&home.0), vec!["me@example.test"]);
    }

    #[cfg(unix)]
    #[test]
    fn rejected_grants_stop_retries_but_transport_errors_do_not() {
        let home = Home::new();
        let mut session = home.session();
        assert!(
            session
                .renew_with(|_| Err(Error {
                    kind: ErrorKind::Transport
                }))
                .is_err()
        );
        assert!(
            session
                .renew_with(|_| Err(Error {
                    kind: ErrorKind::Rejected
                }))
                .is_err()
        );
        assert!(
            session
                .renew_with(|_| panic!("rejected grant must not retry"))
                .is_err()
        );
        assert!(session.authentication().is_none());

        // Re-registering a fresh refresh token on disk lifts the rejection for a live session.
        register(
            &home.0,
            "me@example.test",
            "client",
            None,
            Secret::new("fresh-refresh".into()),
        )
        .unwrap();
        assert!(
            session
                .renew_with(|latest| {
                    assert_eq!(latest.refresh.expose(), "fresh-refresh");
                    Ok(tokens())
                })
                .unwrap()
        );
        assert!(session.authentication().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn failed_persistence_retries_without_rotating_again() {
        let home = Home::new();
        let mut session = home.session();
        let directory = home.0.join(".config/aterm/mail/google");
        let saved = directory.with_extension("saved");
        assert_eq!(
            session
                .renew_with(|_| {
                    std::fs::rename(&directory, &saved).unwrap();
                    std::fs::write(&directory, b"blocked").unwrap();
                    Ok(tokens())
                })
                .unwrap_err()
                .kind,
            ErrorKind::Storage
        );
        assert!(session.authentication().is_none());
        std::fs::remove_file(&directory).unwrap();
        std::fs::rename(saved, directory).unwrap();
        assert!(
            session
                .renew_with(|_| panic!("pending rotation must be saved first"))
                .unwrap()
        );
        assert_eq!(session.generation(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn shared_session_and_independent_readers_only_lock_during_renewal() {
        let home = Home::new();
        save(&home.0, &grant()).unwrap();
        let first = shared(&home.0, "me@example.test").unwrap().unwrap();
        let second = shared(&home.0, "ME@example.test").unwrap().unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        let mut independent = Session::load(&home.0, "me@example.test").unwrap().unwrap();
        let held = crate::session::acquire(&home.0, "google:me@example.test").unwrap();
        assert_eq!(
            independent
                .renew_with(|_| panic!("lease is held"))
                .unwrap_err()
                .kind,
            ErrorKind::Storage
        );
        let started = Instant::now();
        assert_eq!(
            independent
                .renew_until(started + Duration::from_millis(5))
                .unwrap_err()
                .kind,
            ErrorKind::Transport
        );
        drop(held);
        first.lock().unwrap().renew_with(|_| Ok(tokens())).unwrap();
        independent
            .renew_with(|latest| {
                assert_eq!(latest.refresh.expose(), "rotated");
                Ok(tokens())
            })
            .unwrap();
        let released = crate::session::acquire(&home.0, "google:me@example.test").unwrap();
        drop(released);
        let path = location(&home.0, "me@example.test");
        let wrong = location(&home.0, "other@example.test");
        std::fs::copy(&path, &wrong).unwrap();
        assert!(load_grant(&home.0, &wrong).is_err());
        drop((first, second));
        assert!(Session::load(&home.0, "me@example.test").unwrap().is_some());
    }
    #[cfg(unix)]
    #[test]
    fn renewal_refreshes_when_wall_clock_advances_across_sleep() {
        let home = Home::new();
        let mut session = home.session();
        let now = std::cell::Cell::new(1_800_000_000_u64);
        assert!(
            session
                .renew_with_clock(|| Some(now.get()), |_| Ok(tokens()))
                .unwrap()
        );
        assert_eq!(session.generation(), 1);

        // Before `3600 - 60 = 3540` wall-clock seconds, cached token is reused.
        now.set(1_800_003_539);
        assert!(
            !session
                .renew_with_clock(|| Some(now.get()), |_| panic!("must reuse before margin"))
                .unwrap()
        );
        assert_eq!(session.generation(), 1);

        // Once wall-clock time crosses the margin (for example across macOS sleep while Instant
        // barely moved), `renew_with_clock` refreshes the access token and bumps `generation`.
        now.set(1_800_003_540);
        assert!(
            session
                .renew_with_clock(
                    || Some(now.get()),
                    |_| Ok(Tokens {
                        access_token: Secret::new("access-2".into()),
                        refresh_token: None,
                        expires_in: 3600,
                    })
                )
                .unwrap()
        );
        assert_eq!(session.generation(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn renewal_keeps_rotation_across_missing_or_shifted_response_clocks() {
        for timestamp in [1_800_000_000, u64::MAX - 10] {
            for response_clock in [
                None,
                Some(timestamp - 100),
                Some(timestamp.saturating_add(100)),
            ] {
                let home = Home::new();
                let mut session = home.session();
                let clock = std::cell::Cell::new(Some(timestamp));
                assert!(
                    session
                        .renew_with_clock(
                            || clock.get(),
                            |_| {
                                clock.set(response_clock);
                                Ok(tokens())
                            },
                        )
                        .unwrap()
                );
                assert_eq!(session.generation(), 1);
                let base = response_clock.map_or(timestamp, |observed| observed.min(timestamp));
                assert_eq!(session.expires, base.saturating_add(3540));
                assert_eq!(
                    load_grant(&home.0, &location(&home.0, "me@example.test"))
                        .unwrap()
                        .unwrap()
                        .refresh
                        .expose(),
                    "rotated"
                );
                let lease = crate::session::acquire(&home.0, "google:me@example.test").unwrap();
                drop(lease);
                if response_clock.is_none() {
                    assert_eq!(
                        session
                            .renew_with_clock(
                                || clock.get(),
                                |_| panic!("no request without a clock")
                            )
                            .unwrap_err()
                            .kind,
                        ErrorKind::Storage
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn explicit_reenrollment_releases_a_rejected_session_without_retrying_old_credentials() {
        let home = Home::new();
        let mut session = home.session();
        assert!(
            session
                .renew_with(|_| Err(Error {
                    kind: ErrorKind::Rejected
                }))
                .is_err()
        );
        assert!(
            session
                .renew_with(|_| panic!("unchanged rejected grant must not retry"))
                .is_err()
        );
        register(
            &home.0,
            "me@example.test",
            "new-client",
            None,
            Secret::new("new-refresh".into()),
        )
        .unwrap();
        assert!(
            session
                .renew_with(|grant| {
                    assert_eq!(grant.client_id, "new-client");
                    assert_eq!(grant.refresh.expose(), "new-refresh");
                    Ok(tokens())
                })
                .unwrap()
        );
    }
}
