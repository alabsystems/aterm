// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native Proton API operations and encryption, with an optional authenticated transport.

use crate::{Folder, Mailbox, Message, Secret};
use aterm_http::{Client, Method, ProxyMode, Trust};
use aterm_json::{Value, json};
use std::time::{Duration, Instant};

const API: &str = "https://mail.proton.me/api";
/// Hang detector for a required native response, within the original caller's deadline.
const REQUEST_HANG: Duration = Duration::from_secs(60);

/// An authenticated HTTP channel whose session is managed by its owner.
/// Implementations must never replay a submitted write after an uncertain result.
pub trait ApiTransport: Send {
    fn request(
        &mut self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<ApiResponse, ApiError>;
    fn renew(&mut self) -> Result<(), ApiError>;
    /// Apply an outer deadline to all transport operations, including session renewal.
    fn set_deadline(&mut self, _end: Option<Instant>) {}
}

/// A transport failure classified independently from its diagnostic text.
#[derive(Debug)]
pub struct ApiError {
    pub kind: crate::proton_error::Kind,
    pub message: String,
}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        Self {
            kind: crate::proton_error::Kind::Network,
            message,
        }
    }
}
impl From<&str> for ApiError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

pub struct ApiResponse {
    pub status: u16,
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

struct MessageCache {
    entries: std::collections::VecDeque<Message>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for MessageCache {
    fn default() -> Self {
        Self {
            entries: Default::default(),
            bytes: 0,
            max_entries: 32,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

impl MessageCache {
    fn weight(message: &Message) -> usize {
        message.id.len()
            + message.from.len()
            + message.to.len()
            + message.subject.len()
            + message.date.len()
            + message.text.len()
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    fn get(&mut self, id: &str) -> Option<Message> {
        let index = self.entries.iter().position(|entry| entry.id == id)?;
        if index > 0 {
            let message = self.entries.remove(index)?;
            self.entries.push_front(message);
        }
        self.entries.front().cloned()
    }

    fn insert(&mut self, message: Message) {
        let weight = Self::weight(&message);
        if weight > self.max_bytes || self.max_entries == 0 {
            return;
        }
        if let Some(index) = self.entries.iter().position(|entry| entry.id == message.id)
            && let Some(previous) = self.entries.remove(index)
        {
            self.bytes -= Self::weight(&previous);
        }
        while self.entries.len() >= self.max_entries || self.bytes + weight > self.max_bytes {
            if let Some(old) = self.entries.pop_back() {
                self.bytes -= Self::weight(&old);
            } else {
                break;
            }
        }
        self.bytes += weight;
        self.entries.push_front(message);
    }
}

/// What Proton says one of an account's addresses is (`Type` of `/core/v4/addresses`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressKind {
    /// 1: the account's own address, made with the account.
    Original,
    /// 2: an address added to the account.
    Alias,
    /// 3: an address at the owner's own domain.
    Custom,
    /// 4: the short-domain twin of the original (`pm.me`).
    Premium,
    /// 5: an outside address the account signs in with, which Proton does not receive at.
    External,
    /// A type this build does not know.
    Other,
}

impl AddressKind {
    fn of(code: Option<u64>) -> Self {
        match code {
            Some(1) => Self::Original,
            Some(2) => Self::Alias,
            Some(3) => Self::Custom,
            Some(4) => Self::Premium,
            Some(5) => Self::External,
            _ => Self::Other,
        }
    }

    /// Whether the address is the account's own login rather than an address it receives for
    /// others: the original, its short-domain twin, an outside sign-in address, or an unknown type
    /// (which fails closed so an unrecognised or missing `Type` never enters Commons).
    pub const fn own(self) -> bool {
        !matches!(self, Self::Alias | Self::Custom)
    }
}

/// One address of the account.
#[derive(Clone, PartialEq, Eq)]
pub struct Listed {
    pub email: String,
    pub kind: AddressKind,
    /// Enabled and receiving: mail sent to it lands in this mailbox.
    pub receives: bool,
}

/// An account's address list. No `Debug` shows an address.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AddressList {
    pub addresses: Vec<Listed>,
    /// Every entry Proton returned was read; an entry with no address makes the list partial.
    pub complete: bool,
}

impl std::fmt::Debug for AddressList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddressList")
            .field("addresses", &self.addresses.len())
            .field("complete", &self.complete)
            .finish()
    }
}

impl AddressList {
    /// Read `/core/v4/addresses`. An address receives when its `Status` is 1 and its `Receive` is
    /// not 0 (an absent `Receive` reads as receiving, as Proton's own clients do).
    fn read(response: &Value) -> Self {
        let Some(entries) = response["Addresses"].as_array() else {
            return Self::default();
        };
        let mut list = Self {
            addresses: Vec::with_capacity(entries.len()),
            complete: true,
        };
        for entry in entries {
            let Some(email) = entry["Email"].as_str().filter(|e| !e.trim().is_empty()) else {
                list.complete = false;
                continue;
            };
            let kind = AddressKind::of(entry["Type"].as_u64());
            if kind == AddressKind::Other {
                list.complete = false;
            }
            list.addresses.push(Listed {
                email: email.to_owned(),
                kind,
                receives: entry["Status"].as_u64() == Some(1)
                    && entry["Receive"].as_u64().unwrap_or(1) != 0,
            });
        }
        list
    }
}

#[path = "proton_send.rs"]
mod send;
pub use send::PreparedMessage;

pub struct Proton {
    client: Client,
    transport: Option<std::cell::RefCell<Box<dyn ApiTransport>>>,
    cache: MessageCache,
    request_deadline: Option<Instant>,
    uid: Secret,
    access: Secret,
    refresh_token: Secret,
    expires: u64,
    stored: Option<(std::path::PathBuf, String)>,
    credentials_dirty: bool,
    _lease: Option<crate::session::Lease>,
    last_failure: std::cell::RefCell<Option<crate::proton_error::Failure>>,
    paused: std::cell::RefCell<Option<crate::proton_error::Failure>>,
    retry_at: std::cell::Cell<Option<std::time::Instant>>,
    pub needs_two_factor: bool,
    keys: Vec<crate::crypto::pgp::Key>,
    send_keys: Vec<(String, usize)>,
    /// The address list the last unlock read, kept for discovery ([`Proton::address_list`]).
    listed: Option<AddressList>,
    verification: Option<crate::human::Verified>,
    challenge: std::cell::RefCell<Option<crate::human::Challenge>>,
}

impl Proton {
    /// Observe header changes without evicting verified bodies on an unchanged poll.
    pub fn observe_messages(
        &mut self,
        folder: &str,
        limit: usize,
        previous: &[Message],
    ) -> Result<Vec<Message>, String> {
        let messages = self.message_headers(folder, limit)?;
        if messages != previous {
            self.cache.clear();
        }
        Ok(messages)
    }

