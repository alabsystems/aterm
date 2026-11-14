// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! One browser transport per account across independent aterm clients.
//!
//! The private socket carries one bounded operation per connection. An idle CLI
//! therefore holds neither the browser nor the service queue. The detached owner
//! keeps the profile lock until its idle lease expires. Native Proton parsing,
//! identity validation and message decryption remain in each caller.

use super::MailboxOpenError;
use super::native::{BrowserTransport, Credentials, HeadlessMail, StoredCredentials, digest};
use aterm_json::{Value, json};
use aterm_mail::proton::{ApiError, ApiResponse, ApiTransport, Proton};
use aterm_mail::proton_error::Kind;
use aterm_uds::private_file::{PrivateLock, ensure_private_dir};
use aterm_uds::{CtlListener, CtlStream};
use std::io::{self, Read, Write};
use std::os::fd::AsFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// Hang detector for required admitted owner work inside the original caller expiry.
const OPERATION: Duration = Duration::from_secs(60);
const IDLE: Duration = Duration::from_secs(300);
const MAX_HEADER: usize = 64 * 1024;
const MAX_BODY: usize = 32 * 1024 * 1024;

// Every operation uses nonblocking I/O with one absolute readiness deadline.
// A trickle of bytes cannot extend the request's budget.
struct DeadlineStream<'a> {
    stream: &'a mut CtlStream,
    end: Instant,
}

impl Read for DeadlineStream<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            remaining(self.end)?;
            match self.stream.read(bytes) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    aterm_uds::ready::until(
                        self.stream.as_fd(),
                        aterm_uds::ready::Interest::Read,
                        self.end,
                    )?;
                }
                result => return result,
            }
        }
    }
}

impl Write for DeadlineStream<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            remaining(self.end)?;
            match self.stream.write(bytes) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    aterm_uds::ready::until(
                        self.stream.as_fd(),
                        aterm_uds::ready::Interest::Write,
                        self.end,
                    )?;
                }
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn failure(kind: Kind, message: impl Into<String>) -> ApiError {
    ApiError {
        kind,
        message: message.into(),
    }
}

fn io_failure(error: io::Error) -> ApiError {
    let kind = match error.kind() {
        io::ErrorKind::PermissionDenied => Kind::AccessDenied,
        io::ErrorKind::InvalidInput => Kind::Configuration,
        io::ErrorKind::InvalidData => Kind::Protocol,
        io::ErrorKind::TimedOut => Kind::Timeout,
        _ => Kind::Network,
    };
    failure(kind, error.to_string())
}

fn remaining(end: Instant) -> io::Result<Duration> {
    end.checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Mail operation deadline expired"))
}

// Frames have a fixed length prefix; reject lengths before allocating. Secrets
// received during bootstrap or key unlock are erased with their frame buffers.
fn read_frame(input: &mut impl Read, limit: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut size = [0; 4];
    input.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Mail frame exceeds limit",
        ));
    }
    let mut bytes = Zeroizing::new(vec![0; size]);
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn write_frame(output: &mut impl Write, bytes: &[u8], limit: usize) -> io::Result<()> {
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Mail frame exceeds limit",
        ));
    }
    output.write_all(&(bytes.len() as u32).to_be_bytes())?;
    output.write_all(bytes)
}

fn private_peer(stream: &CtlStream) -> io::Result<()> {
    if aterm_uds::peer::peer_uid(stream) != Some(aterm_uds::peer::our_uid()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Mail owner peer has another identity",
        ));
    }
    Ok(())
}

struct Endpoint {
    dir: PathBuf,
    socket: PathBuf,
    key: String,
}

