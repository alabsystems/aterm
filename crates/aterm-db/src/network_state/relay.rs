// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only, same-user forwarding of the journal owner's current observation. Disk projections
//! are never accepted by this protocol as live evidence.

use super::*;
use aterm_uds::{CtlListener, CtlStream, listener, peer};
use std::{
    io::{self, Write},
    os::fd::AsFd,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    thread::{self, JoinHandle},
};

const NAME: &str = "observer.sock";
const REQUEST: &[u8] = b"aterm-observer/1\n";
const ACK: &[u8] = b"\x06";
#[cfg(not(test))]
const EXCHANGE: Duration = Duration::from_millis(250);
// Successful test exchanges use a hang detector, never a latency assertion.
// Deadline refusal itself is tested with an explicitly expired Instant.
#[cfg(test)]
const EXCHANGE: Duration = Duration::from_secs(60);

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "local observation unavailable")
}

#[derive(Debug)]
struct ReadFailure {
    step: &'static str,
    source: io::Error,
}

impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.step, self.source)
    }
}

impl std::error::Error for ReadFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// A locally assigned stage, never parsed from an error message or a peer's data.
pub(crate) fn error_stage(error: &io::Error) -> Option<&'static str> {
    error
        .get_ref()?
        .downcast_ref::<ReadFailure>()
        .map(|failure| failure.step)
}

fn context(step: &'static str, error: io::Error) -> io::Error {
    if error_stage(&error).is_some() {
        return error;
    }
    io::Error::new(
        error.kind(),
        ReadFailure {
            step,
            source: error,
        },
    )
}

fn refused_at(step: &'static str) -> io::Error {
    context(step, refused())
}

#[cfg(test)]
fn endpoint(dir: &Path) -> io::Result<listener::ListenerId> {
    endpoint_at(dir, &socket_path(dir, NAME)?)
}

/// Long configuration paths keep their history in place and use a short,
/// same-user socket name derived from the canonical history directory.
fn socket_path(dir: &Path, name: &str) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;

    private_file::check_private_dir(dir)?;
    let canonical = std::fs::canonicalize(dir)?;
    let direct = canonical.join(name);
    if std::os::unix::net::SocketAddr::from_pathname(&direct).is_ok() {
        return Ok(direct);
    }
    let digest = aterm_digest::Sha256::digest(canonical.as_os_str().as_bytes());
    // The short leaf preserves the socket's length budget after adding the quiet parent,
    // including a uid with all ten decimal digits.
    let short = aterm_tempfile::quiet_in(Path::new("/tmp"))
        .join(format!("ao{}", peer::our_uid()))
        .join(format!(
            "{}-{name}",
            aterm_codec::hex::encode_lower(&digest)
        ));
    std::os::unix::net::SocketAddr::from_pathname(&short)?;
    Ok(short)
}

fn endpoint_at(dir: &Path, path: &Path) -> io::Result<listener::ListenerId> {
    private_file::check_private_dir(dir)?;
    let parent = path.parent().ok_or_else(refused)?;
    if parent != dir {
        private_file::check_private_dir(parent)?;
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != peer::our_uid()
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(refused());
    }
    listener::ListenerId::from_metadata(&metadata).ok_or_else(refused)
}

/// The single worker bounds work even when a client stops reading or writing.
pub(super) struct Server {
    wake: Option<CtlStream>,
    task: Option<JoinHandle<()>>,
    dir: PathBuf,
    path: PathBuf,
    identity: listener::ListenerId,
}

struct BoundSocket {
    path: PathBuf,
    identity: listener::ListenerId,
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| listener::ListenerId::from_metadata(&m) == Some(self.identity))
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Nothing listens at the endpoint: it is not there, or its socket accepts no connection.
pub(super) fn stale(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    )
}

impl Server {
    pub(super) fn start(dir: &Path, source: ObservationHandle) -> io::Result<Self> {
        Self::start_named(dir, NAME, source)
    }

    fn start_named(dir: &Path, name: &'static str, source: ObservationHandle) -> io::Result<Self> {
        Self::start_at(dir, source, socket_path(dir, name)?)
    }