    fn message_headers(&self, folder: &str, limit: usize) -> Result<Vec<Message>, String> {
        let mut path = String::with_capacity(64 + folder.len());
        path.push_str("/mail/v4/messages?LabelID=");
        crate::oauth::encode_into(&mut path, folder);
        use std::fmt::Write as _;
        let _ = write!(path, "&PageSize={}&Sort=Time&Desc=1", limit.min(100));
        let value = self.call(Method::Get, &path, None)?;
        value["Messages"]
            .as_array()
            .ok_or_else(|| "Proton returned no message list".to_string())?
            .iter()
            .map(message)
            .collect()
    }

    /// Run an operation under one deadline, restoring the shared client afterwards.
    pub fn run_until<T>(
        &mut self,
        end: Instant,
        operation: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let previous = self.request_deadline;
        let end = previous.map_or(end, |old| old.min(end));
        self.request_deadline = Some(end);
        if let Some(transport) = &self.transport {
            transport.borrow_mut().set_deadline(Some(end));
        }
        let result = operation(self);
        self.request_deadline = previous;
        if let Some(transport) = &self.transport {
            transport.borrow_mut().set_deadline(previous);
        }
        result
    }

    /// Use an authenticated channel; callers must still check mailbox identity
    /// and unlock its encryption keys before reading or sending.
    pub fn with_transport(transport: Box<dyn ApiTransport>) -> Self {
        let mut session = Self::empty();
        session.transport = Some(std::cell::RefCell::new(transport));
        session
    }

    pub fn sign_in(username: &str, password: &Secret) -> Result<Self, String> {
        Self::sign_in_with_verifier(username, password, |_| {
            Err("Proton requires human verification".into())
        })
    }

    pub fn sign_in_with_verifier(
        username: &str,
        password: &Secret,
        verifier: impl FnOnce(&crate::human::Challenge) -> Result<crate::human::Verified, String>,
    ) -> Result<Self, String> {
        let mut session = Self::empty();
        session.with_verification(|session| session.authenticate(username, password), verifier)?;
        Ok(session)
    }