impl Endpoint {
    fn new(home: &Path, account: &str) -> io::Result<Self> {
        let mut dir = home.join(".aterm/mail-owners");
        ensure_private_dir(&dir)?;
        let mut key = digest(account.trim().to_ascii_lowercase().as_bytes());
        // 128 bits names the endpoint; the full account is checked on every request.
        let mut socket = dir.join(format!("{}.sock", &key[..32]));
        if std::os::unix::net::SocketAddr::from_pathname(socket.with_extension("pending")).is_err()
        {
            // Keep arbitrary-length homes isolated even when sun_path cannot
            // represent them. The canonical home participates in this name.
            let mut identity = std::fs::canonicalize(&dir)?.as_os_str().as_bytes().to_vec();
            identity.push(0);
            identity.extend_from_slice(account.trim().to_ascii_lowercase().as_bytes());
            key = digest(&identity);
            dir = PathBuf::from(format!("/tmp/.aterm-mail-{}", aterm_uds::peer::our_uid()));
            ensure_private_dir(&dir)?;
            socket = dir.join(format!("{}.sock", &key[..32]));
        }
        Ok(Self { dir, socket, key })
    }

    fn metadata(&self) -> io::Result<Option<std::fs::Metadata>> {
        match std::fs::symlink_metadata(&self.socket) {
            Ok(meta)
                if meta.file_type().is_socket()
                    && meta.uid() == aterm_uds::peer::our_uid()
                    && meta.mode() & 0o077 == 0 =>
            {
                Ok(Some(meta))
            }
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Mail owner socket is not private",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn connect(&self, end: Instant) -> io::Result<CtlStream> {
        if self.metadata()?.is_none() {
            return Err(io::ErrorKind::NotFound.into());
        }
        let stream = aterm_uds::connect::until(&self.socket, end)?;
        private_peer(&stream)?;
        Ok(stream)
    }
}

struct RemoteTransport {
    home: PathBuf,
    credentials: StoredCredentials,
    end: Option<Instant>,
}

impl RemoteTransport {
    fn connect(&self, end: Instant) -> io::Result<CtlStream> {
        let account = self
            .credentials
            .borrowed()
            .email
            .trim()
            .to_ascii_lowercase();
        let endpoint = Endpoint::new(&self.home, &account)?;
        match endpoint.connect(end) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) => {}
            Err(error) => return Err(error),
        }
        // Serialize births, never the lifetime of a reader. The child separately
        // holds the owner lock, so a stale socket cannot create a second browser.
        let _startup = PrivateLock::take(
            &endpoint.dir,
            &format!("{}.start", endpoint.key),
            remaining(end)?,
            "Mail owner is starting",
        )?;
        match endpoint.connect(end) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) => {}
            Err(error) => return Err(error),
        }
        let credentials = self.credentials.borrowed();
        let bootstrap = Zeroizing::new(
            aterm_json::to_vec(&json!({
                "email": credentials.email, "login": credentials.login,
                "password": credentials.password, "mailbox_password": credentials.mailbox_password,
                "totp_secret": credentials.totp_secret,
            }))
            .map_err(io::Error::other)?,
        );
        if bootstrap.len() > MAX_HEADER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Mail credentials exceed limit",
            ));
        }
        let (reader, mut writer) = aterm_uds::kept_out(CtlStream::pair)?;
        writer.set_nonblocking(true)?;
        // The owner is `aterm email --transport-owner`, so it is started by aterm's own file
        // and not by the name this process was started under: under a sign-in's opener that
        // name is `aterm-open-url`, which answers as the opener again, fails on these
        // arguments and records that failure as the waiting sign-in's own
        // (`open_url::publish_failed_exit`), which ends the sign-in with no reason. It takes
        // no part in a sign-in, so it is not told where one's outcome or code goes.
        let mut command = aterm_uds::spawnfd::command(crate::own_exe().map_err(io::Error::other)?);
        command
            .env_remove(super::super::open_url::OUTCOME_ENV)
            .env_remove(super::super::open_url::CODE_ENV)
            .arg("email")
            .arg("--transport-owner")
            .arg(&self.home)
            .arg(&account)
            .stdin(std::os::fd::OwnedFd::from(reader))
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .setsid(true);
        let mut child = command.spawn()?;
        write_frame(
            &mut DeadlineStream {
                stream: &mut writer,
                end,
            },
            &bootstrap,
            MAX_HEADER,
        )?;
        drop(writer);
        loop {
            match endpoint.connect(end) {
                Ok(stream) => return Ok(stream),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) => {}
                Err(error) => return Err(error),
            }
            if child.try_wait()?.is_some() {
                return Err(io::Error::other(
                    "Mail owner exited before accepting connections",
                ));
            }
            std::thread::sleep(remaining(end)?.min(Duration::from_millis(10)));
        }
    }

    fn exchange(
        &self,
        request: Value,
        body: &[u8],
    ) -> Result<(Value, Zeroizing<Vec<u8>>), ApiError> {
        self.exchange_started(request, body, Instant::now())
    }

    fn exchange_started(
        &self,
        request: Value,
        body: &[u8],
        started: Instant,
    ) -> Result<(Value, Zeroizing<Vec<u8>>), ApiError> {
        let hang = started + OPERATION;
        let end = self.end.map_or(hang, |end| end.min(hang));
        remaining(end).map_err(io_failure)?;
        let stream = self.connect(end).map_err(io_failure)?;
        let left = remaining(end).map_err(io_failure)?;
        stream.set_nonblocking(true).map_err(io_failure)?;
        let Value::Object(mut request) = request else {
            return Err(failure(Kind::Configuration, "Invalid mail owner request"));
        };
        request.insert("version".into(), json!(1));
        request.insert(
            "account".into(),
            json!(
                self.credentials
                    .borrowed()
                    .email
                    .trim()
                    .to_ascii_lowercase()
            ),
        );
        request.insert(
            "expires".into(),
            json!(monotonic_ms()?.saturating_add(left.as_millis() as u64)),
        );
        let header = aterm_json::to_vec(&Value::Object(request))
            .map_err(|error| failure(Kind::Configuration, error.to_string()))?;
        let mut raw_stream = stream;
        let mut stream = DeadlineStream {
            stream: &mut raw_stream,
            end,
        };
        // Once a byte is submitted the outcome may be unknown. Never restart or
        // replay an exchange here, including a write whose reply was lost.
        let result = (|| -> io::Result<_> {
            write_frame(&mut stream, &header, MAX_HEADER)?;
            write_frame(&mut stream, body, MAX_BODY)?;
            let header = read_frame(&mut stream, MAX_HEADER)?;
            let reply: Value = aterm_json::from_slice(&header)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let body = read_frame(&mut stream, MAX_BODY)?;
            Ok((reply, body))
        })()
        .map_err(|error| {
            let mut error = io_failure(error);
            error.message = format!(
                "Mail owner exchange interrupted; outcome is unknown: {}",
                error.message
            );
            error
        })?;
        let reply = result
            .0
            .as_object()
            .ok_or_else(|| failure(Kind::Protocol, "Invalid mail owner response"))?;
        if let Some(message) = reply.get("error") {
            let message = message
                .as_str()
                .ok_or_else(|| failure(Kind::Protocol, "Invalid mail owner error response"))?;
            return Err(ApiError {
                kind: decode_kind(result.0["kind"].as_str().unwrap_or("Protocol")),
                message: message.into(),
            });
        }
        Ok(result)
    }
}