    fn start_at(dir: &Path, source: ObservationHandle, path: PathBuf) -> io::Result<Self> {
        private_file::check_private_dir(dir)?;
        let parent = path.parent().ok_or_else(refused)?;
        if parent != dir {
            private_file::create_private_dir(parent)?;
        }
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let previous = endpoint_at(dir, &path)?;
                // Never replace a listener which is still accepting connections.
                if !aterm_uds::connect::until(&path, Instant::now() + EXCHANGE)
                    .is_err_and(|error| stale(&error))
                    || endpoint_at(dir, &path)? != previous
                {
                    return Err(refused());
                }
                std::fs::remove_file(&path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let socket = aterm_uds::kept_out(|| CtlListener::bind(&path))?;
        let meta = std::fs::symlink_metadata(&path)?;
        let bound = BoundSocket {
            identity: listener::ListenerId::from_metadata(&meta).ok_or_else(refused)?,
            path: path.clone(),
        };
        if meta.mode() & 0o777 != 0o600 {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        if endpoint_at(dir, &path)? != bound.identity {
            return Err(refused());
        }
        let (wake_read, wake_write) = aterm_uds::kept_out(CtlStream::pair)?;
        let identity = bound.identity;
        let kept_path = path.clone();
        let task = thread::Builder::new()
            .name("network-observer-read".into())
            .spawn(move || {
                while let Ok(ready) = aterm_uds::ready::either(socket.as_fd(), wake_read.as_fd()) {
                    if ready.second
                        || listener::names_now(&path, bound.identity) != listener::Named::Same
                    {
                        break;
                    }
                    match aterm_uds::accept(
                        &socket,
                        aterm_uds::AcceptWait::Ready,
                        aterm_uds::AcceptedMode::Blocking,
                    ) {
                        Ok((mut client, _)) => {
                            if peer::peer_uid(&client) == Some(peer::our_uid()) {
                                let _ = serve(&mut client, &source);
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                            ) => {}
                        Err(_) => break,
                    }
                }
                drop(socket);
                drop(bound);
            })?;
        Ok(Self {
            wake: Some(wake_write),
            task: Some(task),
            dir: dir.to_owned(),
            path: kept_path,
            identity,
        })
    }

    pub(super) fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
            && endpoint_at(&self.dir, &self.path).is_ok_and(|identity| identity == self.identity)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.wake.take());
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "observation deadline"))
}

fn read_exact(stream: &mut CtlStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        match aterm_uds::hangup::read_until(stream, bytes, deadline) {
            Ok(0) => return Err(refused()),
            Ok(n) => bytes = &mut bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(context("read", error)),
        }
    }
    Ok(())
}

fn write_all(stream: &mut CtlStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|error| context("write-timeout", error))?;
        match stream.write(bytes) {
            Ok(0) => return Err(refused()),
            Ok(n) => bytes = &bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(context("write", error)),
        }
    }
    Ok(())
}

fn serve(client: &mut CtlStream, source: &ObservationHandle) -> io::Result<()> {
    let deadline = Instant::now() + EXCHANGE;
    let mut request = [0; REQUEST.len()];
    read_exact(client, &mut request, deadline)?;
    if request != REQUEST {
        return Err(refused());
    }
    let publication = source
        .publication_if_fresh(REACHABLE, Instant::now())
        .ok_or_else(refused)?;
    if publication.observation.via_local_process.is_some() {
        return Err(refused());
    }
    let age = Instant::now()
        .checked_duration_since(publication.at)
        .ok_or_else(refused)?;
    let age_ms = age_millis(age)?;
    if age_ms > aterm_types::network_state::REACHABLE_MS {
        return Err(refused());
    }
    let wire = live_encode_publication(&publication, age_ms)?;
    let mut frame = Vec::with_capacity(4 + wire.len());
    frame.extend_from_slice(&(wire.len() as u32).to_be_bytes());
    frame.extend_from_slice(&wire);
    write_all(client, &frame, deadline)?;
    // Keep the endpoint alive through the reader's kernel identity checks.
    // Darwin refuses timeout updates once the peer has closed its socket.
    let mut ack = [0; ACK.len()];
    read_exact(client, &mut ack, deadline)?;
    if ack != ACK {
        return Err(refused());
    }
    Ok(())
}

