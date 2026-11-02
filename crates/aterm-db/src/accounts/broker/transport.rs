// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private local request transport. Token issuance is deliberately absent.
//! The embedding host owns scheduling and must bound its trusted adapter's I/O.

use super::{Broker, Token};
use crate::resources::{BoundRequest, Observation, ObservationState, Outcome};
use aterm_uds::{CtlListener, CtlStream, listener, peer, private_file, secret::Secret};
use std::{
    io::{self, Read, Write},
    net::Shutdown,
    os::fd::AsFd,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

const NAME: &str = "broker.sock";
const REQUEST: &[u8; 8] = b"ATBRQ01\n";
const RESPONSE: &[u8; 8] = b"ATBRS01\n";
const MAX_BODY: usize = 1024 * 1024;
const MAX_TIME: Duration = Duration::from_secs(60);
const HEADER: usize = 65;

impl Token {
    /// Prepare a one-use private delivery socket, ready to pass to a trusted
    /// child through `spawnfd`. Only token bytes and EOF cross this descriptor;
    /// never put them in arguments, environment variables or a file.
    /// # Errors
    /// Socket creation, deadline or I/O failure. The lease is not revoked on
    /// delivery failure; trusted control can explicitly revoke or rotate it.
    pub fn delivery_until(&self, deadline: Instant) -> io::Result<CtlStream> {
        let (read, mut write) = aterm_uds::kept_out(CtlStream::pair)?;
        write.set_nonblocking(true)?;
        write_all(&mut write, self.bytes(), cap_deadline(deadline))?;
        write.shutdown(Shutdown::Write)?;
        Ok(read)
    }
}

/// Receive an exclusively owned private delivery socket from trusted launch
/// control. A same-user peer is necessary but does not authenticate the issuer;
/// the broker still authenticates the bytes on every request.
/// # Errors
/// Peer, framing, expiry or I/O errors. Exactly 32 bytes followed by EOF are
/// required; all temporary token storage is scrubbed on drop.
pub fn receive_token_until(mut stream: CtlStream, deadline: Instant) -> io::Result<Secret> {
    same_user(&stream)?;
    stream.set_nonblocking(true)?;
    let deadline = cap_deadline(deadline);
    let mut token = Zeroizing::new([0u8; 32]);
    read_exact(&mut stream, token.as_mut(), deadline)?;
    read_end(&mut stream, deadline)?;
    Ok(Secret::new(token.as_ref()))
}

/// A listener with no background workers. Existing paths are never replaced.
/// A crash can leave its socket behind; the host can use a fresh private directory.
pub struct Endpoint {
    socket: CtlListener,
    path: PathBuf,
    identity: listener::ListenerId,
}

impl Endpoint {
    /// Bind `broker.sock` in an existing owner-only directory. No TCP listener
    /// or token-minting request is available on this interface.
    ///
    /// # Errors
    /// An untrusted directory, existing path, bind, mode, or identity error.
    pub fn bind(dir: &Path) -> io::Result<Self> {
        private_file::check_private_dir(dir)?;
        let path = dir.join(NAME);
        let socket = aterm_uds::kept_out(|| CtlListener::bind(&path))?;
        let endpoint = Self {
            identity: own_socket(&std::fs::symlink_metadata(&path)?)?,
            socket,
            path,
        };
        std::fs::set_permissions(&endpoint.path, std::fs::Permissions::from_mode(0o600))?;
        endpoint.socket.set_nonblocking(true)?;
        endpoint.current()?;
        Ok(endpoint)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn current(&self) -> io::Result<()> {
        if socket_identity(self.path.parent().ok_or_else(refused)?)? != self.identity {
            return Err(refused());
        }
        Ok(())
    }

    /// Accept at most one same-user connection without waiting. The host limits
    /// concurrent handlers; this method never creates a thread or queue.
    ///
    /// # Errors
    /// Changed socket/directory identity, peer credentials, or accept/mode errors.
    pub fn try_accept(&self) -> io::Result<Option<CtlStream>> {
        loop {
            self.current()?;
            match aterm_uds::kept_out(|| self.socket.accept()) {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    same_user(&stream)?;
                    self.current()?;
                    return Ok(Some(stream));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| own_socket(&metadata).is_ok_and(|id| id == self.identity))
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Caller-owned input. The token is never serialized to disk or into an error.
pub struct Call<'a> {
    /// Bytes delivered privately by trusted control, for example in a `Secret`
    /// read from an inherited descriptor. The server authenticates them.
    pub token: &'a [u8],
    pub logical: [u8; 16],
    pub adapter: &'a str,
    pub payload: &'a [u8],
}

/// Admission/result knowledge, never permission to use a new id for a retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    /// No admission evidence returned; an earlier call may still have executed.
    Refused = 0,
    /// An attempt exists but its final provider outcome is unknown.
    Unknown = 1,
    Completed = 2,
    Rejected = 3,
    /// This call has adapter evidence whose durable recording was unconfirmed.
    Unconfirmed = 4,
}

/// A bounded reply. Retained attempts have no cached provider body. Bodies are
/// scrubbed on drop and are not stored in the replicated accounting log.
#[derive(Debug)]
pub struct Reply {
    pub status: Status,
    /// Whether this particular exchange entered the adapter, not an earlier one.
    pub entered: bool,
    pub body: Secret,
}

/// Trusted adapter output. The deadline supplied to the callback must cover all
/// provider work; transport cannot interrupt an arbitrary in-process callback.
pub struct AdapterReply {
    body: Secret,
    observation: Option<Observation>,
}

impl AdapterReply {
    /// Copy response bytes; the adapter remains responsible for scrubbing its
    /// source buffer. Prefer `from_secret` when the adapter owns that buffer.
    /// # Errors
    /// A body exceeding the protocol's one-MiB cap.
    pub fn new(body: &[u8], observation: Option<Observation>) -> io::Result<Self> {
        if body.len() > MAX_BODY {
            return Err(refused());
        }
        Ok(Self {
            body: Secret::new(body),
            observation,
        })
    }

    /// Transfer an already protected response without making another copy.
    /// # Errors
    /// A body exceeding the protocol's one-MiB cap.
    pub fn from_secret(body: Secret, observation: Option<Observation>) -> io::Result<Self> {
        if body.len() > MAX_BODY {
            return Err(refused());
        }
        Ok(Self { body, observation })
    }
}

/// Submit one complete request, half-close its write side, and read one reply.
/// Callers retain the same logical id after every ambiguous transport failure.
/// Local private ownership and kernel peer credentials are checked before the
/// token is sent. This does not isolate mutually hostile processes of one uid.
///
/// # Errors
/// Privacy, framing, connection, deadline or I/O errors. An error says nothing
/// about whether an earlier or this request reached the provider.
pub fn call_until(dir: &Path, call: Call<'_>, deadline: Instant) -> io::Result<Reply> {
    let deadline = cap_deadline(deadline);
    let identity = socket_identity(dir)?;
    let mut stream = aterm_uds::connect::until(&dir.join(NAME), deadline)?;
    same_user(&stream)?;
    if socket_identity(dir)? != identity {
        return Err(refused());
    }
    stream.set_nonblocking(true)?;
    let millis = remaining(deadline)?.as_millis().min(MAX_TIME.as_millis()) as u32;
    if millis == 0 {
        return Err(timed_out());
    }
    let header = request_header(call.token, call.logical, call.adapter, call.payload, millis)?;
    write_all(&mut stream, header.as_ref(), deadline)?;
    write_all(&mut stream, call.adapter.as_bytes(), deadline)?;
    write_all(&mut stream, call.payload, deadline)?;
    stream.shutdown(Shutdown::Write)?;
    read_reply(&mut stream, deadline)
}

/// Serve exactly one accepted connection, checking same-user credentials again.
/// All framing, EOF, and deadline checks precede the broker's durable admission.
/// The host supplies the actual adapter; a request cannot name an executable,
/// mint a token, select a producer, or receive a provider credential.
///
/// # Errors
/// Peer, framing, deadline or I/O errors, including loss of a reply after an
/// admitted effect. Adapter panics propagate and do not refund durable charges.
pub fn serve_until(
    stream: CtlStream,
    broker: &Broker,
    deadline: Instant,
    run: impl FnOnce(&BoundRequest, &[u8], Instant) -> Option<AdapterReply>,
) -> io::Result<()> {
    serve_guarded_until(stream, broker, deadline, |_| true, |_| true, run)
}

/// Host-only final entry gate, after durable billing and before provider code.
pub(super) fn serve_guarded_until(
    stream: CtlStream,
    broker: &Broker,
    deadline: Instant,
    admit: impl Fn(Instant) -> bool,
    permit: impl FnOnce(Instant) -> bool,
    run: impl FnOnce(&BoundRequest, &[u8], Instant) -> Option<AdapterReply>,
) -> io::Result<()> {
    serve_with_until(stream, broker, deadline, read_end, admit, permit, run)
}

pub(super) fn serve_prepared_until<T>(
    stream: CtlStream,
    broker: &Broker,
    deadline: Instant,
    gates: (impl Fn(Instant) -> bool, impl FnOnce(Instant) -> bool),
    prepare: impl FnOnce(&str, &[u8], Instant) -> io::Result<T>,
    run: impl FnOnce(T, &BoundRequest, &[u8], Instant) -> Option<AdapterReply>,
) -> io::Result<()> {
    serve_preparing_until(stream, broker, deadline, read_end, gates, prepare, run)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "Trailing",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "Fresh",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "ShortHeader",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "ShortBody",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "Oversize",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "Version",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "Expired",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "WrongToken",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "WrongAdapter",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "LostReply",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "Retry",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbBrokerTransport",
        action = "NoEof",
        project = "aterm_db::accounts::billing::observation::tests::transport_transition"
    )
)]
fn serve_with_until(
    stream: CtlStream,
    broker: &Broker,
    deadline: Instant,
    finish: impl FnOnce(&mut CtlStream, Instant) -> io::Result<()>,
    admit: impl Fn(Instant) -> bool,
    permit: impl FnOnce(Instant) -> bool,
    run: impl FnOnce(&BoundRequest, &[u8], Instant) -> Option<AdapterReply>,
) -> io::Result<()> {
    serve_preparing_until(
        stream,
        broker,
        deadline,
        finish,
        (admit, permit),
        |_, _, _| Ok(()),
        |(), request, credential, until| run(request, credential, until),
    )
}

fn serve_preparing_until<T>(
    mut stream: CtlStream,
    broker: &Broker,
    deadline: Instant,
    finish: impl FnOnce(&mut CtlStream, Instant) -> io::Result<()>,
    gates: (impl Fn(Instant) -> bool, impl FnOnce(Instant) -> bool),
    prepare: impl FnOnce(&str, &[u8], Instant) -> io::Result<T>,
    run: impl FnOnce(T, &BoundRequest, &[u8], Instant) -> Option<AdapterReply>,
) -> io::Result<()> {
    let (admit, permit) = gates;
    same_user(&stream)?;
    stream.set_nonblocking(true)?;
    let start = Instant::now();
    let deadline = deadline.min(start + MAX_TIME);
    let mut header = Zeroizing::new([0u8; HEADER]);
    read_exact(&mut stream, header.as_mut(), deadline)?;
    let request = decode_header(header.as_ref())?;
    let deadline = deadline.min(start + Duration::from_millis(u64::from(request.millis)));
    let mut adapter = [0u8; 64];
    read_exact(&mut stream, &mut adapter[..request.adapter], deadline)?;
    let adapter = std::str::from_utf8(&adapter[..request.adapter]).map_err(|_| refused())?;
    if !crate::crypto::name(adapter) {
        return Err(refused());
    }
    let mut payload = Zeroizing::new(vec![0; request.payload]);
    read_exact(&mut stream, &mut payload, deadline)?;
    finish(&mut stream, deadline)?;
    remaining(deadline)?;
    // Local shutdown can make a still-open peer look like EOF. A host closes
    // admission before shutting down sockets, so that synthetic EOF cannot
    // turn an abandoned frame into a new durable charge.
    if !admit(deadline) {
        return Err(refused());
    }
    let mut body = Secret::new(&[]);
    let mut entered = false;
    let dispatch = broker.dispatch_prepared_until(
        adapter,
        &header[8..40],
        (request.logical, &payload),
        deadline,
        |bytes, until| {
            let prepared = prepare(adapter, bytes, until)
                .map_err(|_| crate::Error::new("provider preparation refused"))?;
            if !admit(until) {
                return Err(crate::Error::new("provider preparation stopped"));
            }
            Ok(prepared)
        },
        |prepared, bound, credential| {
            if !permit(deadline) {
                return None;
            }
            entered = true;
            let reply = run(prepared, bound, credential, deadline)?;
            body = reply.body;
            reply.observation
        },
    );
    let reply = match dispatch {
        Ok(dispatch) => Reply {
            status: match dispatch.accounting.attempt.observation {
                ObservationState::Unknown => Status::Unknown,
                ObservationState::Unconfirmed { .. } => Status::Unconfirmed,
                ObservationState::Recorded(observation) => match observation.outcome() {
                    Outcome::Completed => Status::Completed,
                    Outcome::Rejected => Status::Rejected,
                },
            },
            entered,
            body,
        },
        Err(_) => Reply {
            status: Status::Refused,
            entered: false,
            body: Secret::new(&[]),
        },
    };
    write_reply(&mut stream, &reply, deadline)
}

struct Header {
    logical: [u8; 16],
    millis: u32,
    adapter: usize,
    payload: usize,
}

fn request_header(
    token: &[u8],
    logical: [u8; 16],
    adapter: &str,
    payload: &[u8],
    millis: u32,
) -> io::Result<Zeroizing<[u8; HEADER]>> {
    if token.len() != 32
        || adapter.len() > 64
        || !crate::crypto::name(adapter)
        || payload.len() > MAX_BODY
    {
        return Err(refused());
    }
    let mut header = Zeroizing::new([0u8; HEADER]);
    header[..8].copy_from_slice(REQUEST);
    header[8..40].copy_from_slice(token);
    header[40..56].copy_from_slice(&logical);
    header[56..60].copy_from_slice(&millis.to_be_bytes());
    header[60] = adapter.len() as u8;
    header[61..65].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    decode_header(header.as_ref())?;
    Ok(header)
}

fn decode_header(bytes: &[u8]) -> io::Result<Header> {
    if bytes.len() != HEADER || &bytes[..8] != REQUEST {
        return Err(refused());
    }
    let header = Header {
        logical: bytes[40..56].try_into().map_err(|_| refused())?,
        millis: u32::from_be_bytes(bytes[56..60].try_into().map_err(|_| refused())?),
        adapter: bytes[60] as usize,
        payload: u32::from_be_bytes(bytes[61..65].try_into().map_err(|_| refused())?) as usize,
    };
    if header.logical == [0; 16]
        || header.millis == 0
        || u128::from(header.millis) > MAX_TIME.as_millis()
        || !(1..=64).contains(&header.adapter)
        || header.payload > MAX_BODY
    {
        return Err(refused());
    }
    Ok(header)
}

fn write_reply(stream: &mut CtlStream, reply: &Reply, deadline: Instant) -> io::Result<()> {
    let mut header = [0u8; 14];
    header[..8].copy_from_slice(RESPONSE);
    header[8] = reply.status as u8;
    header[9] = u8::from(reply.entered);
    header[10..14].copy_from_slice(&(reply.body.len() as u32).to_be_bytes());
    write_all(stream, &header, deadline)?;
    write_all(stream, reply.body.expose(), deadline)?;
    // Host retains a cancellation clone until its next poll. Signal exact
    // reply completion now, independently of the host's reaping cadence.
    stream.shutdown(Shutdown::Write)
}

fn read_reply(stream: &mut CtlStream, deadline: Instant) -> io::Result<Reply> {
    let mut header = [0u8; 14];
    read_exact(stream, &mut header, deadline)?;
    if &header[..8] != RESPONSE || header[9] > 1 {
        return Err(refused());
    }
    let status = match header[8] {
        0 => Status::Refused,
        1 => Status::Unknown,
        2 => Status::Completed,
        3 => Status::Rejected,
        4 => Status::Unconfirmed,
        _ => return Err(refused()),
    };
    let size = u32::from_be_bytes(header[10..14].try_into().map_err(|_| refused())?) as usize;
    if size > MAX_BODY
        || (status == Status::Refused && header[9] != 0)
        || (header[9] == 0 && (size != 0 || status == Status::Unconfirmed))
    {
        return Err(refused());
    }
    let mut body = Zeroizing::new(vec![0; size]);
    read_exact(stream, &mut body, deadline)?;
    read_end(stream, deadline)?;
    Ok(Reply {
        status,
        entered: header[9] == 1,
        body: Secret::new(&body),
    })
}

fn socket_identity(dir: &Path) -> io::Result<listener::ListenerId> {
    private_file::check_private_dir(dir)?;
    let path = dir.join(NAME);
    let metadata = std::fs::symlink_metadata(&path)?;
    if metadata.mode() & 0o777 != 0o600 {
        return Err(refused());
    }
    own_socket(&metadata)
}

fn own_socket(metadata: &std::fs::Metadata) -> io::Result<listener::ListenerId> {
    if !metadata.file_type().is_socket()
        || metadata.uid() != peer::our_uid()
        || metadata.nlink() != 1
    {
        return Err(refused());
    }
    listener::ListenerId::from_metadata(metadata).ok_or_else(refused)
}

fn same_user(stream: &CtlStream) -> io::Result<()> {
    same_user_as(stream, peer::our_uid())
}

fn same_user_as(stream: &CtlStream, expected: u32) -> io::Result<()> {
    if peer::peer_uid(stream) != Some(expected) {
        return Err(refused());
    }
    Ok(())
}

fn cap_deadline(deadline: Instant) -> Instant {
    deadline.min(Instant::now() + MAX_TIME)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|n| !n.is_zero())
        .ok_or_else(timed_out)
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "broker exchange deadline")
}

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "broker exchange refused")
}

