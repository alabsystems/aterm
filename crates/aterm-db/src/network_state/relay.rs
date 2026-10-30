// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only, same-user forwarding of the journal owner's current observation, and on a socket of
//! its own ([`WAIT`]) of a held owner's wait: an owner that opened no journal yet, held before its
//! first LAN use or by a disabled Commons, so a reader hears why. Readers prefer the journal
//! owner's. Disk projections are never accepted by this protocol as live evidence.

use super::*;
use aterm_uds::{CtlListener, CtlStream, listener, peer};
use std::{
    io::{self, Read, Write},
    os::fd::AsFd,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    thread::{self, JoinHandle},
};

const NAME: &str = "observer.sock";
/// The socket a held owner serves its wait on, apart from [`NAME`], so it never stands in the way
/// of the journal owner's relay.
const WAIT: &str = "waiting.sock";
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
    endpoint_named(dir, NAME)
}

fn endpoint_named(dir: &Path, name: &str) -> io::Result<listener::ListenerId> {
    private_file::check_private_dir(dir)?;
    let path = dir.join(name);
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != peer::our_uid()
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(refused());
    }
    listener::listener_read(path)?.ok_or_else(refused)
}

/// The single worker bounds work even when a client stops reading or writing.
pub(super) struct Server {
    wake: Option<CtlStream>,
    task: Option<JoinHandle<()>>,
    dir: PathBuf,
    name: &'static str,
    identity: listener::ListenerId,
}

struct BoundSocket {
    path: PathBuf,
    identity: listener::ListenerId,
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| m.file_type().is_socket())
            && listener::names_now(&self.path, self.identity) == listener::Named::Same
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn stale(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    )
}

impl Server {
    pub(super) fn start(dir: &Path, source: ObservationHandle) -> io::Result<Self> {
        Self::start_named(dir, NAME, source)
    }

    /// Serve a held owner's wait ([`WAIT`]).
    pub(super) fn start_wait(dir: &Path, source: ObservationHandle) -> io::Result<Self> {
        Self::start_named(dir, WAIT, source)
    }