#[cfg(test)]
fn read(config: &Path) -> io::Result<NetworkObservation> {
    read_publication(config)
        .map(|publication| publication.current_at(Instant::now(), aterm_time::unix_millis()))
}

/// The live observation of `config`'s Commons from the journal owner, another local process.
pub(crate) fn read_publication(config: &Path) -> io::Result<Publication> {
    read_named(config, NAME)
}

/// Read the transport owner's relay in its leased Commons directory.
pub(super) fn read_owner_at(dir: &Path) -> io::Result<Publication> {
    read_in(dir, NAME)
}

fn read_named(config: &Path, name: &str) -> io::Result<Publication> {
    read_in(&directory(config), name)
}

fn read_in(dir: &Path, name: &str) -> io::Result<Publication> {
    let requested = Instant::now();
    let deadline = requested + EXCHANGE;
    let path = socket_path(dir, name).map_err(|error| context("endpoint", error))?;
    let result = read_at(dir, &path, requested, deadline);
    let legacy = dir.join(name);
    if result.as_ref().is_err_and(stale)
        && legacy != path
        && std::os::unix::net::SocketAddr::from_pathname(&legacy).is_ok()
    {
        // An older owner may have bound through a short alias of this directory.
        return read_at(dir, &legacy, requested, deadline);
    }
    result
}

fn read_at(
    dir: &Path,
    path: &Path,
    requested: Instant,
    deadline: Instant,
) -> io::Result<Publication> {
    let bound = endpoint_at(dir, path).map_err(|error| context("endpoint", error))?;
    let mut client =
        aterm_uds::connect::until(path, deadline).map_err(|error| context("connect", error))?;
    if peer::peer_uid(&client) != Some(peer::our_uid()) {
        return Err(refused_at("peer-user"));
    }
    write_all(&mut client, REQUEST, deadline).map_err(|error| context("request", error))?;
    let mut size = [0; 4];
    read_exact(&mut client, &mut size, deadline).map_err(|error| context("frame-header", error))?;
    let pid = aterm_uds::fdpass::peer_pid(&client).map_err(|error| context("peer-pid", error))?;
    let native_pid = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| refused_at("peer-pid"))?;
    let birth =
        aterm_uds::process::read_birth(native_pid).ok_or_else(|| refused_at("peer-process"))?;
    let size = u32::from_be_bytes(size) as usize;
    if size == 0 || size > MAX_BYTES {
        return Err(refused_at("frame-size"));
    }
    let mut wire = vec![0; size];
    read_exact(&mut client, &mut wire, deadline).map_err(|error| context("frame-body", error))?;
    let now = aterm_time::unix_millis().ok_or_else(|| refused_at("clock"))?;
    let mut publication =
        decode_publication(&wire, now, requested).map_err(|error| context("snapshot", error))?;
    if aterm_uds::process::read_birth(native_pid) != Some(birth)
        || endpoint_at(dir, path).map_err(|error| context("endpoint-recheck", error))? != bound
    {
        return Err(refused_at("endpoint-changed"));
    }
    remaining(deadline).map_err(|error| context("deadline", error))?;
    if Instant::now()
        .checked_duration_since(publication.at)
        .is_none_or(|age| age > REACHABLE)
    {
        return Err(refused_at("sample-stale"));
    }
    write_all(&mut client, ACK, deadline).map_err(|error| context("acknowledgement", error))?;
    publication.observation.via_local_process = Some(pid);
    Ok(publication)
}

fn age_millis(age: Duration) -> io::Result<u64> {
    u64::try_from(age.as_nanos().div_ceil(1_000_000)).map_err(|_| refused())
}

#[cfg(test)]
fn live_encode(state: &NetworkObservation, age_ms: u64) -> io::Result<Vec<u8>> {
    let mut publication = Publication::new(state.clone(), Instant::now());
    publication.contact_ages = state
        .peers
        .iter()
        .map(|peer| {
            (
                peer.slot,
                Duration::from_millis(state.observed_at_ms.saturating_sub(peer.last_seen_ms)),
            )
        })
        .collect();
    live_encode_publication(&publication, age_ms)
}