fn io_at(step: &'static str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("broker {step}: {error}"))
}

fn read_exact(stream: &mut CtlStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    remaining(deadline)?;
    while !bytes.is_empty() {
        remaining(deadline)?;
        match stream.read(bytes) {
            Ok(0) => return Err(refused()),
            Ok(n) => bytes = &mut bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                aterm_uds::ready::until(
                    stream.as_fd(),
                    aterm_uds::ready::Interest::Read,
                    deadline,
                )?;
            }
            Err(error) => return Err(io_at("read", error)),
        }
    }
    Ok(())
}

fn read_end(stream: &mut CtlStream, deadline: Instant) -> io::Result<()> {
    loop {
        remaining(deadline)?;
        match stream.read(&mut [0]) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(refused()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                aterm_uds::ready::until(
                    stream.as_fd(),
                    aterm_uds::ready::Interest::Read,
                    deadline,
                )?;
            }
            Err(error) => return Err(io_at("end read", error)),
        }
    }
}

fn write_all(stream: &mut CtlStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    remaining(deadline)?;
    while !bytes.is_empty() {
        remaining(deadline)?;
        match stream.write(bytes) {
            Ok(0) => return Err(refused()),
            Ok(n) => bytes = &bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                aterm_uds::ready::until(
                    stream.as_fd(),
                    aterm_uds::ready::Interest::Write,
                    deadline,
                )?;
            }
            Err(error) => return Err(io_at("write", error)),
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