    fn empty() -> Self {
        Self {
            client: Client::new(
                Trust::PlatformVerifier,
                ProxyMode::Environment,
                REQUEST_HANG,
            ),
            transport: None,
            cache: MessageCache::default(),
            request_deadline: None,
            uid: Secret::new(String::new()),
            access: Secret::new(String::new()),
            refresh_token: Secret::new(String::new()),
            expires: 0,
            stored: None,
            credentials_dirty: false,
            _lease: None,
            last_failure: std::cell::RefCell::new(None),
            paused: std::cell::RefCell::new(None),
            retry_at: std::cell::Cell::new(None),
            needs_two_factor: false,
            keys: Vec::new(),
            send_keys: Vec::new(),
            listed: None,
            verification: None,
            challenge: std::cell::RefCell::new(None),
        }
    }

    /// None means Proton explicitly rejected the saved refresh credentials.
    /// Network, verification and service errors retain the saved session.
    pub fn resume(
        credentials: crate::session::Credentials,
        verifier: impl FnOnce(&crate::human::Challenge) -> Result<crate::human::Verified, String>,
    ) -> Result<Option<Self>, String> {
        let mut session = Self::empty();
        session.uid = credentials.uid;
        session.access = credentials.access;
        session.refresh_token = credentials.refresh;
        session.expires = credentials.expires;
        if aterm_time::unix_secs().is_some_and(|now| now.saturating_add(60) < session.expires) {
            return Ok(Some(session));
        }
        match session.with_verification(Self::refresh, verifier) {
            Ok(()) => Ok(Some(session)),
            Err(_)
                if session
                    .last_failure
                    .borrow()
                    .as_ref()
                    .is_some_and(crate::proton_error::Failure::rejects_refresh_token) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Structured, credential-free details for the most recent failed API call.
    pub fn last_failure(&self) -> Option<crate::proton_error::Failure> {
        self.last_failure.borrow().clone()
    }

    fn fail(&self, failure: crate::proton_error::Failure) -> String {
        let message = failure.to_string();
        if failure.recovery() == crate::proton_error::Recovery::RetryRead {
            let seconds = failure.retry_after_seconds.unwrap_or(30).max(1);
            let deadline = std::time::Instant::now().checked_add(Duration::from_secs(seconds));
            self.retry_at.set(deadline);
            if deadline.is_none() {
                *self.paused.borrow_mut() = Some(failure.clone());
            }
        } else {
            *self.paused.borrow_mut() = Some(failure.clone());
        }
        *self.last_failure.borrow_mut() = Some(failure);
        message
    }

    fn ready(&self) -> Result<(), String> {
        if let Some(failure) = self.paused.borrow().as_ref() {
            return Err(format!("Mail connection paused: {failure}"));
        }
        if self
            .retry_at
            .get()
            .is_some_and(|at| std::time::Instant::now() < at)
        {
            return Err("Mail connection is waiting before another request".into());
        }
        Ok(())
    }

    pub fn credentials(&self) -> crate::session::Credentials {
        crate::session::Credentials {
            uid: self.uid.clone(),
            access: self.access.clone(),
            refresh: self.refresh_token.clone(),
            expires: self.expires,
        }
    }

    pub fn hold_session(&mut self, lease: crate::session::Lease) {
        self._lease = Some(lease);
    }

    pub fn refresh_if_needed(&mut self) -> Result<bool, String> {
        self.refresh_if_needed_with(Self::refresh)
    }

    fn refresh_if_needed_with(
        &mut self,
        refresh: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<bool, String> {
        if self.transport.is_some() {
            return Ok(false);
        }
        // Publish a previous rotation before another exchange, even when it has expired.
        self.persist_dirty()?;
        let now = aterm_time::unix_secs().ok_or("Invalid system clock")?;
        let refreshed = now.saturating_add(60) >= self.expires;
        if refreshed {
            refresh(self)?;
        }
        Ok(refreshed)
    }

    fn persist_dirty(&mut self) -> Result<(), String> {
        if self.credentials_dirty
            && let Some((home, email)) = &self.stored
        {
            crate::session::save(home, email, &self.credentials())?;
            self.credentials_dirty = false;
        }
        Ok(())
    }

    pub fn persist(&mut self, home: &std::path::Path, email: &str) -> Result<(), String> {
        if self.transport.is_some() {
            return Err("The authenticated transport owns this session".into());
        }
        crate::session::save(home, email, &self.credentials())?;
        self.stored = Some((home.to_owned(), email.to_owned()));
        self.credentials_dirty = false;
        Ok(())
    }

    /// Native success publishes a stored session's rotation before returning. An external
    /// transport owns its credentials and publication instead.
    fn refresh(&mut self) -> Result<(), String> {
        if let Some(transport) = &self.transport {
            return transport.borrow_mut().renew().map_err(|error| {
                let failure = self.fail(crate::proton_error::Failure::new(
                    "/auth/v4/refresh",
                    error.kind,
                ));
                format!("{failure}; {}", error.message)
            });
        }
        self.persist_dirty()?;
        let mut nonce = [0; 32];
        aterm_uds::rand::fill(&mut nonce).map_err(|_| "Could not generate session nonce")?;
        let state = aterm_codec::hex::encode_lower(&nonce);
        let response = self.call(
            Method::Post,
            "/auth/v4/refresh",
            Some(json!({
                "UID": self.uid.expose(), "RefreshToken": self.refresh_token.expose(),
                "AccessToken": self.access.expose(), "ResponseType": "token",
                "GrantType": "refresh_token", "RedirectURI": "https://protonmail.ch", "State": state
            })),
        )?;
        self.install_tokens(&response)?;
        self.persist_dirty()
    }

    fn install_tokens(&mut self, response: &Value) -> Result<(), String> {
        self.install_tokens_at(response, aterm_time::unix_secs())
    }

    fn install_tokens_at(&mut self, response: &Value, now: Option<u64>) -> Result<(), String> {
        let uid = field(response, "UID")?;
        let access = field(response, "AccessToken")?;
        let refresh = field(response, "RefreshToken")?;
        if [uid, access, refresh].iter().any(|value| {
            value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control)
        }) {
            return Err("Proton returned invalid session credentials".into());
        }
        // Unknown expiry forbids cache reuse, but must not discard a valid rotation.
        let expires = now.map_or(0, |now| {
            now.saturating_add(response["ExpiresIn"].as_u64().unwrap_or(3600))
        });
        self.uid = Secret::new(uid.into());
        self.access = Secret::new(access.into());
        self.refresh_token = Secret::new(refresh.into());
        self.expires = expires;
        self.credentials_dirty = true;
        Ok(())
    }

    /// Retry authentication or a read after one verification. A read may also
    /// renew a rejected access token once without starting a new password login.
    /// Do not wrap message submission: an uncertain send must not be retried.
    pub fn with_verification<T>(
        &mut self,
        operation: impl FnMut(&mut Self) -> Result<T, String>,
        verifier: impl FnOnce(&crate::human::Challenge) -> Result<crate::human::Verified, String>,
    ) -> Result<T, String> {
        self.with_recovery(operation, Self::refresh, verifier)
    }

    fn with_recovery<T>(
        &mut self,
        mut operation: impl FnMut(&mut Self) -> Result<T, String>,
        mut renew: impl FnMut(&mut Self) -> Result<(), String>,
        verifier: impl FnOnce(&crate::human::Challenge) -> Result<crate::human::Verified, String>,
    ) -> Result<T, String> {
        self.challenge.take();
        self.verification = None;
        let first = operation(self);
        let mut renewal_pending = first.is_err()
            && (!self.access.expose().is_empty() || self.transport.is_some())
            && self.last_failure.borrow().as_ref().is_some_and(|failure| {
                (failure.kind == crate::proton_error::Kind::SessionExpired
                    || self.transport.is_some()
                        && matches!(
                            failure.kind,
                            crate::proton_error::Kind::Verification
                                | crate::proton_error::Kind::DeviceVerification
                        ))
                    && !matches!(
                        failure.operation,
                        crate::proton_error::Operation::SignIn
                            | crate::proton_error::Operation::AuthInfo
                            | crate::proton_error::Operation::Refresh
                            | crate::proton_error::Operation::TwoFactor
                    )
            });
        let needs_renewal = renewal_pending;
        let mut attempt = |session: &mut Self| {
            if renewal_pending {
                renew(session)?;
                session.verification = None;
                renewal_pending = false;
            }
            operation(session)
        };
        let result = if needs_renewal {
            self.paused.take();
            self.challenge.take();
            attempt(self)
        } else {
            first
        };
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                let Some(challenge) = self.challenge.take() else {
                    return Err(error);
                };
                let verified = verifier(&challenge).map_err(|reason| {
                    format!("{error}; verification could not complete: {reason}")
                })?;
                if !challenge.methods.contains(&verified.method) {
                    return Err("Verification method was not offered".into());
                }
                self.verification = Some(verified);
                self.paused.take();
                self.retry_at.set(None);
                let retried = attempt(self);
                self.verification = None;
                retried
            }
        }
    }