fn live_encode_publication(publication: &Publication, age_ms: u64) -> io::Result<Vec<u8>> {
    let state = &publication.observation;
    let history = encode_value(state, None);
    // A Reachable peer's contact age comes with the lease it is read against (`Contact::lease`);
    // a row with no lease is read against `REACHABLE`.
    let live = state.peers.iter().map(|peer| {
        let reachable = peer.reachability == Reachability::Reachable;
        let age = if reachable {
            publication.contact_ages.get(&peer.slot).copied().map(age_millis).transpose()?
        } else {
            None
        };
        let lease = peer.lease_ms.filter(|_| reachable);
        Ok(json!({"slot":peer.slot,"state":state_value(peer.reachability),"checked":peer.last_checked_ms,"contact_age_ms":age,"lease_ms":lease}))
    }).collect::<io::Result<Vec<_>>>()?;
    let wire = aterm_json::to_vec(&json!({
        "version":1, "source_age_ms":age_ms, "history":history, "visibility":visibility(state.visibility),
        "history_state":match state.history {HistoryState::Ready=>"ready",HistoryState::Restoring=>"restoring",HistoryState::Unavailable=>"unavailable"},
        "detail":state.detail, "unverified":state.unverified_endpoints,
        "live":live
    })).map_err(|_| refused())?;
    if wire.len() > MAX_BYTES {
        return Err(refused());
    }
    Ok(wire)
}

fn visibility(state: Visibility) -> &'static str {
    match state {
        Visibility::Checking => "checking",
        Visibility::Active => "active",
        Visibility::Offline => "offline",
        Visibility::Disabled => "disabled",
        Visibility::Waiting => "waiting",
        Visibility::Unavailable => "unavailable",
    }
}

#[cfg(test)]
fn live_decode(wire: &[u8], now: u64) -> io::Result<NetworkObservation> {
    live_decode_parts(wire, now).map(|(observation, _, _)| observation)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRelayHistory",
        action = "Read",
        project = "aterm_db::network_state::relay::history_conformance::relay_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbRelayHistory",
        action = "Case",
        reason = "The native fixture chooses retained history, current clocks, source sample age and contact lease"
    )
)]
fn decode_publication(wire: &[u8], now: u64, requested: Instant) -> io::Result<Publication> {
    let (observation, age_ms, contact_ages) = live_decode_parts(wire, now)?;
    let at = requested
        .checked_sub(Duration::from_millis(age_ms))
        .ok_or_else(|| refused_at("monotonic-clock"))?;
    Ok(Publication {
        observation,
        at,
        contact_ages,
    })
}