    fn start_named(dir: &Path, name: &'static str, source: ObservationHandle) -> io::Result<Self> {
        private_file::check_private_dir(dir)?;
        let path = dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let previous = endpoint_named(dir, name)?;
                // Never replace a listener which is still accepting connections.
                if !aterm_uds::connect::until(&path, Instant::now() + EXCHANGE)
                    .is_err_and(|error| stale(&error))
                    || endpoint_named(dir, name)? != previous
                    || listener::names_now(&path, previous) != listener::Named::Same
                {
                    return Err(refused());
                }
                std::fs::remove_file(&path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let socket = CtlListener::bind(&path)?;
        let bound = BoundSocket {
            identity: listener::listener_read(&path)?.ok_or_else(refused)?,
            path: path.clone(),
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        aterm_uds::keep_out_of_children(&socket)?;
        socket.set_nonblocking(true)?;
        if endpoint_named(dir, name)? != bound.identity {
            return Err(refused());
        }
        let (wake_read, wake_write) = CtlStream::pair()?;
        aterm_uds::keep_out_of_children(&wake_read)?;
        aterm_uds::keep_out_of_children(&wake_write)?;
        let identity = bound.identity;
        let task = thread::Builder::new()
            .name("network-observer-read".into())
            .spawn(move || {
                while let Ok(ready) = aterm_uds::ready::either(socket.as_fd(), wake_read.as_fd()) {
                    if ready.second
                        || listener::names_now(&path, bound.identity) != listener::Named::Same
                    {
                        break;
                    }
                    match socket.accept() {
                        Ok((mut client, _)) => {
                            if aterm_uds::keep_out_of_children(&client).is_ok()
                                && client.set_nonblocking(false).is_ok()
                                && peer::peer_uid(&client) == Some(peer::our_uid())
                            {
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
            name,
            identity,
        })
    }

    pub(super) fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
            && endpoint_named(&self.dir, self.name).is_ok_and(|identity| identity == self.identity)
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
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|error| context("read-timeout", error))?;
        match stream.read(bytes) {
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
    if age_ms > 15_000 {
        return Err(refused());
    }
    let wire = live_encode_publication(&publication, age_ms)?;
    write_all(client, &(wire.len() as u32).to_be_bytes(), deadline)?;
    write_all(client, &wire, deadline)?;
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

/// The live observation of `config`'s Commons from another local owner: the journal owner's, else
/// a held owner's wait. If both fail, preserve the owner's error unless its endpoint is absent
/// or no longer accepting connections.
pub(crate) fn read_publication(config: &Path) -> io::Result<Publication> {
    read_named(config, NAME).or_else(|owner_error| {
        read_named(config, WAIT).map_err(|wait_error| {
            if stale(&owner_error) {
                wait_error
            } else {
                owner_error
            }
        })
    })
}

/// The journal owner's live observation only ([`read_publication`] without a held owner's wait).
pub(crate) fn read_owner_publication(config: &Path) -> io::Result<Publication> {
    read_named(config, NAME)
}

fn read_named(config: &Path, name: &str) -> io::Result<Publication> {
    let requested = Instant::now();
    let deadline = requested + EXCHANGE;
    let dir = directory(config);
    let bound = endpoint_named(&dir, name).map_err(|error| context("endpoint", error))?;
    let mut client = aterm_uds::connect::until(&dir.join(name), deadline)
        .map_err(|error| context("connect", error))?;
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
        || endpoint_named(&dir, name).map_err(|error| context("endpoint-recheck", error))? != bound
        || listener::names_now(dir.join(name), bound) != listener::Named::Same
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
    let history = encode(state, None).map_err(|_| refused())?;
    let history: Value = aterm_json::from_slice(&history).map_err(|_| refused())?;
    let live = state.peers.iter().map(|peer| {
        let age = if peer.reachability == Reachability::Reachable {
            publication.contact_ages.get(&peer.slot).copied().map(age_millis).transpose()?
        } else {
            None
        };
        Ok(json!({"slot":peer.slot,"state":state_value(peer.reachability),"checked":peer.last_checked_ms,"contact_age_ms":age}))
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
    if age_ms > 15_000 {
        return Err(refused_at("source-stale"));
    }
    let history = aterm_json::to_vec(
        value
            .get("history")
            .ok_or_else(|| refused_at("snapshot-history"))?,
    )
    .map_err(|_| refused_at("snapshot-history"))?;
    // History decoding validates bounded identities and samples but deliberately
    // resets current evidence. Only this live exchange can supply it again.
    let mut observation = decode(&history).map_err(|_| refused_at("snapshot-history"))?;
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
        let peer = observation
            .peers
            .iter_mut()
            .find(|p| p.slot == slot)
            .ok_or_else(|| refused_at("snapshot-slots"))?;
        peer.reachability = state(row.get("state").ok_or_else(|| refused_at("peer-state"))?)
            .map_err(|_| refused_at("peer-state"))?;
        let contact_age = match row.get("contact_age_ms") {
            Some(value) if value.is_null() => None,
            Some(value) => {
                let age = value
                    .as_u64()
                    .filter(|age| *age <= 15_000)
                    .ok_or_else(|| refused_at("peer-contact-age"))?;
                Some(Duration::from_millis(age))
            }
            None => None,
        };
        if peer.reachability == Reachability::Reachable {
            let age = contact_age.ok_or_else(|| refused_at("peer-contact-age"))?;
            contact_ages.insert(slot, age);
        }
        peer.last_checked_ms = match row.get("checked") {
            Some(value) if value.is_null() => None,
            Some(value) => Some(value.as_u64().ok_or_else(|| refused_at("peer-check"))?),
            None => return Err(refused_at("peer-check")),
        };
        if peer.first_seen_ms > peer.last_seen_ms
            || peer.last_seen_ms > now
            || peer.last_checked_ms.is_some_and(|at| at > now)
            || peer.samples.iter().any(|sample| sample.at_ms > now)
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
mod tests {
    use super::*;

    fn publish(handle: &ObservationHandle, state: NetworkObservation) {
        let mut publication = Publication::new(state, Instant::now());
        publication.contact_ages = publication
            .observation
            .peers
            .iter()
            .map(|peer| {
                (
                    peer.slot,
                    Duration::from_millis(
                        publication
                            .observation
                            .observed_at_ms
                            .saturating_sub(peer.last_seen_ms),
                    ),
                )
            })
            .collect();
        handle.replace_publication(publication);
    }

    #[test]
    fn relayed_source_age_survives_repeated_reads_and_clock_rollback() {
        let state = current();
        let requested = Instant::now();
        let first = decode_publication(
            &live_encode(&state, 12_000).unwrap(),
            state.observed_at_ms,
            requested,
        )
        .unwrap();
        let again = decode_publication(
            &live_encode(&state, 14_000).unwrap(),
            state.observed_at_ms,
            requested + Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(first.at, again.at);
        assert_eq!(
            first
                .current_at(
                    requested + Duration::from_secs(4),
                    Some(state.observed_at_ms)
                )
                .visibility,
            Visibility::Unavailable
        );
        assert_eq!(
            again
                .current_at(
                    requested + Duration::from_secs(4),
                    Some(state.observed_at_ms)
                )
                .visibility,
            Visibility::Unavailable
        );
        assert_eq!(first.observation.observed_at_ms, state.observed_at_ms);
    }

    #[test]
    fn relayed_current_claim_requires_a_bounded_source_age() {
        let state = current();
        let mut value: Value = aterm_json::from_slice(&live_encode(&state, 0).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("source_age_ms");
        assert!(live_decode(&aterm_json::to_vec(&value).unwrap(), state.observed_at_ms).is_err());
        for age in [json!(-1), json!(15_001), json!("0"), json!(null)] {
            value
                .as_object_mut()
                .unwrap()
                .insert("source_age_ms".into(), age);
            assert!(
                live_decode(&aterm_json::to_vec(&value).unwrap(), state.observed_at_ms).is_err()
            );
        }
    }

    #[test]
    fn relay_preserves_monotonic_contact_age_after_a_prior_wall_rollback() {
        let mut state = current();
        let observed = state.observed_at_ms;
        state.peers[0].first_seen_ms = observed - 2000;
        state.peers[0].last_seen_ms = observed - 2000;
        state.peers[0].last_checked_ms = Some(observed - 2000);
        let requested = Instant::now();
        let mut source = Publication::new(state, requested);
        source
            .contact_ages
            .insert(source.observation.peers[0].slot, Duration::from_secs(10));
        let wire = live_encode_publication(&source, 0).unwrap();
        let received = decode_publication(&wire, observed, requested).unwrap();
        assert_eq!(
            received
                .current_at(requested + Duration::from_secs(7), Some(observed + 7000))
                .peers[0]
                .reachability,
            Reachability::LastSeen
        );
        let mut value: Value = aterm_json::from_slice(&wire).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .get_mut("live")
            .unwrap()
            .as_array_mut()
            .unwrap()[0]
            .as_object_mut()
            .unwrap()
            .remove("contact_age_ms");
        assert!(live_decode(&aterm_json::to_vec(&value).unwrap(), observed).is_err());
        for age in [json!(-1), json!(u64::MAX), json!(null), json!("10")] {
            value
                .as_object_mut()
                .unwrap()
                .get_mut("live")
                .unwrap()
                .as_array_mut()
                .unwrap()[0]
                .as_object_mut()
                .unwrap()
                .insert("contact_age_ms".into(), age);
            assert!(live_decode(&aterm_json::to_vec(&value).unwrap(), observed).is_err());
        }
    }

    fn socket_dir() -> aterm_tempfile::TempDir {
        // macOS's default temporary root plus the per-config suffix exceeds
        // sockaddr_un on some hosts. Keep socket fixtures in a short root.
        aterm_tempfile::Builder::new()
            .prefix("observer-")
            .tempdir_in("/tmp")
            .unwrap()
    }

    fn current() -> NetworkObservation {
        let now = aterm_time::unix_millis().unwrap();
        NetworkObservation {
            observed_at_ms: now,
            visibility: Visibility::Active,
            history: HistoryState::Ready,
            observer: Some(MachineIdentity {
                machine_id: [1; 16],
                incarnation: [2; 16],
                label: None,
            }),
            peers: vec![PeerObservation {
                slot: 3,
                identity: MachineIdentity {
                    machine_id: [3; 16],
                    incarnation: [4; 16],
                    label: None,
                },
                first_seen_ms: now - 100,
                last_seen_ms: now,
                last_checked_ms: Some(now),
                reachability: Reachability::Reachable,
                instances: None,
                samples: vec![PresenceSample {
                    at_ms: now,
                    state: Reachability::Reachable,
                }],
            }],
            ..NetworkObservation::default()
        }
    }

    #[test]
    #[ignore = "child process fixture, invoked by cross_process_owner"]
    fn child_observer() {
        let config =
            PathBuf::from(std::env::var_os("ATERM_TEST_OBSERVER_CONFIG").expect("fixture config"));
        let handle = ObservationHandle::default();
        let owner = Observer::open(&config, handle.clone()).unwrap();
        let state = current();
        publish(&handle, state.clone());
        private_file::write_private(
            &directory(&config),
            "projection.json",
            &encode(&state, None).unwrap(),
        )
        .unwrap();
        println!("observer-ready");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read(&mut [0; 1]);
        drop(owner);
    }

    #[test]
    fn cross_process_owner_supplies_kernel_identity_then_releases_ownership() {
        struct ChildGuard(aterm_uds::spawnfd::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                aterm_uds::deadline::kill_within(&mut self.0, Duration::from_secs(60));
            }
        }
        let root = socket_dir();
        let config = root.path().join("aterm.toml");
        let executable = std::env::current_exe().unwrap();
        let mut command = aterm_uds::spawnfd::command(executable);
        command
            .args([
                "--exact",
                "network_state::relay::tests::child_observer",
                "--ignored",
                "--nocapture",
            ])
            .env("ATERM_TEST_OBSERVER_CONFIG", &config)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::inherit());
        let mut child = ChildGuard(command.spawn().unwrap());
        aterm_uds::deadline::prints(&mut child.0, "observer-ready", Duration::from_secs(60));
        assert!(matches!(
            Observer::open(&config, ObservationHandle::default()),
            Err(OpenFailure::Owned)
        ));
        let live = read(&config).unwrap();
        assert_eq!(live.via_local_process, Some(child.0.id()));
        assert_ne!(child.0.id(), std::process::id());
        assert_eq!(live.peers[0].reachability, Reachability::Reachable);
        drop(child.0.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut exit = aterm_uds::deadline::ExitWait::new(&child.0);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "observer child did not exit");
            exit.wait(deadline.saturating_duration_since(Instant::now()));
        }
        assert!(read(&config).is_err());
        let history = read_history(&config).unwrap();
        assert!(history.via_local_process.is_none());
        assert_eq!(history.peers[0].reachability, Reachability::Unknown);
        let successor = Observer::open(&config, ObservationHandle::default()).unwrap();
        drop(successor);
    }

    #[test]
    fn owner_serves_live_memory_without_promoting_persisted_history() {
        let root = socket_dir();
        let config = root.path().join("aterm.toml");
        let handle = ObservationHandle::default();
        let owner = Observer::open(&config, handle.clone()).unwrap();
        assert!(matches!(
            Observer::open(&config, ObservationHandle::default()),
            Err(OpenFailure::Owned)
        ));
        let state = current();
        publish(&handle, state.clone());
        let seen = read(&config).unwrap();
        assert_eq!(seen.via_local_process, Some(std::process::id()));
        assert_eq!(seen.observed_at_ms, state.observed_at_ms);
        assert_eq!(seen.observer, state.observer);
        assert_eq!(seen.peers, state.peers);
        let mut unwrapped = seen;
        unwrapped.via_local_process = None;
        assert_eq!(unwrapped, state);
        let history = encode(&state, None).unwrap();
        private_file::write_private(&directory(&config), "projection.json", &history).unwrap();
        drop(owner);
        assert!(read(&config).is_err());
        let retained = read_history(&config).unwrap();
        assert_eq!(retained.peers[0].reachability, Reachability::Unknown);
        assert!(retained.observer.is_none());
        assert!(retained.via_local_process.is_none());
        let successor = Observer::open(&config, ObservationHandle::default()).unwrap();
        drop(successor);
    }

    #[test]
    fn busy_publisher_refuses_relay_read_without_refreshing_evidence() {
        let root = socket_dir();
        let config = root.path().join("aterm.toml");
        let handle = ObservationHandle::default();
        let owner = Observer::open(&config, handle.clone()).unwrap();
        let state = current();
        publish(&handle, state.clone());
        let publisher = handle.0.lock().unwrap();
        let published_at = publisher.at;
        let request_config = config.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let request = thread::spawn(move || send.send(read(&request_config)).unwrap());
        let refused = receive.recv_timeout(Duration::from_secs(60));
        // Release before checking the hang detector, so a failed implementation
        // can finish and its server can be joined during test cleanup.
        drop(publisher);
        request.join().unwrap();
        let error = refused
            .expect("busy publication must not hold the relay request")
            .expect_err("a busy publisher supplies no current evidence");
        assert!(!matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));

        assert_eq!(handle.snapshot_if_fresh(Duration::MAX), Some(state));
        assert_eq!(handle.0.lock().unwrap().at, published_at);
        drop(owner);
    }

    #[test]
    fn monotonic_publication_age_and_nonrecursive_source_are_required() {
        let root = socket_dir();
        private_file::create_private_dir(root.path()).unwrap();
        let handle = ObservationHandle::default();
        let server = Server::start(root.path(), handle.clone()).unwrap();
        publish(&handle, current());
        // A wall-clock-fresh snapshot cannot conceal a stopped observer loop.
        handle.0.lock().unwrap().at = Instant::now() - REACHABLE - Duration::from_secs(1);
        let unchanged = handle.snapshot();
        // Query uses network-state beside config, so use a direct client here.
        let mut client =
            aterm_uds::connect::until(&root.path().join(NAME), Instant::now() + EXCHANGE).unwrap();
        write_all(&mut client, REQUEST, Instant::now() + EXCHANGE).unwrap();
        assert!(read_exact(&mut client, &mut [0; 4], Instant::now() + EXCHANGE).is_err());
        assert_eq!(handle.snapshot(), unchanged);
        let mut relayed = current();
        relayed.via_local_process = Some(42);
        publish(&handle, relayed);
        let mut client =
            aterm_uds::connect::until(&root.path().join(NAME), Instant::now() + EXCHANGE).unwrap();
        write_all(&mut client, REQUEST, Instant::now() + EXCHANGE).unwrap();
        assert!(read_exact(&mut client, &mut [0; 4], Instant::now() + EXCHANGE).is_err());
        drop(server);
    }

    #[test]
    fn replacement_socket_is_never_removed_by_previous_server() {
        let root = socket_dir();
        private_file::create_private_dir(root.path()).unwrap();
        let server = Server::start(root.path(), ObservationHandle::default()).unwrap();
        let path = root.path().join(NAME);
        std::fs::remove_file(&path).unwrap();
        let replacement = CtlListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let identity = endpoint(root.path()).unwrap();
        drop(server);
        assert_eq!(endpoint(root.path()).unwrap(), identity);
        drop(replacement);
    }

    #[test]
    fn unsafe_endpoints_are_refused_without_unlinking_them() {
        let root = socket_dir();
        private_file::create_private_dir(root.path()).unwrap();
        let path = root.path().join(NAME);
        std::fs::write(&path, b"keep").unwrap();
        assert!(Server::start(root.path(), ObservationHandle::default()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        std::fs::remove_file(&path).unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(Server::start(root.path(), ObservationHandle::default()).is_err());
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    }

    #[test]
    fn expiring_contacts_preserve_a_fresh_snapshot_and_its_other_peers() {
        for (seen_age_ms, checked_age_ms) in [(14_500, 14_500), (14_500, 0), (0, 14_500)] {
            let mut state = current();
            let observed = state.observed_at_ms;
            let mut fresh = state.peers[0].clone();
            fresh.slot = 4;
            fresh.identity.machine_id = [5; 16];
            fresh.identity.incarnation = [6; 16];
            state.peers.push(fresh);
            let old = &mut state.peers[0];
            old.first_seen_ms = observed - 20_000;
            old.last_seen_ms = observed - seen_age_ms;
            old.last_checked_ms = Some(observed - checked_age_ms);
            old.samples[0].at_ms = old.last_seen_ms;
            let requested = Instant::now();
            let initial =
                decode_publication(&live_encode(&state, 0).unwrap(), observed, requested).unwrap();
            assert_eq!(
                initial.current_at(requested, Some(observed)).peers[0].reachability,
                Reachability::Reachable
            );
            let received = decode_publication(
                &live_encode(&state, 1000).unwrap(),
                observed + 1000,
                requested,
            )
            .unwrap();
            let aged = received.current_at(requested, Some(observed + 1000));
            assert_eq!(aged.visibility, Visibility::Active);
            assert_eq!(aged.observed_at_ms, observed);
            assert_eq!(aged.peers[0].reachability, Reachability::LastSeen);
            assert_eq!(aged.peers[1].reachability, Reachability::Reachable);
        }
    }

    #[test]
    fn live_decoder_rejects_stale_future_and_unconfirmed_current_peers() {
        let state = current();
        let wire = live_encode(&state, 0).unwrap();
        assert!(live_decode(&wire, state.observed_at_ms).is_ok());
        assert!(live_decode(&wire, 0).is_err());
        assert!(live_decode(&wire, state.observed_at_ms - 1).is_err());
        assert!(live_decode(&wire, state.observed_at_ms + 15_001).is_err());
        for invalid in [
            Visibility::Unavailable,
            Visibility::Waiting,
            Visibility::Disabled,
        ] {
            let mut state = state.clone();
            state.visibility = invalid;
            assert!(live_decode(&live_encode(&state, 0).unwrap(), state.observed_at_ms).is_err());
        }
        let mut missing = state.clone();
        missing.peers[0].last_checked_ms = None;
        assert!(live_decode(&live_encode(&missing, 0).unwrap(), state.observed_at_ms).is_err());
        let mut future = state.clone();
        future.peers[0].last_seen_ms += 1;
        assert!(live_decode(&live_encode(&future, 0).unwrap(), state.observed_at_ms).is_err());
    }

    #[test]
    fn producer_hint_capacity_round_trips_and_larger_counts_are_refused() {
        let mut state = current();
        let maximum = crate::runtime::MAX_UNVERIFIED_ENDPOINTS;
        for count in [0, 128, 129, maximum] {
            state.unverified_endpoints = count;
            let received = live_decode(&live_encode(&state, 0).unwrap(), state.observed_at_ms)
                .expect("a bounded producer count must decode");
            assert_eq!(received.unverified_endpoints, count);
        }
        for count in [maximum + 1, usize::MAX] {
            state.unverified_endpoints = count;
            assert!(
                live_decode(&live_encode(&state, 0).unwrap(), state.observed_at_ms).is_err(),
                "a count outside the producer's bound was accepted"
            );
        }
    }

    #[test]
    fn malformed_frames_and_duplicate_identity_are_rejected() {
        let state = current();
        assert!(live_decode(b"{}", state.observed_at_ms).is_err());
        assert!(live_decode(&vec![b' '; MAX_BYTES + 1], state.observed_at_ms).is_err());
        let mut duplicate = state.clone();
        duplicate.peers.push(state.peers[0].clone());
        assert!(live_decode(&live_encode(&duplicate, 0).unwrap(), state.observed_at_ms).is_err());
        let mut oversized = state.clone();
        oversized.detail = "x".repeat(2049);
        assert!(live_decode(&live_encode(&oversized, 0).unwrap(), state.observed_at_ms).is_err());
        let mut future_sample = state.clone();
        future_sample.peers[0].samples[0].at_ms += 1;
        assert!(
            live_decode(
                &live_encode(&future_sample, 0).unwrap(),
                state.observed_at_ms
            )
            .is_err()
        );
    }

    #[test]
    fn deadline_expires_across_partial_progress() {
        let (mut reader, _writer) = CtlStream::pair().unwrap();
        assert!(read_exact(&mut reader, &mut [0; 1], Instant::now()).is_err());
        assert!(write_all(&mut reader, b"x", Instant::now()).is_err());
    }

    fn fake_reply(body: &[u8], size: u32) -> io::Result<NetworkObservation> {
        let root = socket_dir();
        let config = root.path().join("aterm.toml");
        let dir = directory(&config);
        private_file::create_private_dir(&dir).unwrap();
        let path = dir.join(NAME);
        let listener = CtlListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let body = body.to_vec();
        let (release, hold) = std::sync::mpsc::channel();
        let sender = thread::spawn(move || {
            let hang = Instant::now() + Duration::from_secs(60);
            let mut client = loop {
                match listener.accept() {
                    Ok((client, _)) => break client,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < hang, "fixture client did not connect");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture listener failed: {error}"),
                }
            };
            client.set_nonblocking(false).unwrap();
            let deadline = Instant::now() + EXCHANGE;
            let mut request = [0; REQUEST.len()];
            read_exact(&mut client, &mut request, deadline).unwrap();
            write_all(&mut client, &size.to_be_bytes(), deadline).unwrap();
            let _ = write_all(&mut client, &body, deadline);
            // Keep native peer identity and timeout operations valid until the reader finishes.
            hold.recv_timeout(Duration::from_secs(60))
                .expect("fixture reader did not finish");
        });
        let answer = read(&config);
        release.send(()).unwrap();
        sender.join().unwrap();
        answer
    }

    #[test]
    fn connected_peer_cannot_forward_malformed_oversized_or_stale_frames() {
        let rejected = |error: io::Error, stage| {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(error_stage(&error), Some(stage));
        };
        rejected(fake_reply(b"{}", 2).unwrap_err(), "snapshot-version");
        rejected(fake_reply(b"\xff", 1).unwrap_err(), "snapshot-json");
        rejected(
            fake_reply(b"", (MAX_BYTES + 1) as u32).unwrap_err(),
            "frame-size",
        );
        let mut stale = current();
        stale.observed_at_ms -= 15_001;
        let wire = live_encode(&stale, 0).unwrap();
        rejected(
            fake_reply(&wire, wire.len() as u32).unwrap_err(),
            "sample-stale",
        );
        let mut future = current();
        future.observed_at_ms += 60_000;
        let wire = live_encode(&future, 0).unwrap();
        rejected(
            fake_reply(&wire, wire.len() as u32).unwrap_err(),
            "sample-clock",
        );
        let wire = live_encode(&current(), 15_001).unwrap();
        rejected(
            fake_reply(&wire, wire.len() as u32).unwrap_err(),
            "source-stale",
        );
    }

    #[test]
    fn refused_owner_preserves_its_error_and_still_accepts_a_valid_wait() {
        let root = socket_dir();
        let config = root.path().join("aterm.toml");
        let owner_handle = ObservationHandle::default();
        let owner = Observer::open(&config, owner_handle.clone()).unwrap();
        let state = current();
        publish(&owner_handle, state.clone());
        let wait_handle = ObservationHandle::default();
        let mut held = current();
        held.visibility = Visibility::Disabled;
        held.peers.clear();
        publish(&wait_handle, held);
        let wait = Server::start_wait(&directory(&config), wait_handle).unwrap();
        assert_eq!(read(&config).unwrap().visibility, Visibility::Active);
        let mut refused = state;
        refused.unverified_endpoints = crate::runtime::MAX_UNVERIFIED_ENDPOINTS + 1;
        publish(&owner_handle, refused);
        let fallback = read(&config).unwrap();
        assert_eq!(fallback.visibility, Visibility::Disabled);
        assert_eq!(fallback.via_local_process, Some(std::process::id()));
        assert!(fallback.peers.is_empty());
        drop(wait);
        assert_eq!(
            read(&config).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        drop(owner);
        assert_eq!(read(&config).unwrap_err().kind(), io::ErrorKind::NotFound);
        let wait_handle = ObservationHandle::default();
        let mut invalid = current();
        invalid.unverified_endpoints = crate::runtime::MAX_UNVERIFIED_ENDPOINTS + 1;
        publish(&wait_handle, invalid);
        let wait = Server::start_wait(&directory(&config), wait_handle).unwrap();
        assert_eq!(
            read(&config).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        drop(wait);
    }

    #[test]
    fn live_listener_and_unsafe_socket_mode_are_not_replaced() {
        let root = socket_dir();
        private_file::create_private_dir(root.path()).unwrap();
        let path = root.path().join(NAME);
        let listener = CtlListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let original = endpoint(root.path()).unwrap();
        assert!(Server::start(root.path(), ObservationHandle::default()).is_err());
        assert_eq!(endpoint(root.path()).unwrap(), original);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(Server::start(root.path(), ObservationHandle::default()).is_err());
        assert!(path.exists());
        drop(listener);
    }

    #[test]
    fn full_or_unresponsive_listener_is_not_evidence_of_a_stale_endpoint() {
        for kind in [
            io::ErrorKind::TimedOut,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Interrupted,
        ] {
            assert!(!stale(&io::Error::from(kind)));
        }
        assert!(stale(&io::Error::from(io::ErrorKind::ConnectionRefused)));
        assert!(stale(&io::Error::from(io::ErrorKind::NotFound)));
    }

    #[test]
    fn long_socket_path_does_not_stop_local_observation_or_history() {
        let root = socket_dir();
        let long = root.path().join("x".repeat(160));
        private_file::create_private_dir(&long).unwrap();
        let config = long.join("aterm.toml");
        let handle = ObservationHandle::default();
        let mut owner = Observer::open(&config, handle.clone()).unwrap();
        assert!(owner.relay.is_none());
        owner.observe(
            Visibility::Active,
            0,
            "Discovery only",
            aterm_time::unix_millis().unwrap(),
            Instant::now(),
        );
        assert_eq!(handle.snapshot().visibility, Visibility::Active);
        assert!(owner.log.as_ref().unwrap().stats().retained_records > 0);
        assert_eq!(read_history(&config).unwrap().history, HistoryState::Ready);
        assert!(read(&config).is_err());
    }

    #[test]
    fn failed_start_retries_after_cadence_without_reopening_history() {
        let root = socket_dir();
        let config = root.path().join("aterm.toml");
        let dir = directory(&config);
        private_file::create_private_dir(&dir).unwrap();
        let path = dir.join(NAME);
        std::fs::write(&path, b"unrelated").unwrap();
        let mut owner = Observer::open(&config, ObservationHandle::default()).unwrap();
        assert!(owner.relay.is_none());
        std::fs::remove_file(&path).unwrap();
        let retry = Instant::now();
        owner.relay_checked = retry - Duration::from_secs(10);
        owner.observe(
            Visibility::Active,
            0,
            "Discovery only",
            aterm_time::unix_millis().unwrap(),
            retry - Duration::from_secs(1),
        );
        assert!(owner.relay.is_none());
        owner.observe(
            Visibility::Active,
            0,
            "Discovery only",
            aterm_time::unix_millis().unwrap(),
            retry,
        );
        assert!(owner.relay.as_ref().unwrap().is_running());
        assert_eq!(read(&config).unwrap().visibility, Visibility::Active);
        assert!(matches!(
            Observer::open(&config, ObservationHandle::default()),
            Err(OpenFailure::Owned)
        ));
    }

    #[test]
    fn finished_worker_is_replaced_at_retry_cadence() {
        let root = socket_dir();
        let config = root.path().join("aterm.toml");
        let mut owner = Observer::open(&config, ObservationHandle::default()).unwrap();
        let server = owner.relay.as_mut().unwrap();
        drop(server.wake.take());
        let hang = Instant::now() + Duration::from_secs(60);
        while !server.task.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < hang, "observer reader did not stop");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!server.is_running());
        let retry = Instant::now();
        owner.relay_checked = retry - Duration::from_secs(10);
        owner.observe(
            Visibility::Active,
            0,
            "Discovery only",
            aterm_time::unix_millis().unwrap(),
            retry,
        );
        assert!(owner.relay.as_ref().unwrap().is_running());
        assert_eq!(read(&config).unwrap().visibility, Visibility::Active);
    }
}