impl ApiTransport for RemoteTransport {
    fn set_deadline(&mut self, end: Option<Instant>) {
        self.end = end;
    }

    fn request(
        &mut self,
        method: aterm_http::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<ApiResponse, ApiError> {
        let body = body
            .map(|body| aterm_json::to_vec(&body))
            .transpose()
            .map_err(|error| failure(Kind::Configuration, error.to_string()))?
            .unwrap_or_default();
        let (reply, body) = self.exchange(
            json!({"operation":"request", "method":method.as_str(), "path":path}),
            &body,
        )?;
        let status = reply["status"]
            .as_u64()
            .and_then(|status| u16::try_from(status).ok())
            .filter(|status| (100..=599).contains(status))
            .ok_or_else(|| failure(Kind::Protocol, "Invalid mail owner response"))?;
        let retry_after = match reply.get("retry_after") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            _ => return Err(failure(Kind::Protocol, "Invalid mail owner retry deadline")),
        };
        Ok(ApiResponse {
            status,
            retry_after,
            body: body.to_vec(),
        })
    }

    fn renew(&mut self) -> Result<(), ApiError> {
        self.exchange(json!({"operation":"renew"}), &[]).map(|_| ())
    }
}

pub(super) fn mailbox(
    home: &Path,
    credentials: Credentials<'_>,
    end: Option<Instant>,
) -> Result<Proton, MailboxOpenError> {
    let email = credentials.email.to_owned();
    let transport = RemoteTransport {
        home: home.into(),
        credentials: credentials.into(),
        end,
    };
    let (_, key) = transport
        .exchange(json!({"operation":"key"}), &[])
        .map_err(|error| error.message)?;
    let key = aterm_mail::Secret::new(
        String::from_utf8(key.to_vec()).map_err(|_| "Invalid mailbox key")?,
    );
    let mut mailbox = Proton::with_transport(Box::new(transport));
    let unlock = |mailbox: &mut Proton| {
        if !mailbox.has_address(&email)? {
            return Err("API session belongs to a different mailbox".into());
        }
        mailbox.unlock_mailbox_with_key_password(&key)
    };
    let opened = if let Some(end) = end {
        mailbox.run_until(end, unlock)
    } else {
        unlock(&mut mailbox)
    };
    if let Err(message) = opened {
        return Err(MailboxOpenError {
            message,
            proton_failure: mailbox.last_failure(),
        });
    }
    Ok(mailbox)
}