fn live_decode_parts(
    wire: &[u8],
    now: u64,
) -> io::Result<(NetworkObservation, u64, BTreeMap<u64, Duration>)> {
    if wire.len() > MAX_BYTES {
        return Err(refused_at("frame-size"));
    }
    if now == 0 {
        return Err(refused_at("clock"));
    }
    let value: Value = aterm_json::from_slice(wire).map_err(|_| refused_at("snapshot-json"))?;
    if number(&value, "version").map_err(|_| refused_at("snapshot-version"))? != 1 {
        return Err(refused_at("snapshot-version"));
    }
    let age_ms = number(&value, "source_age_ms").map_err(|_| refused_at("snapshot-age"))?;
    if age_ms > aterm_types::network_state::REACHABLE_MS {
        return Err(refused_at("source-stale"));
    }
    let history = value
        .get("history")
        .ok_or_else(|| refused_at("snapshot-history"))?;
    // History decoding validates bounded identities and samples but deliberately
    // resets current evidence. Only this live exchange can supply it again.
    let mut observation = decode_value(history).map_err(|_| refused_at("snapshot-history"))?;
    if observation.observed_at_ms == 0 || observation.observed_at_ms > now {
        return Err(refused_at("sample-clock"));
    }
    if now - observation.observed_at_ms > REACHABLE.as_millis() as u64 {
        return Err(refused_at("sample-stale"));
    }
    observation.visibility = match value.get("visibility").and_then(Value::as_str) {
        Some("checking") => Visibility::Checking,
        Some("active") => Visibility::Active,
        Some("offline") => Visibility::Offline,
        Some("disabled") => Visibility::Disabled,
        Some("waiting") => Visibility::Waiting,
        Some("unavailable") => Visibility::Unavailable,
        _ => return Err(refused_at("snapshot-visibility")),
    };
    observation.history = match value.get("history_state").and_then(Value::as_str) {
        Some("ready") => HistoryState::Ready,
        Some("restoring") => HistoryState::Restoring,
        Some("unavailable") => HistoryState::Unavailable,
        _ => return Err(refused_at("snapshot-history-state")),
    };
    let detail = value
        .get("detail")
        .and_then(Value::as_str)
        .filter(|text| text.len() <= 2048)
        .ok_or_else(|| refused_at("snapshot-detail"))?;
    observation.detail = detail.into();
    observation.unverified_endpoints =
        usize::try_from(number(&value, "unverified").map_err(|_| refused_at("snapshot-hints"))?)
            .map_err(|_| refused_at("snapshot-hints"))?;
    if observation.unverified_endpoints > crate::runtime::MAX_UNVERIFIED_ENDPOINTS {
        return Err(refused_at("snapshot-hints"));
    }
    let live = value
        .get("live")
        .and_then(Value::as_array)
        .ok_or_else(|| refused_at("snapshot-peers"))?;
    if live.len() != observation.peers.len() {
        return Err(refused_at("snapshot-peers"));
    }
    let mut slots = std::collections::BTreeSet::new();
    let mut contact_ages = BTreeMap::new();
    for row in live {
        let slot = number(row, "slot").map_err(|_| refused_at("snapshot-slots"))?;
        if !slots.insert(slot) {
            return Err(refused_at("snapshot-slots"));
        }
        let peer_idx = observation
            .peers
            .binary_search_by_key(&slot, |p| p.slot)
            .map_err(|_| refused_at("snapshot-slots"))?;
        let peer = &mut observation.peers[peer_idx];
        peer.reachability = state(row.get("state").ok_or_else(|| refused_at("peer-state"))?)
            .map_err(|_| refused_at("peer-state"))?;
        // The lease a contact is read against: the producer's measured one, within the
        // bounds every producer keeps, or `REACHABLE` where the row names none.
        let named = match row.get("lease_ms") {
            None => None,
            Some(value) if value.is_null() => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .filter(|lease| {
                        (REACHABLE..=LEASE_MAX).contains(&Duration::from_millis(*lease))
                    })
                    .ok_or_else(|| refused_at("peer-lease"))?,
            ),
        };
        let lease = named.map_or(REACHABLE, Duration::from_millis);
        let contact_age = match row.get("contact_age_ms") {
            Some(value) if value.is_null() => None,
            Some(value) => {
                let age = value
                    .as_u64()
                    .map(Duration::from_millis)
                    .filter(|age| *age <= lease)
                    .ok_or_else(|| refused_at("peer-contact-age"))?;
                Some(age)
            }
            None => None,
        };
        if peer.reachability == Reachability::Reachable {
            let age = contact_age.ok_or_else(|| refused_at("peer-contact-age"))?;
            contact_ages.insert(slot, age);
            peer.lease_ms = named;
        }
        peer.last_checked_ms = match row.get("checked") {
            Some(value) if value.is_null() => None,
            Some(value) => Some(value.as_u64().ok_or_else(|| refused_at("peer-check"))?),
            None => return Err(refused_at("peer-check")),
        };
        // Retained contacts and samples stay in observation order across wall-clock rollback.
        // Only a claim of current reachability needs current contact/check timestamps.
        if peer.reachability == Reachability::Reachable
            && (peer.last_seen_ms > now || peer.last_checked_ms.is_some_and(|at| at > now))
        {
            return Err(refused_at("peer-clock"));
        }
        // A contact may expire while a fresh publication is in transit.
        // `Publication::current_at` ages it without discarding the other peers.
        if peer.reachability == Reachability::Reachable
            && (observation.visibility != Visibility::Active || peer.last_checked_ms.is_none())
        {
            return Err(refused_at("peer-evidence"));
        }
    }
    Ok((observation, age_ms, contact_ages))
}

#[cfg(test)]
#[path = "relay_history_conformance.rs"]
mod history_conformance;

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