    fn authenticate(&mut self, username: &str, password: &Secret) -> Result<(), String> {
        let info = self.call(
            Method::Post,
            "/auth/v4/info",
            Some(json!({"Username":username})),
        )?;
        let proof = crate::crypto::srp::authenticate(
            password.expose(),
            info["Version"]
                .as_u64()
                .ok_or("Missing Proton auth version")?,
            field(&info, "Salt")?,
            field(&info, "Modulus")?,
            field(&info, "ServerEphemeral")?,
        )?;
        let response = self.call(Method::Post, "/auth/v4", Some(json!({"Username":username,"ClientProof":proof.client_proof,"ClientEphemeral":proof.client_ephemeral,"SRPSession":field(&info,"SRPSession")?})))?;
        proof.verify_server(field(&response, "ServerProof")?)?;
        self.install_tokens(&response)?;
        self.needs_two_factor = response["2FA"]["Enabled"].as_u64().unwrap_or(0) != 0;
        Ok(())
    }

    pub fn two_factor(&mut self, code: &Secret) -> Result<(), String> {
        self.call(
            Method::Post,
            "/auth/v4/2fa",
            Some(json!({"TwoFactorCode":code.expose()})),
        )?;
        self.needs_two_factor = false;
        Ok(())
    }

    /// Verify the requested address actually belongs to this signed-in account.
    pub fn has_address(&self, address: &str) -> Result<bool, String> {
        let response = self.call(Method::Get, "/core/v4/addresses", None)?;
        Ok(response["Addresses"]
            .as_array()
            .ok_or("Proton returned no addresses")?
            .iter()
            .any(|item| {
                item["Email"]
                    .as_str()
                    .is_some_and(|email| email.eq_ignore_ascii_case(address))
            }))
    }