fn monotonic_ms() -> Result<u64, ApiError> {
    // All local processes share this boot-relative clock. Wall-clock adjustment
    // must not extend a queued write beyond its caller's original deadline.
    aterm_uds::process::awake_ms()
        .ok_or_else(|| failure(Kind::Configuration, "Mail operation clock unavailable"))
}

fn request_end(request: &Value, account: &str) -> Result<Instant, ApiError> {
    request_end_started(request, account, Instant::now())
}

fn request_end_started(
    request: &Value,
    account: &str,
    started: Instant,
) -> Result<Instant, ApiError> {
    if request["version"].as_u64() != Some(1) || request["account"].as_str() != Some(account) {
        return Err(ApiError {
            kind: Kind::Refused,
            message: "Mail owner account or protocol mismatch".into(),
        });
    }
    let left = request["expires"]
        .as_u64()
        .ok_or_else(|| failure(Kind::Protocol, "Invalid mail owner deadline"))?
        .saturating_sub(monotonic_ms()?);
    if left == 0 {
        return Err(failure(Kind::Timeout, "Mail operation deadline expired"));
    }
    let end = Instant::now()
        .checked_add(Duration::from_millis(left))
        .ok_or_else(|| failure(Kind::Protocol, "Invalid mail owner deadline"))?
        .min(started + OPERATION);
    remaining(end).map_err(io_failure)?;
    Ok(end)
}

fn decode_kind(kind: &str) -> Kind {
    match kind {
        "Network" => Kind::Network,
        "Timeout" => Kind::Timeout,
        "AccessDenied" => Kind::AccessDenied,
        "Configuration" => Kind::Configuration,
        "Credentials" => Kind::Credentials,
        "SessionExpired" => Kind::SessionExpired,
        "Verification" => Kind::Verification,
        "DeviceVerification" => Kind::DeviceVerification,
        "Scope" => Kind::Scope,
        "AccountRestricted" => Kind::AccountRestricted,
        "RateLimited" => Kind::RateLimited,
        "Service" => Kind::Service,
        "ClientRejected" => Kind::ClientRejected,
        "Refused" => Kind::Refused,
        _ => Kind::Protocol,
    }
}

struct Owner {
    home: PathBuf,
    credentials: StoredCredentials,
    transport: Option<BrowserTransport>,
}

impl Owner {
    fn transport(&mut self, end: Instant) -> Result<&mut BrowserTransport, ApiError> {
        if self.transport.is_none() {
            let session = HeadlessMail::connect_until(
                &self.home,
                self.credentials.borrowed(),
                |_| {},
                Some(end),
            )
            .map_err(super::native::transport_error)?;
            self.transport =
                Some(BrowserTransport::new(session).map_err(super::native::transport_error)?);
        }
        let transport = self
            .transport
            .as_mut()
            .ok_or_else(|| failure(Kind::Configuration, "Mail transport unavailable"))?;
        transport.set_deadline(Some(end));
        Ok(transport)
    }

