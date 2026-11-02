// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native Proton API operations and encryption, with an optional authenticated transport.

use crate::{Folder, Mailbox, Message, Secret};
use aterm_http::{Client, Method, ProxyMode, Trust};
use aterm_json::{Value, json};
use std::time::Duration;

const API: &str = "https://mail.proton.me/api";

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
        let message = self.entries.remove(index)?;
        self.entries.push_front(message.clone());
        Some(message)
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

#[path = "proton_send.rs"]
mod send;
pub use send::PreparedMessage;

pub struct Proton {
    client: Client,
    transport: Option<std::cell::RefCell<Box<dyn ApiTransport>>>,
    cache: MessageCache,
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
    verification: Option<crate::human::Verified>,
    challenge: std::cell::RefCell<Option<crate::human::Challenge>>,
}

impl Proton {
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
                Duration::from_secs(30),
            ),
            transport: None,
            cache: MessageCache::default(),
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
        if self.transport.is_some() {
            return Ok(false);
        }
        let now = aterm_time::unix_secs().ok_or("Invalid system clock")?;
        let refreshed = now.saturating_add(60) >= self.expires;
        if refreshed {
            self.refresh()?;
        }
        self.persist_dirty()?;
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
        let mut nonce = [0; 32];
        aterm_uds::rand::fill(&mut nonce).map_err(|_| "Could not generate session nonce")?;
        let state: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let response = self.call(
            Method::Post,
            "/auth/v4/refresh",
            Some(json!({
                "UID": self.uid.expose(), "RefreshToken": self.refresh_token.expose(),
                "AccessToken": self.access.expose(), "ResponseType": "token",
                "GrantType": "refresh_token", "RedirectURI": "https://protonmail.ch", "State": state
            })),
        )?;
        self.install_tokens(&response)
    }

    fn install_tokens(&mut self, response: &Value) -> Result<(), String> {
        let uid = field(response, "UID")?;
        let access = field(response, "AccessToken")?;
        let refresh = field(response, "RefreshToken")?;
        if [uid, access, refresh].iter().any(|value| {
            value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control)
        }) {
            return Err("Proton returned invalid session credentials".into());
        }
        self.uid = Secret::new(uid.into());
        self.access = Secret::new(access.into());
        self.refresh_token = Secret::new(refresh.into());
        self.credentials_dirty = true;
        self.expires = aterm_time::unix_secs()
            .ok_or("Invalid system clock")?
            .saturating_add(response["ExpiresIn"].as_u64().unwrap_or(3600));
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
        self.with_recovery(
            operation,
            |session| {
                session.refresh()?;
                session.persist_dirty()
            },
            verifier,
        )
    }

    fn with_recovery<T>(
        &mut self,
        mut operation: impl FnMut(&mut Self) -> Result<T, String>,
        mut renew: impl FnMut(&mut Self) -> Result<(), String>,
        verifier: impl FnOnce(&crate::human::Challenge) -> Result<crate::human::Verified, String>,
    ) -> Result<T, String> {
        self.challenge.take();
        let first = operation(self);
        let mut renewal_pending = first.is_err()
            && (!self.access.expose().is_empty() || self.transport.is_some())
            && self.last_failure.borrow().as_ref().is_some_and(|failure| {
                failure.kind == crate::proton_error::Kind::SessionExpired
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
                renewal_pending = false;
            }
            operation(session)
        };
        let result = if needs_renewal {
            self.paused.take();
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
                attempt(self)
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
            if let Ok(mut unlocked) =
                crate::crypto::pgp::unlock(field(key, "PrivateKey")?, derived.expose().as_bytes())
            {
                user_keys.append(&mut unlocked);
                signing_keys.extend(crate::crypto::pgp_signature::signing_keys(
                    field(key, "PrivateKey")?,
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
        Ok(())
    }

    fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value, String> {
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
        let endpoint = format!("{API}{path}");
        let platform = if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            "linux"
        };
        // The API validates a fixed product vocabulary. This selects its mail
        // compatibility protocol; User-Agent identifies our actual client.
        let app_version = format!("{platform}-bridge@3.27.1");
        let mut request = self
            .client
            .request(method, &endpoint)
            .header("x-pm-appversion", &app_version)
            .header(
                "User-Agent",
                concat!("aterm-mail/", env!("CARGO_PKG_VERSION")),
            )
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
            request = request
                .secret_header(
                    "Authorization",
                    aterm_uds::secret::Secret::from_vec(
                        format!("Bearer {}", self.access.expose()).into_bytes(),
                    ),
                )
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
        self.last_failure.take();
        let response = request
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
                && let Some(token) = parsed["Details"]["HumanVerificationToken"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 8192)
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
                let methods = parsed["Details"]["HumanVerificationMethods"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .filter(|name| {
                                matches!(
                                    *name,
                                    "captcha"
                                        | "email"
                                        | "sms"
                                        | "ownership-email"
                                        | "ownership-sms"
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "none supplied".into());
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
        let path = format!(
            "/mail/v4/messages?LabelID={}&PageSize={}&Sort=Time&Desc=1",
            crate::oauth::encode(folder),
            limit.min(100)
        );
        let value = self.call(Method::Get, &path, None)?;
        self.cache.clear();
        value["Messages"]
            .as_array()
            .ok_or_else(|| "Proton returned no message list".to_string())?
            .iter()
            .map(message)
            .collect()
    }

    fn read(&mut self, folder: &str, id: &str) -> Result<Message, String> {
        if folder != "1"
            && let Some(message) = self.cache.get(id)
        {
            return Ok(message);
        }
        let value = self.call(
            Method::Get,
            &format!("/mail/v4/messages/{}", crate::oauth::encode(id)),
            None,
        )?;
        let mut message = message(&value["Message"])?;
        let body = value["Message"]["Body"]
            .as_str()
            .ok_or("Proton returned no message body")?;
        let decrypted = crate::crypto::pgp::decrypt(body, &self.keys)?;
        let body = std::str::from_utf8(&decrypted).map_err(|_| "Message body is not UTF-8")?;
        message.text = if value["Message"]["MIMEType"].as_str() == Some("text/html") {
            crate::mime::html_text(body)
        } else {
            body.into()
        };
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

fn message(value: &Value) -> Result<Message, String> {
    let addresses = |key: &str| {
        value[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["Address"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    Ok(Message {
        id: field(value, "ID")?.into(),
        from: value["Sender"]["Address"].as_str().unwrap_or("").into(),
        to: addresses("ToList"),
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
mod session_tests {
    use super::*;

    #[test]
    fn bulk_success_requires_every_item_and_is_not_a_read_success() {
        let ok = br#"{"Code":1001,"Responses":[{"ID":"a","Response":{"Code":1000}}]}"#;
        assert!(
            Proton::empty()
                .process_response("/mail/v4/messages/delete", 200, None, ok)
                .is_ok()
        );
        assert!(
            Proton::empty()
                .process_response("/mail/v4/messages", 200, None, ok)
                .is_err()
        );
        for bad in [
            br#"{"Code":1001,"Responses":[]}"#.as_slice(),
            br#"{"Code":1001,"Responses":[{"Response":{"Code":1000}},{"Response":{"Code":2001}}]}"#,
        ] {
            assert!(
                Proton::empty()
                    .process_response("/mail/v4/messages/read", 200, None, bad)
                    .is_err()
            );
        }
    }

    fn cached_message(id: &str, text: &str) -> Message {
        Message {
            id: id.into(),
            from: String::new(),
            to: String::new(),
            subject: String::new(),
            date: String::new(),
            unread: false,
            text: text.into(),
        }
    }

    #[test]
    fn message_cache_bounds_bytes_and_evicts_least_recently_read() {
        let mut cache = MessageCache {
            max_entries: 2,
            max_bytes: 20,
            ..Default::default()
        };
        cache.insert(cached_message("a", "one"));
        cache.insert(cached_message("b", "two"));
        assert!(cache.get("a").is_some());
        cache.insert(cached_message("c", "three"));
        assert!(cache.get("b").is_none());
        assert!(cache.get("a").is_some());
        cache.insert(cached_message("huge", &"x".repeat(21)));
        assert_eq!(cache.entries.len(), 2);
        assert!(cache.bytes <= cache.max_bytes);
        cache.insert(cached_message("a", "updated"));
        assert_eq!(cache.get("a").unwrap().text, "updated");
        cache.clear();
        assert_eq!((cache.entries.len(), cache.bytes), (0, 0));
    }

    #[test]
    fn cached_body_avoids_network_and_folder_refresh_invalidates_it() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
            responses: [reply(200, r#"{"Code":1000,"Messages":[]}"#)].into(),
            ..Default::default()
        }));
        let mut proton = Proton::with_transport(Box::new(Channel(state.clone())));
        proton.cache.insert(cached_message("one", "cached body"));
        assert_eq!(proton.read("0", "one").unwrap().text, "cached body");
        assert_eq!(state.lock().unwrap().requests, 0);
        proton.messages("0", 50).unwrap();
        assert!(proton.cache.get("one").is_none());
        assert_eq!(state.lock().unwrap().requests, 1);
    }

    #[derive(Default)]
    struct ChannelState {
        requests: usize,
        renewals: usize,
        responses: std::collections::VecDeque<Result<ApiResponse, ApiError>>,
    }

    struct Channel(std::sync::Arc<std::sync::Mutex<ChannelState>>);

    impl ApiTransport for Channel {
        fn request(
            &mut self,
            _: Method,
            _: &str,
            _: Option<Value>,
        ) -> Result<ApiResponse, ApiError> {
            let mut state = self.0.lock().unwrap();
            state.requests += 1;
            state
                .responses
                .pop_front()
                .expect("unexpected repeated request")
        }

        fn renew(&mut self) -> Result<(), ApiError> {
            self.0.lock().unwrap().renewals += 1;
            Ok(())
        }
    }

    fn reply(status: u16, body: &str) -> Result<ApiResponse, ApiError> {
        Ok(ApiResponse {
            status,
            retry_after: None,
            body: body.as_bytes().to_vec(),
        })
    }

    #[test]
    fn external_session_renews_once_without_rotating_browser_credentials() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
            responses: [
                reply(401, r#"{"Code":401}"#),
                reply(200, r#"{"Code":1000,"Messages":[]}"#),
            ]
            .into(),
            ..Default::default()
        }));
        let mut proton = Proton::with_transport(Box::new(Channel(state.clone())));
        assert!(!proton.refresh_if_needed().unwrap());
        assert!(
            proton
                .with_verification(|p| p.messages("0", 10), |_| panic!("not a human challenge"))
                .unwrap()
                .is_empty()
        );
        let state = state.lock().unwrap();
        assert_eq!((state.requests, state.renewals), (2, 1));
        assert!(
            proton
                .persist(std::path::Path::new("/unavailable"), "test@example.test")
                .is_err()
        );
    }

    #[test]
    fn rejected_external_credentials_stop_background_retries() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
            responses: [Err(ApiError {
                kind: crate::proton_error::Kind::Credentials,
                message: "credentials rejected".into(),
            })]
            .into(),
            ..Default::default()
        }));
        let mut proton = Proton::with_transport(Box::new(Channel(state.clone())));
        assert!(proton.messages("0", 10).is_err());
        assert!(proton.messages("0", 10).is_err());
        assert_eq!(state.lock().unwrap().requests, 1);
        assert_eq!(
            proton.last_failure().unwrap().kind,
            crate::proton_error::Kind::Credentials
        );
    }

    #[test]
    fn uncertain_external_write_is_not_replayed_and_failed_channel_is_throttled() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(ChannelState {
            responses: [Err("disconnected".into())].into(),
            ..Default::default()
        }));
        let mut proton = Proton::with_transport(Box::new(Channel(state.clone())));
        assert!(proton.mark_read("0", "one", true).is_err());
        assert!(proton.mark_read("0", "one", true).is_err());
        let state = state.lock().unwrap();
        assert_eq!((state.requests, state.renewals), (1, 0));
    }

    #[test]
    fn revoked_access_token_renews_once_and_retries_only_the_read() {
        let mut proton = Proton::empty();
        proton.access = Secret::new("old-access".into());
        let mut reads = 0;
        let mut renewals = 0;
        let result = proton.with_recovery(
            |session| {
                reads += 1;
                session.process_response("/core/v4/addresses", 401, None, br#"{"Code":401}"#)
            },
            |_| {
                renewals += 1;
                Ok(())
            },
            |_| panic!("Expired tokens are not ownership challenges"),
        );
        assert!(result.is_err());
        assert_eq!((reads, renewals), (2, 1));
    }

    #[test]
    fn refresh_challenge_is_verified_before_retrying_the_original_read() {
        let mut proton = Proton::empty();
        proton.access = Secret::new("old-access".into());
        let mut reads = 0;
        let mut renewals = 0;
        let result = proton.with_recovery(
            |session| {
                reads += 1;
                if reads == 1 {
                    session.process_response("/core/v4/addresses", 401, None, br#"{"Code":401}"#)
                } else {
                    session.process_response("/core/v4/addresses", 200, None, br#"{"Code":1000}"#)
                }
            },
            |session| {
                renewals += 1;
                if renewals == 1 {
                    session.process_response("/auth/v4/refresh", 422, None, br#"{"Code":9001,"Details":{"HumanVerificationMethods":["ownership-email"],"HumanVerificationToken":"private-token"}}"#).map(|_| ())
                } else { Ok(()) }
            },
            |_| Ok(crate::human::Verified { token: Secret::new("proof".into()), method: "ownership-email".into() }),
        );
        assert!(result.is_ok());
        assert_eq!((reads, renewals), (2, 2));
    }

    #[test]
    fn invalid_refresh_token_does_not_loop_through_another_refresh() {
        let mut proton = Proton::empty();
        proton.access = Secret::new("old-access".into());
        assert!(
            proton
                .with_recovery(
                    |session| session.process_response(
                        "/auth/v4/refresh",
                        422,
                        None,
                        br#"{"Code":10013}"#
                    ),
                    |_| panic!("A rejected refresh token must not trigger another renewal"),
                    |_| panic!("A rejected refresh token is not a verification challenge"),
                )
                .is_err()
        );
    }

    #[test]
    fn valid_saved_access_token_does_not_force_a_refresh_or_verification() {
        let credentials = crate::session::Credentials {
            uid: Secret::new("uid".into()),
            access: Secret::new("access".into()),
            refresh: Secret::new("refresh".into()),
            expires: aterm_time::unix_secs().unwrap().saturating_add(3600),
        };
        let session = Proton::resume(credentials, |_| {
            panic!("An unexpired token needs no verification")
        })
        .unwrap()
        .unwrap();
        assert_eq!(session.access.expose(), "access");
        assert!(!session.credentials_dirty);
    }

    #[test]
    fn verification_failure_keeps_the_original_operation_and_code() {
        let mut proton = Proton::empty();
        let error = proton.with_verification(
            |proton| proton.process_response("/core/v4/addresses", 422, None, br#"{"Code":9001,"Details":{"HumanVerificationMethods":["ownership-email"],"HumanVerificationToken":"private-token"}}"#),
            |_| Err("Recovery mailbox unavailable".into()),
        ).unwrap_err();
        assert!(error.contains("account addresses"));
        assert!(error.contains("9001"));
        assert!(error.contains("Recovery mailbox unavailable"));
        assert!(!error.contains("private-token"));
        assert_eq!(
            proton.last_failure().unwrap().kind,
            crate::proton_error::Kind::Verification
        );
    }

    #[test]
    fn verification_retries_once_and_never_retries_credentials() {
        let mut proton = Proton::empty();
        let mut calls = 0;
        let result = proton.with_verification(
            |proton| {
                calls += 1;
                proton.process_response("/auth/v4", 422, None, br#"{"Code":9001,"Details":{"HumanVerificationMethods":["ownership-email"],"HumanVerificationToken":"private-token"}}"#)
            },
            |_| Ok(crate::human::Verified { token: Secret::new("proof".into()), method: "ownership-email".into() }),
        );
        assert!(result.is_err());
        assert_eq!(calls, 2);
        let result = proton.with_verification(
            |proton| proton.process_response("/auth/v4", 422, None, br#"{"Code":8002}"#),
            |_| panic!("Incorrect passwords must not trigger ownership verification"),
        );
        assert!(result.unwrap_err().contains("credentials"));
    }

    #[test]
    fn non_json_rate_limit_preserves_retry_and_never_becomes_bad_credentials() {
        let proton = Proton::empty();
        assert!(
            proton
                .process_response("/auth/v4/refresh", 429, Some("60"), b"unavailable")
                .is_err()
        );
        let failure = proton.last_failure().unwrap();
        assert_eq!(failure.kind, crate::proton_error::Kind::RateLimited);
        assert_eq!(failure.retry_after_seconds, Some(60));
        assert!(proton.ready().is_err());
        proton.retry_at.set(Some(std::time::Instant::now()));
        assert!(proton.ready().is_ok());
    }

    #[test]
    fn paused_connections_do_not_repeat_authentication_or_verification() {
        let proton = Proton::empty();
        assert!(
            proton
                .process_response("/auth/v4", 422, None, br#"{"Code":8002}"#)
                .is_err()
        );
        assert!(proton.ready().unwrap_err().contains("paused"));
        // An attempt made while paused fails before touching the transport.
        assert!(
            proton
                .call(Method::Get, "/core/v4/addresses", None)
                .unwrap_err()
                .contains("paused")
        );
        assert_eq!(proton.last_failure().unwrap().api_code, Some(8002));
    }

    #[test]
    fn token_rotation_validates_every_field_before_replacing_credentials() {
        let mut proton = Proton::empty();
        proton.install_tokens(&json!({"UID":"uid", "AccessToken":"old-access", "RefreshToken":"old-refresh", "ExpiresIn":3600})).unwrap();
        assert!(proton.install_tokens(&json!({"UID":"uid", "AccessToken":"new-access", "RefreshToken":"bad\r\nheader"})).is_err());
        assert_eq!(proton.access.expose(), "old-access");
        assert_eq!(proton.refresh_token.expose(), "old-refresh");
        proton.install_tokens(&json!({"UID":"uid", "AccessToken":"new-access", "RefreshToken":"new-refresh", "ExpiresIn":3600})).unwrap();
        assert_eq!(proton.refresh_token.expose(), "new-refresh");
        assert!(proton.credentials_dirty);
    }

    #[cfg(unix)]
    #[test]
    fn failed_session_write_retries_without_losing_rotated_tokens() {
        let mut nonce = [0; 8];
        aterm_uds::rand::fill(&mut nonce).unwrap();
        let home = std::env::temp_dir().join(format!(
            "aterm-mail-rotation-{}-{:x}",
            std::process::id(),
            u64::from_le_bytes(nonce)
        ));
        std::fs::write(&home, b"not a directory").unwrap();
        let mut proton = Proton::empty();
        proton.install_tokens(&json!({"UID":"uid", "AccessToken":"new-access", "RefreshToken":"new-refresh", "ExpiresIn":3600})).unwrap();
        proton.stored = Some((home.clone(), "a@example.test".into()));
        assert!(proton.refresh_if_needed().is_err());
        assert!(proton.credentials_dirty);
        assert_eq!(proton.refresh_token.expose(), "new-refresh");
        std::fs::remove_file(&home).unwrap();
        assert!(!proton.refresh_if_needed().unwrap());
        assert!(!proton.credentials_dirty);
        assert_eq!(
            crate::session::load(&home, "a@example.test")
                .unwrap()
                .unwrap()
                .refresh
                .expose(),
            "new-refresh"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