    pub fn sign_out(&self) -> Result<(), String> {
        self.call(Method::Delete, "/auth/v4", None).map(|_| ())
    }

    pub fn sign_out_if_temporary(&self) -> Result<(), String> {
        if self.stored.is_none() && self.transport.is_none() {
            self.sign_out()?;
        }
        Ok(())
    }

    /// Unlock account keys locally, then the address keys those account keys protect.
    pub fn unlock_mailbox(&mut self, password: &Secret) -> Result<(), String> {
        let salts = self.call(Method::Get, "/core/v4/keys/salts", None)?;
        self.unlock_mailbox_keys(password, Some(salts))
    }

    /// Resume with the derived key password of an already unlocked session.
    /// This does not request password-only scopes from a refreshed session.
    pub fn unlock_mailbox_with_key_password(&mut self, password: &Secret) -> Result<(), String> {
        self.unlock_mailbox_keys(password, None)
    }

    fn unlock_mailbox_keys(
        &mut self,
        password: &Secret,
        salts: Option<Value>,
    ) -> Result<(), String> {
        let user = self.call(Method::Get, "/core/v4/users", None)?;
        let addresses = self.call(Method::Get, "/core/v4/addresses", None)?;
        let mut user_keys = Vec::new();
        let mut legacy_passphrases = Vec::new();
        let mut signing_keys = Vec::new();
        for key in user["User"]["Keys"]
            .as_array()
            .ok_or("Proton returned no account keys")?
        {
            let id = field(key, "ID")?;
            let salt = salts.as_ref().and_then(|salts| {
                salts["KeySalts"]
                    .as_array()
                    .and_then(|salts| salts.iter().find(|salt| salt["ID"].as_str() == Some(id)))
                    .and_then(|salt| salt["KeySalt"].as_str())
            });
            let derived = match salt {
                Some(salt) => crate::crypto::mailbox_password(
                    password.expose(),
                    &aterm_codec::base64::decode(salt).map_err(|_| "Invalid mailbox key salt")?,
                )?,
                None => password.clone(),
            };
            let private_key = field(key, "PrivateKey")?;
            if let Ok(mut unlocked) =
                crate::crypto::pgp::unlock(private_key, derived.expose().as_bytes())
            {
                user_keys.append(&mut unlocked);
                signing_keys.extend(crate::crypto::pgp_signature::signing_keys(
                    private_key,
                    derived.expose().as_bytes(),
                )?);
                legacy_passphrases.push(derived);
            }
        }
        if user_keys.is_empty() {
            return Err("Mailbox password could not unlock an account encryption key".into());
        }
        let mut address_keys = Vec::new();
        let mut send_keys = Vec::new();
        for address in addresses["Addresses"]
            .as_array()
            .ok_or("Proton returned no addresses")?
        {
            for key in address["Keys"].as_array().into_iter().flatten() {
                let private_key = field(key, "PrivateKey")?;
                let unlocked = if let Some(token) = key["Token"].as_str().filter(|s| !s.is_empty())
                {
                    match crate::crypto::pgp::decrypt(token, &user_keys) {
                        Ok(token) => {
                            crate::crypto::pgp_signature::verify(
                                &token,
                                field(key, "Signature")?,
                                &signing_keys,
                            )?;
                            crate::crypto::pgp::unlock(private_key, &token)
                        }
                        Err(_) => continue,
                    }
                } else {
                    legacy_passphrases
                        .iter()
                        .find_map(|passphrase| {
                            crate::crypto::pgp::unlock(private_key, passphrase.expose().as_bytes())
                                .ok()
                        })
                        .ok_or_else(|| "Legacy address key could not be unlocked".to_string())
                };
                if let Ok(mut unlocked) = unlocked {
                    if !unlocked.is_empty() && key["Active"].as_u64().unwrap_or(1) != 0 {
                        let entry = (field(address, "Email")?.to_owned(), address_keys.len());
                        if key["Primary"].as_u64() == Some(1) {
                            send_keys.insert(0, entry);
                        } else {
                            send_keys.push(entry);
                        }
                    }
                    address_keys.append(&mut unlocked);
                }
            }
        }
        // Some older accounts use the account key directly for mail.
        address_keys.append(&mut user_keys);
        self.keys = address_keys;
        self.send_keys = send_keys;
        self.listed = Some(AddressList::read(&addresses));
        Ok(())
    }