    fn execute(
        &mut self,
        request: &Value,
        body: &[u8],
        end: Instant,
    ) -> Result<(Value, Zeroizing<Vec<u8>>), ApiError> {
        let transport = self.transport(end)?;
        match request["operation"].as_str() {
            Some("key") => {
                let key = transport
                    .session()
                    .and_then(HeadlessMail::mailbox_key_password)
                    .map_err(super::native::transport_error)?;
                Ok((json!({}), Zeroizing::new(key.expose().as_bytes().to_vec())))
            }
            Some("renew") => {
                transport.renew()?;
                Ok((json!({}), Zeroizing::new(Vec::new())))
            }
            Some("request") => {
                let method = match request["method"].as_str() {
                    Some("GET") => aterm_http::Method::Get,
                    Some("HEAD") => aterm_http::Method::Head,
                    Some("POST") => aterm_http::Method::Post,
                    Some("PATCH") => aterm_http::Method::Patch,
                    Some("PUT") => aterm_http::Method::Put,
                    Some("DELETE") => aterm_http::Method::Delete,
                    _ => return Err(failure(Kind::Protocol, "Invalid mail API method")),
                };
                let path = request["path"]
                    .as_str()
                    .ok_or_else(|| failure(Kind::Protocol, "Missing mail API path"))?;
                let body = if body.is_empty() {
                    None
                } else {
                    Some(
                        aterm_json::from_slice(body)
                            .map_err(|_| failure(Kind::Protocol, "Invalid mail API body"))?,
                    )
                };
                let response = transport.request(method, path, body)?;
                Ok((
                    json!({"status":response.status,"retry_after":response.retry_after}),
                    Zeroizing::new(response.body),
                ))
            }
            _ => Err(failure(Kind::Protocol, "Invalid mail owner operation")),
        }
    }
}

struct BoundSocket {
    endpoint: Endpoint,
    identity: aterm_uds::listener::ListenerId,
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        if aterm_uds::listener::names_now(&self.endpoint.socket, self.identity)
            == aterm_uds::listener::Named::Same
        {
            let _ = std::fs::remove_file(&self.endpoint.socket);
        }
    }
}

pub(super) fn serve(home: &Path, account: &str) -> Result<(), String> {
    serve_inner(home, account).map_err(|error| error.to_string())
}

fn serve_inner(home: &Path, account: &str) -> io::Result<()> {
    let bootstrap = read_frame(&mut std::io::stdin().lock(), MAX_HEADER)?;
    let value: Value = aterm_json::from_slice(&bootstrap)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let Value::Object(mut value) = value else {
        return Err(io::Error::other("Invalid mail owner bootstrap"));
    };
    let mut take = |field: &str| match value.remove(field) {
        Some(Value::String(text)) => Some(text),
        _ => None,
    };
    let email = take("email").ok_or_else(|| io::Error::other("No mailbox identity"))?;
    let login = take("login").ok_or_else(|| io::Error::other("No mailbox login"))?;
    let password =
        Zeroizing::new(take("password").ok_or_else(|| io::Error::other("No mailbox credential"))?);
    let mailbox_password = take("mailbox_password").map(Zeroizing::new);
    let totp_secret = take("totp_secret").map(Zeroizing::new);
    if email.trim().to_ascii_lowercase() != account {
        return Err(io::Error::other("Mail owner account mismatch"));
    }
    let credentials = Credentials {
        email: &email,
        login: &login,
        password: &password,
        mailbox_password: mailbox_password.as_deref().map(String::as_str),
        totp_secret: totp_secret.as_deref().map(String::as_str),
    }
    .into();
    let endpoint = Endpoint::new(home, account)?;
    let _owner = PrivateLock::take(
        &endpoint.dir,
        &format!("{}.owner", endpoint.key),
        Duration::ZERO,
        "Mail owner already exists",
    )?;
    // Only the exclusive owner removes a proven private stale endpoint. A live
    // owner holds this lock even while no caller is connected.
    if endpoint.metadata()?.is_some() {
        std::fs::remove_file(&endpoint.socket)?;
    }
    // Publish only after chmod, so a concurrent reader never sees the socket's
    // initial umask-derived permissions and mistakes startup for an unsafe peer.
    let pending = Endpoint {
        socket: endpoint.socket.with_extension("pending"),
        dir: endpoint.dir.clone(),
        key: endpoint.key.clone(),
    };
    match std::fs::symlink_metadata(&pending.socket) {
        Ok(metadata)
            if metadata.file_type().is_socket() && metadata.uid() == aterm_uds::peer::our_uid() =>
        {
            std::fs::remove_file(&pending.socket)?
        }
        Ok(_) => return Err(io::Error::other("Unsafe pending mail owner socket")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&pending.socket))?;
    let identity = aterm_uds::listener::listener_at(&pending.socket)
        .ok_or_else(|| io::Error::other("Mail owner socket disappeared"))?;
    let mut bound = BoundSocket {
        endpoint: pending,
        identity,
    };
    std::fs::set_permissions(
        &bound.endpoint.socket,
        std::fs::Permissions::from_mode(0o600),
    )?;
    std::fs::rename(&bound.endpoint.socket, &endpoint.socket)?;
    bound.endpoint = endpoint;
    let _bound = bound;
    let mut owner = Owner {
        home: home.into(),
        credentials,
        transport: None,
    };
    serve_connections(&listener, account, IDLE, |request, body, end| {
        owner.execute(request, body, end)
    })
}