    /// The account's address list as the unlock that opened this mailbox read it, with no request
    /// of its own: what the lab's address discovery records while a mailbox is open anyway
    /// (docs/DESIGN-alab-lab-model-2026-11-01.md §4.10). `None` before an unlock.
    pub fn address_list(&self) -> Option<&AddressList> {
        self.listed.as_ref()
    }

    /// Test seam: the list an unlock would have read, for callers' tests that cannot unlock.
    #[cfg(any(test, debug_assertions))]
    pub fn read_address_list(&mut self, response: &Value) {
        self.listed = Some(AddressList::read(response));
    }

    /// Whether authentication is owned by an external session transport.
    pub fn externally_authenticated(&self) -> bool {
        self.transport.is_some()
    }

    /// Decrypt a message while retaining its original body and MIME type.
    /// Authentication readers need HTML link targets that a text projection omits.
    pub fn read_source(&mut self, id: &str) -> Result<(Message, String), String> {
        let mut path = String::with_capacity(18 + id.len());
        path.push_str("/mail/v4/messages/");
        crate::oauth::encode_into(&mut path, id);
        let value = self.call(Method::Get, &path, None)?;
        let mut message = message(&value["Message"])?;
        let body = value["Message"]["Body"]
            .as_str()
            .ok_or("Proton returned no message body")?;
        let mut decrypted = crate::crypto::pgp::decrypt(body, &self.keys)?;
        message.text = String::from_utf8(std::mem::take(&mut *decrypted))
            .map_err(|_| "Message body is not UTF-8")?;
        let mime = value["Message"]["MIMEType"]
            .as_str()
            .unwrap_or("text/plain")
            .to_owned();
        Ok((message, mime))
    }

    fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        self.call_started(method, path, body, Instant::now(), API)
    }