fn serve_connections(
    listener: &CtlListener,
    account: &str,
    idle_for: Duration,
    mut execute: impl FnMut(&Value, &[u8], Instant) -> Result<(Value, Zeroizing<Vec<u8>>), ApiError>,
) -> io::Result<()> {
    let mut idle = Instant::now() + idle_for;
    while Instant::now() < idle {
        let mut stream = match aterm_uds::accept(
            listener,
            aterm_uds::AcceptWait::Until(idle),
            aterm_uds::AcceptedMode::Nonblocking,
        ) {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => break,
            Err(error) => return Err(error),
        };
        if private_peer(&stream).is_err() {
            continue;
        }
        // An abandoned or partial frame may occupy the admission step only briefly.
        let request = (|| -> io::Result<_> {
            let mut input = DeadlineStream {
                stream: &mut stream,
                end: Instant::now() + Duration::from_secs(2),
            };
            let bytes = read_frame(&mut input, MAX_HEADER)?;
            let request: Value = aterm_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let body = read_frame(&mut input, MAX_BODY)?;
            Ok((request, body))
        })();
        let Ok((request, body)) = request else {
            continue;
        };
        let end = request_end(&request, account);
        let reply_end = end
            .as_ref()
            .copied()
            .unwrap_or_else(|_| Instant::now() + Duration::from_secs(2));
        let result = end.and_then(|end| execute(&request, &body, end));
        // A later caller may have refreshed the registered password. Do not
        // retain rejected bootstrap credentials in a long-lived owner.
        let retire = result
            .as_ref()
            .is_err_and(|error| error.kind == Kind::Credentials);
        let (header, body) = match result {
            Ok(reply) => reply,
            Err(error) => (
                json!({"error":error.message,"kind":format!("{:?}",error.kind)}),
                Zeroizing::new(Vec::new()),
            ),
        };
        let bytes = aterm_json::to_vec(&header).map_err(io::Error::other)?;
        // A lost recipient does not terminate the shared browser or affect others.
        let mut output = DeadlineStream {
            stream: &mut stream,
            end: reply_end,
        };
        let _ = write_frame(&mut output, &bytes, MAX_HEADER)
            .and_then(|()| write_frame(&mut output, &body, MAX_BODY));
        if retire {
            break;
        }
        idle = Instant::now() + idle_for;
    }
    Ok(())
}

#[cfg(test)]
#[path = "owner/native_wait_tests.rs"]
mod native_wait_tests;

#[cfg(test)]
#[path = "owner/open_error_tests.rs"]
mod open_error_tests;

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