    fn call_started(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        started: Instant,
        api: &str,
    ) -> Result<Value, String> {
        if self
            .request_deadline
            .is_some_and(|end| Instant::now() >= end)
        {
            return Err("Mail operation deadline expired".into());
        }
        // Revocation must remain available even when mailbox operations are paused.
        if !(path == "/auth/v4" && matches!(method, Method::Delete)) {
            self.ready()?;
        }
        if let Some(transport) = &self.transport {
            self.last_failure.take();
            let response = transport
                .borrow_mut()
                .request(method, path, body)
                .map_err(|error| {
                    let failure = self.fail(crate::proton_error::Failure::new(path, error.kind));
                    format!("{failure}; {}", error.message)
                })?;
            return self.process_response(
                path,
                response.status,
                response.retry_after.as_deref(),
                &response.body,
            );
        }
        let endpoint = format!("{api}{path}");
        // The API validates a fixed product vocabulary. This selects its mail
        // compatibility protocol; User-Agent identifies our actual client.
        let app_version = if cfg!(target_os = "macos") {
            "macos-bridge@3.27.1"
        } else if cfg!(target_os = "windows") {
            "windows-bridge@3.27.1"
        } else {
            "linux-bridge@3.27.1"
        };
        let mut request = self
            .client
            .request(method, &endpoint)
            .header("x-pm-appversion", app_version)
            .header("User-Agent", crate::user_agent())
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .limit(32 * 1024 * 1024);
        if let Some(token) = &self.verification {
            request = request
                .secret_header(
                    "x-pm-human-verification-token",
                    aterm_uds::secret::Secret::new(token.token.expose().as_bytes()),
                )
                .header("x-pm-human-verification-token-type", &token.method);
        }
        // Proton's refresh endpoint authenticates the credentials in its body.
        // An expired access token must not become the refresh request's bearer.
        if !self.access.expose().is_empty() && path != "/auth/v4/refresh" {
            let access = self.access.expose().as_bytes();
            let mut bearer = Vec::with_capacity(7 + access.len());
            bearer.extend_from_slice(b"Bearer ");
            bearer.extend_from_slice(access);
            request = request
                .secret_header("Authorization", aterm_uds::secret::Secret::from_vec(bearer))
                .secret_header(
                    "x-pm-uid",
                    aterm_uds::secret::Secret::new(self.uid.expose().as_bytes()),
                );
        }
        let bytes = zeroize::Zeroizing::new(
            body.map(|body| aterm_json::to_vec(&body))
                .transpose()
                .map_err(|_| "Could not encode Proton request")?
                .unwrap_or_default(),
        );
        let own = started + REQUEST_HANG;
        let end = self.request_deadline.map_or(own, |end| end.min(own));
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("Mail operation deadline expired".into());
        }
        self.last_failure.take();
        let response = request
            .backstop(left)
            .send_phased(&bytes)
            .map_err(|error| self.fail(crate::proton_error::Failure::transport(path, &error)))?;
        self.process_response(
            path,
            response.status(),
            response.header("Retry-After"),
            response.body(),
        )
    }

    fn process_response(
        &self,
        path: &str,
        status: u16,
        retry_after: Option<&str>,
        body: &[u8],
    ) -> Result<Value, String> {
        let parsed: Value = aterm_json::from_slice(body).map_err(|_| {
            let mut failure = crate::proton_error::Failure::response(path, status, 0, retry_after);
            if (200..300).contains(&status) {
                failure.kind = crate::proton_error::Kind::Protocol;
            }
            self.fail(failure)
        })?;
        let mut code = parsed["Code"].as_u64().unwrap_or(0);
        if (200..300).contains(&status)
            && code == 1001
            && matches!(
                path,
                "/mail/v4/messages/delete"
                    | "/mail/v4/messages/read"
                    | "/mail/v4/messages/unread"
                    | "/mail/v4/messages/label"
                    | "/mail/v4/messages/unlabel"
            )
            && let Some(responses) = parsed["Responses"]
                .as_array()
                .filter(|rows| !rows.is_empty())
        {
            if responses
                .iter()
                .all(|row| row["Response"]["Code"].as_u64() == Some(1000))
            {
                return Ok(parsed);
            }
            code = responses
                .iter()
                .filter_map(|row| row["Response"]["Code"].as_u64())
                .find(|code| *code != 1000)
                .unwrap_or(1001);
        }
        if !(200..300).contains(&status) || code != 1000 {
            let failure = crate::proton_error::Failure::response(path, status, code, retry_after);
            let reason = self.fail(failure);
            if code == 9001
                && parsed["Details"]["HumanVerificationMethods"]
                    .as_array()
                    .is_some_and(|methods| {
                        methods
                            .iter()
                            .any(|method| method.as_str().is_some_and(crate::human::supported))
                    })
                && let Some(token) =
                    parsed["Details"]["HumanVerificationToken"]
                        .as_str()
                        .filter(|s| {
                            !s.is_empty() && s.len() <= 8192 && !s.chars().any(char::is_control)
                        })
            {
                let methods = parsed["Details"]["HumanVerificationMethods"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|method| crate::human::supported(method))
                    .map(str::to_owned)
                    .collect();
                *self.challenge.borrow_mut() = Some(crate::human::Challenge {
                    token: Secret::new(token.to_owned()),
                    methods,
                });
            }
            if code == 9001 && self.challenge.borrow().is_none() {
                let mut methods = String::new();
                if let Some(values) = parsed["Details"]["HumanVerificationMethods"].as_array() {
                    for name in values.iter().filter_map(Value::as_str) {
                        if matches!(
                            name,
                            "captcha" | "email" | "sms" | "ownership-email" | "ownership-sms"
                        ) {
                            if !methods.is_empty() {
                                methods.push_str(", ");
                            }
                            methods.push_str(name);
                        }
                    }
                }
                let methods = if methods.is_empty() {
                    "none supplied"
                } else {
                    methods.as_str()
                };
                return Err(format!(
                    "{reason}; no supported verification challenge (methods: {methods})"
                ));
            }
            return Err(reason);
        }
        Ok(parsed)
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Proton response has no {key}"))
}

impl Mailbox for Proton {
    fn folders(&mut self) -> Result<Vec<Folder>, String> {
        let mut folders = vec![
            Folder {
                id: "0".into(),
                name: "Inbox".into(),
            },
            Folder {
                id: "5".into(),
                name: "All mail".into(),
            },
            Folder {
                id: "1".into(),
                name: "Drafts".into(),
            },
            Folder {
                id: "2".into(),
                name: "Sent".into(),
            },
            Folder {
                id: "6".into(),
                name: "Archive".into(),
            },
            Folder {
                id: "4".into(),
                name: "Spam".into(),
            },
            Folder {
                id: "3".into(),
                name: "Trash".into(),
            },
        ];
        let value = self.call(Method::Get, "/core/v4/labels?Type=1", None)?;
        for label in value["Labels"]
            .as_array()
            .ok_or("Proton returned no labels")?
        {
            folders.push(Folder {
                id: field(label, "ID")?.into(),
                name: field(label, "Name")?.into(),
            });
        }
        Ok(folders)
    }

    fn messages(&mut self, folder: &str, limit: usize) -> Result<Vec<Message>, String> {
        let messages = self.message_headers(folder, limit)?;
        self.cache.clear();
        Ok(messages)
    }

    fn read(&mut self, folder: &str, id: &str) -> Result<Message, String> {
        if folder != "1"
            && let Some(message) = self.cache.get(id)
        {
            return Ok(message);
        }
        let (mut message, mime) = self.read_source(id)?;
        message.text = project_body(&mime, message.text);
        if folder != "1" {
            self.cache.insert(message.clone());
        }
        Ok(message)
    }

    fn mark_read(&mut self, _folder: &str, id: &str, read: bool) -> Result<(), String> {
        self.call(
            Method::Put,
            if read {
                "/mail/v4/messages/read"
            } else {
                "/mail/v4/messages/unread"
            },
            Some(json!({"IDs":[id]})),
        )?;
        self.cache.clear();
        Ok(())
    }
}

fn project_body(mime: &str, body: String) -> String {
    let media = mime.split(';').next().unwrap_or("").trim();
    if media.eq_ignore_ascii_case("text/html") {
        crate::mime::html_text(&body)
    } else if media
        .get(..10)
        .is_some_and(|p| p.eq_ignore_ascii_case("multipart/"))
        && let Ok(parsed) = crate::mime::parse(body.as_bytes())
    {
        parsed.text
    } else {
        body
    }
}

fn message(value: &Value) -> Result<Message, String> {
    let mut recipients = Vec::new();
    let mut to = String::new();
    for key in ["ToList", "CCList", "BCCList"] {
        for addr in value[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["Address"].as_str().map(str::trim))
            .filter(|addr| !addr.is_empty())
        {
            if !recipients
                .iter()
                .any(|seen: &&str| seen.eq_ignore_ascii_case(addr))
            {
                if !to.is_empty() {
                    to.push_str(", ");
                }
                to.push_str(addr);
                recipients.push(addr);
            }
        }
    }
    Ok(Message {
        id: field(value, "ID")?.into(),
        from: value["Sender"]["Address"].as_str().unwrap_or("").into(),
        to,
        subject: value["Subject"].as_str().unwrap_or("").into(),
        date: value["Time"]
            .as_i64()
            .map(|t| t.to_string())
            .unwrap_or_default(),
        unread: value["Unread"].as_u64().unwrap_or(0) != 0,
        text: String::new(),
    })
}

#[cfg(test)]
#[path = "proton/native_wait_tests.rs"]
mod native_wait_tests;

#[cfg(test)]
#[path = "proton_session_tests.rs"]
mod session_tests;
