// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-process `@<sid>` proxy forwarding over the per-process control socket.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use aterm_session::{LaunchNonce, SessionId};
use aterm_types::control_request::Sid;
use aterm_uds::CtlStream;

#[cfg(unix)]
use crate::native_document_host::HeldAdvisoryLock;

fn graph_path(sock_dir: &Path, sid: Sid<'_>) -> std::path::PathBuf {
    sock_dir.join("graph").join(sid.as_str())
}

fn entry_path(sock_dir: &Path, sid: &SessionId) -> Option<std::path::PathBuf> {
    Sid::parse(sid.as_str()).map(|sid| graph_path(sock_dir, sid))
}

struct Discovery {
    /// This instance's own bound control socket `(sock_dir, sock_path)`, recorded
    /// by the control server at bind time.
    self_sock: RwLock<Option<(PathBuf, String)>>,
    /// The `aterm-publish` thread ([`publisher`]).
    publisher: std::sync::OnceLock<Option<Publisher>>,
    /// Test-only override for the mirror target dir ([`set_mirror_dir_override`]).
    #[cfg(test)]
    mirror_dir_override: RwLock<Option<PathBuf>>,
}

impl Discovery {
    const fn new() -> Self {
        Self {
            self_sock: RwLock::new(None),
            publisher: std::sync::OnceLock::new(),
            #[cfg(test)]
            mirror_dir_override: RwLock::new(None),
        }
    }
}

static PROCESS_DISCOVERY: Discovery = Discovery::new();

fn discovery() -> &'static Discovery {
    #[cfg(test)]
    if let Some(own) = OWN_DISCOVERY.get() {
        return own;
    }
    &PROCESS_DISCOVERY
}

#[cfg(test)]
thread_local! {
    static OWN_DISCOVERY: std::cell::Cell<Option<&'static Discovery>> =
        const { std::cell::Cell::new(None) };
}

/// TESTS ONLY: a test sets discovery state only on a state of its own
/// ([`self_sock_test_guard`], lent to the thread that binds): the process's is
/// every other test's, whose Commit would wait behind this one's writes.
#[cfg(test)]
fn assert_own_discovery(what: &str) {
    assert!(
        OWN_DISCOVERY.get().is_some(),
        "a test {what} on the process's discovery state: hold `self_sock_test_guard()` \
         and lend it to the thread that does (`OwnDiscovery::lend`)"
    );
}

/// Record this instance's bound control socket for session discovery publication.
pub(crate) fn set_self_sock(sock_dir: &Path, sock_path: &str) {
    let canon = std::fs::canonicalize(sock_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            let p = Path::new(sock_path);
            match (
                p.parent().and_then(|d| std::fs::canonicalize(d).ok()),
                p.file_name(),
            ) {
                (Some(dir), Some(name)) => dir.join(name).to_string_lossy().into_owned(),
                _ => sock_path.to_string(),
            }
        });
    #[cfg(test)]
    assert_own_discovery("binds");
    *discovery()
        .self_sock
        .write()
        .unwrap_or_else(|p| p.into_inner()) = Some((sock_dir.to_path_buf(), canon));
    let _ = publisher();
}

/// This instance's own bound socket path, or `None` when unbound.
#[must_use]
pub(crate) fn self_sock_path() -> Option<String> {
    discovery()
        .self_sock
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|(_, p)| p.clone())
}

/// Test-only: clear the recorded self socket.
#[cfg(test)]
pub(crate) fn clear_self_sock() {
    *discovery()
        .self_sock
        .write()
        .unwrap_or_else(|p| p.into_inner()) = None;
}

/// Test-only: isolate discovery state on this thread until the guard drops.
#[cfg(test)]
pub(crate) fn self_sock_test_guard() -> OwnDiscovery {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let serial = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let own: &'static Discovery = Box::leak(Box::new(Discovery::new()));
    OwnDiscovery {
        own,
        previous: OWN_DISCOVERY.replace(Some(own)),
        _serial: serial,
    }
}

/// Test-only guard returned by [`self_sock_test_guard`].
#[cfg(test)]
pub(crate) struct OwnDiscovery {
    own: &'static Discovery,
    previous: Option<&'static Discovery>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(all(test, unix))]
impl OwnDiscovery {
    /// Lend this test's discovery state to a spawned thread ([`LentDiscovery::enter`]).
    pub(crate) fn lend(&self) -> LentDiscovery {
        LentDiscovery(self.own)
    }
}

#[cfg(test)]
impl Drop for OwnDiscovery {
    fn drop(&mut self) {
        OWN_DISCOVERY.set(self.previous);
        if let Some(Some(p)) = self.own.publisher.get() {
            let _ = p.tx.send(Publication::Stop);
        }
    }
}

/// Test-only discovery state lent to a spawned thread ([`OwnDiscovery::lend`]).
#[cfg(all(test, unix))]
#[derive(Clone, Copy)]
pub(crate) struct LentDiscovery(&'static Discovery);

#[cfg(all(test, unix))]
impl LentDiscovery {
    /// Bind this thread to the lender's discovery state until the guard drops.
    pub(crate) fn enter(self) -> EnteredDiscovery {
        EnteredDiscovery(OWN_DISCOVERY.replace(Some(self.0)))
    }
}

/// Test-only guard returned by [`LentDiscovery::enter`].
#[cfg(all(test, unix))]
pub(crate) struct EnteredDiscovery(Option<&'static Discovery>);

#[cfg(all(test, unix))]
impl Drop for EnteredDiscovery {
    fn drop(&mut self) {
        OWN_DISCOVERY.set(self.0);
    }
}

/// Publish the discovery graph entry for a session this instance hosts.
pub(crate) fn publish_session(sid: &SessionId, nonce: &LaunchNonce) {
    let guard = discovery()
        .self_sock
        .read()
        .unwrap_or_else(|p| p.into_inner());
    let Some((dir, sock)) = guard.as_ref() else {
        return;
    };
    queue_publication(Publication::Publish {
        dir: dir.clone(),
        sid: sid.clone(),
        sock: published_sock_spelling(sock),
        nonce: *nonce,
    });
    drop(guard);
}

enum Publication {
    /// [`publish_graph_entry`] of `sid` naming `sock`.
    Publish {
        dir: PathBuf,
        sid: SessionId,
        sock: String,
        nonce: LaunchNonce,
    },
    /// [`retire_graph_entry`] of `sid`.
    Retire { dir: PathBuf, sid: SessionId },
    /// Retire only this instance's graph entries, behind all earlier publishes.
    RetireOwn {
        dir: PathBuf,
        sid: SessionId,
        sock: String,
        recorded_sock: Option<String>,
    },
    /// Answered once every write queued before it has been processed (a rendezvous
    /// channel holds the thread until the answer is taken: a test's).
    Flush(std::sync::mpsc::SyncSender<()>),
    /// TESTS ONLY: a test's publication thread ends here, behind every write
    /// queued before it ([`OwnDiscovery`]'s drop).
    #[cfg(test)]
    Stop,
}

impl Publication {
    fn write(self) {
        match self {
            Self::Publish {
                dir,
                sid,
                sock,
                nonce,
            } => publish_graph_entry(&dir, &sid, &sock, &nonce),
            Self::Retire { dir, sid } => retire_graph_entry(&dir, &sid),
            Self::RetireOwn {
                dir,
                sid,
                sock,
                recorded_sock,
            } => {
                let retire = |at: &Path| {
                    remove_own_graph_entry(at, &sid, &sock);
                    if let Some(recorded) = recorded_sock.as_deref() {
                        remove_own_graph_entry(at, &sid, recorded);
                        remove_own_graph_entry(at, &sid, &published_sock_spelling(recorded));
                    }
                };
                retire(&dir);
                if let Some(mirror) = rendezvous_mirror_dir(&dir) {
                    retire(&mirror);
                }
            }
            Self::Flush(done) => {
                let _ = done.send(());
            }
            #[cfg(test)]
            Self::Stop => {}
        }
    }
}

struct Publisher {
    tx: std::sync::mpsc::Sender<Publication>,
    pending: std::sync::atomic::AtomicUsize,
}

fn publisher() -> Option<&'static Publisher> {
    let state = discovery();
    state
        .publisher
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Publication>();
            let spawned = crate::qos::spawn_lane_thread(
                "aterm-publish",
                crate::qos::Role::Responsive,
                move || {
                    #[cfg(test)]
                    OWN_DISCOVERY.set(Some(state));
                    for publication in rx {
                        #[cfg(test)]
                        if matches!(publication, Publication::Stop) {
                            break;
                        }
                        publication.write();
                        if let Some(Some(p)) = state.publisher.get() {
                            p.pending.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                        }
                    }
                },
            );
            match spawned {
                Ok(_) => Some(Publisher {
                    tx,
                    pending: std::sync::atomic::AtomicUsize::new(0),
                }),
                Err(e) => {
                    aterm_log::warn!("discovery: could not start the publication thread: {e}");
                    None
                }
            }
        })
        .as_ref()
}

fn queue_publication(publication: Publication) {
    let Some(p) = publisher() else {
        publication.write();
        return;
    };
    p.pending.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    if let Err(std::sync::mpsc::SendError(publication)) = p.tx.send(publication) {
        p.pending.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        publication.write();
    }
}

/// Wait up to `within` for earlier queued publication operations to complete.
pub(crate) fn flush_publications(within: std::time::Duration) -> bool {
    let Some(Some(p)) = discovery().publisher.get() else {
        return true;
    };
    if p.pending.load(std::sync::atomic::Ordering::Acquire) == 0 {
        return true;
    }
    let (done, landed) = std::sync::mpsc::sync_channel(1);
    queue_publication(Publication::Flush(done));
    landed.recv_timeout(within).is_ok()
}

/// Stop session publication and queue guarded retirements for `sids`.
pub(crate) fn retire_own_sessions(
    dir: &Path,
    sock: &str,
    sessions: impl IntoIterator<Item = SessionId>,
) {
    let recorded_sock = {
        let mut guard = discovery()
            .self_sock
            .write()
            .unwrap_or_else(|p| p.into_inner());
        guard.take().map(|(_, recorded)| recorded)
    };
    for sid in sessions {
        queue_publication(Publication::RetireOwn {
            dir: dir.to_path_buf(),
            sid,
            sock: sock.to_string(),
            recorded_sock: recorded_sock.clone(),
        });
    }
}

fn published_sock_spelling(sock: &str) -> String {
    let stripped = sock.strip_prefix(r"\\?\").unwrap_or(sock);
    let drive = stripped.len() >= 2
        && stripped.as_bytes()[1] == b':'
        && stripped.as_bytes()[0].is_ascii_alphabetic();
    if drive {
        stripped.to_string()
    } else {
        sock.to_string()
    }
}

/// Remove a closed session's discovery entry (best-effort).
pub(crate) fn unpublish_session(sid: &SessionId) {
    let guard = discovery()
        .self_sock
        .read()
        .unwrap_or_else(|p| p.into_inner());
    let Some((dir, _)) = guard.as_ref() else {
        return;
    };
    queue_publication(Publication::Retire {
        dir: dir.clone(),
        sid: sid.clone(),
    });
    drop(guard);
}

/// Read the per-launch auth token beside `sock_path`.
#[must_use]
pub(crate) fn read_sibling_token(sock_path: &str) -> Option<String> {
    aterm_ctl::read_token_beside(sock_path).ok()
}

/// Write `<sock_dir>/graph/<sid>` with `sock_path` and `nonce`.
pub(crate) fn write_graph_entry(
    sock_dir: &Path,
    sid: &SessionId,
    sock_path: &str,
    nonce: &LaunchNonce,
) {
    let Some(path) = entry_path(sock_dir, sid) else {
        return;
    };
    let dir = sock_dir.join("graph");
    if crate::control_auth::ensure_private_dir(&dir).is_err() {
        return;
    }
    let body = format!(
        "sock {sock_path}\nnonce {}\npid {}\n",
        nonce.to_hex(),
        std::process::id()
    );
    if let Ok(mut f) = open_private(&path) {
        let _ = f.write_all(body.as_bytes());
    }
}

/// Test-only: override the rendezvous mirror target directory.
#[cfg(test)]
pub(crate) fn set_mirror_dir_override(dir: Option<PathBuf>) {
    assert_own_discovery("redirects the mirror");
    *discovery()
        .mirror_dir_override
        .write()
        .unwrap_or_else(|p| p.into_inner()) = dir;
}

fn rendezvous_mirror_dir(sock_dir: &Path) -> Option<PathBuf> {
    #[cfg(test)]
    {
        let over_guard = discovery().mirror_dir_override.read();
        if let Some(over) = over_guard.unwrap_or_else(|p| p.into_inner()).clone() {
            return if same_dir(sock_dir, &over) {
                None
            } else {
                Some(over)
            };
        }
    }
    let rv = aterm_uds::control_socket_dir()?;
    if same_dir(sock_dir, &rv) {
        None
    } else {
        Some(rv)
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Publish a session's discovery entry to `sock_dir` and the default rendezvous mirror.
pub(crate) fn publish_graph_entry(
    sock_dir: &Path,
    sid: &SessionId,
    sock_path: &str,
    nonce: &LaunchNonce,
) {
    write_graph_entry(sock_dir, sid, sock_path, nonce);
    if let Some(rv) = rendezvous_mirror_dir(sock_dir) {
        write_graph_entry(&rv, sid, sock_path, nonce);
    }
}

/// Retire a session's discovery entry from `sock_dir` and the rendezvous mirror.
pub(crate) fn retire_graph_entry(sock_dir: &Path, sid: &SessionId) {
    remove_graph_entry(sock_dir, sid);
    if let Some(rv) = rendezvous_mirror_dir(sock_dir) {
        remove_graph_entry(&rv, sid);
    }
}

/// Open (create/truncate) a private sidecar file for writing. Unix: mode
/// `0600`.
#[cfg(unix)]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// Windows twin of [`open_private`] (ACL-inherited; see the Unix docs).
#[cfg(windows)]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// Remove `<sock_dir>/graph/<sid>` on graceful exit (best-effort).
pub(crate) fn remove_graph_entry(sock_dir: &Path, sid: &SessionId) {
    if let Some(path) = entry_path(sock_dir, sid) {
        let _ = std::fs::remove_file(path);
    }
}

/// Remove `sid`'s discovery entry only if it still names `our_sock`.
pub(crate) fn remove_own_graph_entry(sock_dir: &Path, sid: &SessionId, our_sock: &str) {
    let Some(path) = entry_path(sock_dir, sid) else {
        return;
    };
    let Ok(body) = std::fs::read_to_string(&path) else {
        return;
    };
    let Some(named) = aterm_types::control_socket::graph_entry_sock(&body) else {
        return;
    };
    let canonical = |p: &str| {
        std::fs::canonicalize(p)
            .map_or_else(|_| p.to_string(), |c| c.to_string_lossy().into_owned())
    };
    let ours = named == our_sock
        || canonical(&named) == canonical(our_sock)
        || self_sock_path().is_some_and(|me| named == me || named == published_sock_spelling(&me));
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

/// Remove `graph/<sid>` entries whose recorded socket has no live listener.
pub(crate) fn sweep_stale_graph(sock_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(sock_dir.join("graph")) else {
        return;
    };
    for ent in entries.flatten() {
        let path = ent.path();
        if let Ok(body) = std::fs::read_to_string(&path)
            && let Some((sock, _nonce)) = parse_graph_entry(&body)
            && !crate::control_auth::socket_is_live(&sock)
        {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Write parent-to-child edge-token secrets to `<sock_dir>/edges/<child_sid>` (0600).
pub(crate) fn write_edge_tokens(
    sock_dir: &Path,
    child_sid: &SessionId,
    read_hex: &str,
    write_hex: &str,
    signal_hex: &str,
) -> Option<EdgeFile> {
    let dir = sock_dir.join("edges");
    if crate::control_auth::ensure_private_dir(&dir).is_err() {
        return None;
    }
    let path = dir.join(child_sid.as_str());
    let body = format!("read {read_hex}\nwrite {write_hex}\nsignal {signal_hex}\n");
    let f = open_private(&path).ok()?;
    let Some(lock) = take_owner_lock(f) else {
        let _ = std::fs::remove_file(&path);
        return None;
    };
    let mut secret: &std::fs::File = owner_file(&lock);
    if secret.write_all(body.as_bytes()).is_err() {
        let _ = std::fs::remove_file(&path);
        drop(lock);
        return None;
    }
    Some(EdgeFile { path, _lock: lock })
}

/// An edge-token file written by [`write_edge_tokens`] and its owner-lock handle.
#[derive(Debug)]
pub(crate) struct EdgeFile {
    path: PathBuf,
    _lock: OwnerLock,
}

/// What an [`EdgeFile`] holds: on unix the owner's `flock`, as a guard that lets
/// go by `LOCK_UN`; elsewhere no sweep tells owners apart, so the open file is
/// all there is (a Windows lock would also refuse the child's read).
#[cfg(unix)]
type OwnerLock = HeldAdvisoryLock;
#[cfg(not(unix))]
type OwnerLock = std::fs::File;

impl EdgeFile {
    /// Absolute path to the edge-token file.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Take `f`'s exclusive owner lock without waiting (`flock(LOCK_EX|LOCK_NB)` on
/// its own open description): `None`, holding nothing, if it cannot be taken.
#[cfg(unix)]
fn take_owner_lock(f: std::fs::File) -> Option<OwnerLock> {
    HeldAdvisoryLock::try_lock(f).ok()
}

#[cfg(not(unix))]
fn take_owner_lock(f: std::fs::File) -> Option<OwnerLock> {
    Some(f)
}

#[cfg(unix)]
const fn owner_file(lock: &OwnerLock) -> &std::fs::File {
    lock.file()
}

#[cfg(not(unix))]
fn owner_file(lock: &OwnerLock) -> &std::fs::File {
    lock
}

/// Remove the edge-token files in `dir` whose owning parent is gone. A file is
/// its parent's while the parent's `flock` on it holds ([`EdgeFile`]), probed
/// as the crash markers are (`crash_signal::markers::probe`): a file whose lock
/// this sweep can take belongs to a dead parent (its tokens authorize nothing
/// again, since the child's `(sid, nonce)` is never reissued) and is removed.
#[cfg(unix)]
fn sweep_dead_edge_files(dir: &Path) {
    use crate::crash_signal::markers::{Owner, probe};
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.file_type().is_file() || meta.len() == 0 {
            continue;
        }
        let path = entry.path();
        if probe(&path) == Owner::Dead {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// How long the edge sweeper ([`start_edge_sweeper`]) waits between passes.
#[cfg(unix)]
const EDGE_SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Start the background sweeper for dead parents' edge files.
#[cfg(unix)]
pub(crate) fn start_edge_sweeper() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-edge-sweep",
            crate::qos::Role::Housekeeping,
            || {
                loop {
                    if let Some(dir) = crate::control_auth::socket_dir() {
                        sweep_dead_edge_files(&dir.join("edges"));
                    }
                    std::thread::sleep(EDGE_SWEEP_EVERY);
                }
            },
        );
        if let Err(e) = spawned {
            aterm_log::warn!("edge-token sweep: could not start its thread: {e}");
        }
    });
}

/// Read `(read, write, signal)` edge-token hexes from the 0600 file at `path`.
pub(crate) fn read_edge_tokens(path: &str) -> Option<(String, String, String)> {
    let body = std::fs::read_to_string(path).ok()?;
    let (mut r, mut w, mut s) = (None, None, None);
    for line in body.lines() {
        if let Some(v) = line.strip_prefix("read ") {
            r = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("write ") {
            w = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("signal ") {
            s = Some(v.trim().to_string());
        }
    }
    Some((r?, w?, s?))
}

/// Edge-token file held for a provisioned child session.
pub(crate) struct ChildProxy {
    edge_file: Option<EdgeFile>,
}

impl ChildProxy {
    /// Take ownership of `edge_file`.
    pub(crate) const fn new(edge_file: Option<EdgeFile>) -> Self {
        Self { edge_file }
    }
}

impl Drop for ChildProxy {
    fn drop(&mut self) {
        if let Some(file) = self.edge_file.take() {
            crate::spawn_prep::retire_edge_file(file);
        }
    }
}

/// Return the hosting PID recorded in `sid`'s discovery entry, if present.
pub(crate) fn graph_entry_host_pid(sock_dir: &Path, sid: &SessionId) -> Option<u32> {
    let body = std::fs::read_to_string(entry_path(sock_dir, sid)?).ok()?;
    aterm_types::control_socket::graph_entry_pid(&body)
}

/// Test-only: view `id` as a [`Sid`] for [`read_graph_entry`].
#[cfg(test)]
pub(crate) fn sid_of(id: &SessionId) -> Sid<'_> {
    Sid::parse(id.as_str()).expect("a minted id is a Sid")
}

/// Read `(sock_path, nonce)` from a session's discovery entry.
pub(crate) fn read_graph_entry(sock_dir: &Path, sid: Sid<'_>) -> Option<(String, LaunchNonce)> {
    let body = std::fs::read_to_string(graph_path(sock_dir, sid)).ok()?;
    parse_graph_entry(&body)
}

fn parse_graph_entry(body: &str) -> Option<(String, LaunchNonce)> {
    let sock = aterm_types::control_socket::graph_entry_sock(body)?;
    let nonce = body
        .lines()
        .find_map(|l| l.strip_prefix("nonce "))
        .and_then(|h| LaunchNonce::from_hex(h.trim()))?;
    Some((sock, nonce))
}

/// Reason a forward connection to the hosting instance failed ([`connect_hop`]).
#[derive(Debug)]
pub(crate) enum ForwardError {
    /// Nothing listens at the socket, or no token can be read beside it.
    NotAHop,
    /// Any other connect error.
    Unreachable(std::io::Error),
}

/// Connection to a session's hosting instance opened by [`connect_hop`].
pub(crate) struct Hop {
    stream: CtlStream,
    sock: String,
    token: String,
    listener: Option<aterm_uds::listener::ListenerId>,
}

/// Connect to host socket `sock` and read its sibling auth token.
/// # Errors
///
/// [`ForwardError::NotAHop`] for `ENOENT`, `ECONNREFUSED` or an unreadable
/// token; [`ForwardError::Unreachable`] for any other connect error.
pub(crate) fn connect_hop(sock: &str) -> Result<Hop, ForwardError> {
    let stream = CtlStream::connect(sock).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            ForwardError::NotAHop
        }
        _ => ForwardError::Unreachable(error),
    })?;
    let listener = aterm_uds::listener::listener_at(sock);
    let token = read_sibling_token(sock).ok_or(ForwardError::NotAHop)?;
    Ok(Hop {
        stream,
        sock: sock.to_string(),
        token,
        listener,
    })
}

/// Format the `TOKEN <token> <verb>` handshake line.
#[cfg(test)]
#[must_use]
pub(crate) fn forward_first_line(token: &str, verb: &str) -> String {
    format!("TOKEN {token} {verb}\n")
}

/// Deadline for a forwarded connection's connect probe.
#[cfg(not(test))]
pub(crate) const FORWARD_ACCEPT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
/// A unit test's hosts serve, or refuse or close at once, and its forwards run on threads a
/// per-thread bound would not reach: the probe waits the hang detector. A test of a host that
/// accepts nothing passes its own bound to [`deliver_then_relay`].
#[cfg(test)]
pub(crate) const FORWARD_ACCEPT_DEADLINE: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

/// Verify the host serves on `hop`, send `line` and `client_prebuffered`, and splice `client`.
pub(crate) fn relay_hop(
    hop: Hop,
    line: &str,
    client: &CtlStream,
    client_prebuffered: &[u8],
) -> std::io::Result<()> {
    deliver_then_relay(
        hop,
        line,
        client,
        client_prebuffered,
        FORWARD_ACCEPT_DEADLINE,
        relay_bidirectional,
    )
}

/// Whether a connect-probe reply refuses the connection rather than answering the probe.
pub(crate) fn is_connection_refusal(line: &str) -> bool {
    line == "ERR auth" || line.starts_with("ERR control server busy")
}

fn prove_host_serving(
    host: &CtlStream,
    token: &str,
    within: std::time::Duration,
) -> std::io::Result<()> {
    let line = probe_answer(host, token, within)?;
    if is_connection_refusal(&line) {
        return Err(std::io::Error::other(format!("the host answered {line:?}")));
    }
    host.set_read_timeout(None)
}

fn probe_answer(
    host: &CtlStream,
    token: &str,
    within: std::time::Duration,
) -> std::io::Result<String> {
    let mut hello = String::with_capacity(token.len() + 16);
    hello.push_str("TOKEN ");
    hello.push_str(token);
    hello.push_str(" version\n");
    match (&*host)
        .write_all(hello.as_bytes())
        .and_then(|()| (&*host).flush())
    {
        Ok(()) => read_probe_line(host, within),
        Err(e) => read_probe_line(host, within).map_err(|_| e),
    }
}

fn read_probe_line(host: &CtlStream, within: std::time::Duration) -> std::io::Result<String> {
    use std::io::{Error, ErrorKind};
    const MAX_PROBE_LINE: usize = 4096;
    let deadline = std::time::Instant::now() + within;
    let mut line = Vec::with_capacity(160);
    let mut chunk = [0u8; 1024];
    let not_accepted = || {
        Error::new(
            ErrorKind::TimedOut,
            format!(
                "the host accepted no connection within {}s",
                within.as_secs().max(1)
            ),
        )
    };
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(not_accepted());
        }
        let _ = host.set_read_timeout(Some(deadline - now));
        let read = match (&*host).read(&mut chunk) {
            Ok(0) => {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "the host closed the connection before answering",
                ));
            }
            Ok(n) => &chunk[..n],
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(not_accepted());
            }
            Err(e) => return Err(e),
        };
        match read.iter().position(|&b| b == b'\n') {
            Some(end) if end + 1 < read.len() => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "the host sent bytes past its probe answer",
                ));
            }
            Some(end) => {
                line.extend_from_slice(&read[..end]);
                break;
            }
            None => line.extend_from_slice(read),
        }
        if line.len() > MAX_PROBE_LINE {
            return Err(Error::new(ErrorKind::InvalidData, "runaway probe reply"));
        }
    }
    let line = String::from_utf8_lossy(&line);
    Ok(line.trim_end_matches('\r').to_string())
}

struct ForwardHost {
    sock: String,
    token: String,
    pid: Option<u32>,
    listener: Option<aterm_uds::listener::ListenerId>,
}

/// How long a look at a host that ended a forward waits for its answer.
#[cfg(not(test))]
const HOST_LOOK: std::time::Duration = std::time::Duration::from_secs(2);
/// A unit test's host answers its looks, or drops its listener, which refuses or resets them: a
/// look a loaded machine slows waits the hang detector rather than calling a serving host gone.
#[cfg(test)]
const HOST_LOOK: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

impl ForwardHost {
    fn gone_line(&self) -> Option<String> {
        let pid = self.pid?;
        (!self.serves(pid)).then(|| aterm_types::control_verbs::host_gone_line(pid, &self.sock))
    }

    fn serves(&self, pid: u32) -> bool {
        let Ok(look) = CtlStream::connect(&self.sock) else {
            return false;
        };
        let answered = aterm_uds::fdpass::peer_pid(&look).ok();
        if answered.is_some_and(|answered| answered != pid) {
            return false;
        }
        probe_answer(&look, &self.token, HOST_LOOK).is_ok()
            && match answered {
                Some(_) => aterm_uds::listener::still_names(&self.sock, self.listener),
                None => self
                    .listener
                    .is_some_and(|then| aterm_uds::listener::listener_at(&self.sock) == Some(then)),
            }
    }
}

fn deliver_then_relay(
    hop: Hop,
    line: &str,
    client: &CtlStream,
    client_prebuffered: &[u8],
    accept_within: std::time::Duration,
    relay: impl FnOnce(&CtlStream, &CtlStream, ForwardHost) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let Hop {
        stream: child,
        sock,
        token,
        listener,
    } = hop;
    prove_host_serving(&child, &token, accept_within)?;
    let pid = aterm_uds::fdpass::peer_pid(&child).ok();
    (&child).write_all(line.as_bytes())?;
    (&child).write_all(b"\n")?;
    if !client_prebuffered.is_empty() {
        (&child).write_all(client_prebuffered)?;
    }
    (&child).flush()?;
    let host = ForwardHost {
        sock,
        pid,
        token,
        listener,
    };
    let _ = relay(client, &child, host);
    Ok(())
}

fn relay_bidirectional(
    client: &CtlStream,
    child: &CtlStream,
    host: ForwardHost,
) -> std::io::Result<()> {
    use aterm_uds::hangup::hang_up;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

    let client_gone = &AtomicBool::new(false);
    let stop = &AtomicBool::new(false);
    let client_stop = &AtomicBool::new(false);
    let hang_up_child = move || {
        stop.store(true, SeqCst);
        hang_up(child);
    };
    let hang_up_both = move || {
        client_stop.store(true, SeqCst);
        hang_up(client);
        hang_up_child();
    };
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let relayed: std::io::Result<()> = std::thread::scope(|scope| {
        let _unwinding = HangUpOnUnwind(hang_up_both);
        let worker = std::thread::Builder::new().spawn_scoped(scope, move || {
            let _unwinding = HangUpOnUnwind(hang_up_both);
            let (mut from_host, mut to_client) = (child, client);
            let ready = || readable_unless_stopped(child, stop);
            match copy_host_to_client(&mut from_host, &mut to_client, ready) {
                HostLeg::HostEnded => {
                    if !client_gone.load(SeqCst)
                        && !aterm_uds::hangup::peer_closed(client)
                        && let Some(line) = host.gone_line()
                    {
                        let _ = to_client.write_all(line.as_bytes());
                        let _ = to_client.flush();
                    }
                    let _ = client.shutdown(std::net::Shutdown::Write);
                }
                HostLeg::Broke => hang_up_both(),
            }
            let _ = done_tx.send(());
        })?;
        let (mut from_client, mut to_child) = (client, child);
        let client_ready = || readable_unless_stopped(client, client_stop);
        match copy_until_eof(&mut from_client, &mut to_child, client_ready) {
            Ok(()) if !aterm_uds::hangup::peer_closed(client) => {
                let _ = child.shutdown(std::net::Shutdown::Write);
            }
            Err(_) if !aterm_uds::hangup::peer_closed(client) => {
                hang_up_child();
            }
            Ok(()) | Err(_) => {
                client_gone.store(true, SeqCst);
                hang_up(client);
                hang_up_child();
            }
        }
        while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
            done_rx.recv_timeout(crate::control::HANGUP_POLL)
        {
            if aterm_uds::hangup::peer_closed(client) {
                client_gone.store(true, SeqCst);
                hang_up(client);
                hang_up_child();
                break;
            }
        }
        let _ = worker.join();
        Ok(())
    });
    hang_up(client);
    hang_up_child();
    relayed
}

struct HangUpOnUnwind<F: Fn()>(F);

impl<F: Fn()> Drop for HangUpOnUnwind<F> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            (self.0)();
        }
    }
}

fn readable_unless_stopped(end: &CtlStream, stop: &std::sync::atomic::AtomicBool) -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        loop {
            match crate::control_listener::poll_readable(
                end.as_raw_fd(),
                crate::control::HANGUP_POLL,
            ) {
                Ok(true) => return true,
                Ok(false) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return true,
            }
            if stop.load(std::sync::atomic::Ordering::SeqCst) {
                return false;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = end;
        !stop.load(std::sync::atomic::Ordering::SeqCst)
    }
}

enum HostLeg {
    /// The host's side ended: its end of file, or the connection it reset
    /// (Linux resets a socket closed with unread input) — the host closed it,
    /// on purpose or by going away.
    HostEnded,
    /// Anything else: the client's side failed, or an error that says
    /// nothing about the host.
    Broke,
}

fn copy_host_to_client<R: Read, W: Write>(
    host: &mut R,
    client: &mut W,
    mut ready: impl FnMut() -> bool,
) -> HostLeg {
    let mut buf = [0u8; 32 * 1024];
    loop {
        if !ready() {
            return HostLeg::HostEnded;
        }
        match host.read(&mut buf) {
            Ok(0) => return HostLeg::HostEnded,
            Ok(n) => {
                if client
                    .write_all(&buf[..n])
                    .and_then(|()| client.flush())
                    .is_err()
                {
                    return HostLeg::Broke;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::UnexpectedEof
                ) =>
            {
                return HostLeg::HostEnded;
            }
            Err(_) => return HostLeg::Broke,
        }
    }
}

fn copy_until_eof<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    mut ready: impl FnMut() -> bool,
) -> std::io::Result<()> {
    let mut buf = [0u8; 32 * 1024];
    loop {
        if !ready() {
            return Ok(());
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        writer.write_all(&buf[..n])?;
        writer.flush()?;
    }
}

#[cfg(all(test, unix))]
#[path = "reply_fidelity_conformance.rs"]
mod reply_fidelity_conformance;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_socket::read_to_end_of_file;
    use crate::test_socket::{hang_up, join_after_hang_up};
    use std::io::{BufRead, BufReader};

    fn unknown_host() -> ForwardHost {
        ForwardHost {
            sock: String::new(),
            token: String::new(),
            pid: None,
            listener: None,
        }
    }

    fn token_beside(sock: &Path, token: &str) {
        let name = aterm_types::control_socket::token_name_for_sock(
            &sock.file_name().expect("a socket file").to_string_lossy(),
        );
        std::fs::write(sock.with_file_name(name), format!("{token}\n")).expect("the host's token");
    }

    fn hop_to(sock: &Path, token: &str) -> Hop {
        token_beside(sock, token);
        connect_hop(&sock.to_string_lossy())
            .unwrap_or_else(|e| panic!("{}: no hop: {e:?}", sock.display()))
    }

    #[test]
    fn published_sock_spelling_drops_only_the_verbatim_drive_prefix() {
        assert_eq!(
            published_sock_spelling(r"\\?\C:\Users\u\AppData\Local\Temp\aterm\aterm-1.sock"),
            r"C:\Users\u\AppData\Local\Temp\aterm\aterm-1.sock"
        );
        assert_eq!(
            published_sock_spelling(r"C:\Users\u\aterm-1.sock"),
            r"C:\Users\u\aterm-1.sock"
        );
        assert_eq!(
            published_sock_spelling(r"\\?\UNC\srv\share\aterm.sock"),
            r"\\?\UNC\srv\share\aterm.sock"
        );
        assert_eq!(
            published_sock_spelling("/private/var/folders/x/aterm-1.sock"),
            "/private/var/folders/x/aterm-1.sock"
        );
    }

    #[test]
    fn explicit_socket_instance_resolvable_from_default_dir_entry() {
        let default_dir =
            std::env::temp_dir().join(format!("aterm-defaultdir-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&default_dir);
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let explicit_sock = "/some/explicit/place/myapp-control.sock";

        write_graph_entry(&default_dir, &sid, explicit_sock, &nonce);

        let (sock, got_nonce) =
            read_graph_entry(&default_dir, sid_of(&sid)).expect("entry in default dir");
        assert_eq!(
            sock, explicit_sock,
            "the default-dir entry names the explicit out-of-dir socket verbatim"
        );
        assert!(got_nonce.ct_eq(&nonce));
        let body =
            std::fs::read_to_string(default_dir.join("graph").join(sid.as_str())).expect("body");
        assert_eq!(
            aterm_types::control_socket::graph_entry_sock(&body).as_deref(),
            Some(explicit_sock)
        );
        assert_eq!(
            aterm_types::control_socket::graph_entry_pid(&body),
            Some(std::process::id())
        );
        let _ = std::fs::remove_dir_all(&default_dir);
    }

    #[test]
    fn mirror_dir_only_for_out_of_default_instances() {
        let _guard = self_sock_test_guard();
        let Some(def) = aterm_uds::control_socket_dir() else {
            aterm_spec::host::could_not_run("no per-user control socket dir resolves here");
        };
        assert!(
            rendezvous_mirror_dir(&def).is_none(),
            "a default instance must not double-write"
        );
        let other = std::env::temp_dir().join("aterm-not-the-default-dir-xyz");
        assert_eq!(
            rendezvous_mirror_dir(&other).as_deref(),
            Some(def.as_path()),
            "an out-of-dir instance mirrors into the default rendezvous dir"
        );
    }

    #[test]
    fn parse_graph_entry_rejects_malformed() {
        assert!(
            parse_graph_entry("sock /a/b.sock\nnonce deadbeef\n").is_none(),
            "short nonce"
        );
        assert!(
            parse_graph_entry(
                "nonce {}\n"
                    .replace("{}", &LaunchNonce::generate().to_hex())
                    .as_str()
            )
            .is_none(),
            "no sock"
        );
        let good = format!("sock /x.sock\nnonce {}\n", LaunchNonce::generate().to_hex());
        assert!(parse_graph_entry(&good).is_some());
    }

    #[test]
    fn an_id_that_is_no_sid_is_never_joined_into_a_path() {
        let dir = crate::test_scratch::short_dir("the graph dir");
        let nonce = LaunchNonce::generate();
        let minted = SessionId::generate();
        write_graph_entry(dir.path(), &minted, "/x/aterm-1.sock", &nonce);
        assert!(
            dir.path().join("graph").join(minted.as_str()).is_file(),
            "a minted id's entry is written"
        );
        for id in ["../x", "*", "s-0A1B", "s-"] {
            let id = SessionId::new(id);
            write_graph_entry(dir.path(), &id, "/x/aterm-1.sock", &nonce);
            assert_eq!(graph_entry_host_pid(dir.path(), &id), None, "{id:?}");
        }
        assert!(!dir.path().join("x").exists(), "`../x` left graph/");
        let mut names: Vec<_> = std::fs::read_dir(dir.path().join("graph"))
            .expect("graph/")
            .map(|e| e.expect("entry").file_name())
            .collect();
        names.sort();
        assert_eq!(names, [std::ffi::OsString::from(minted.as_str())]);
    }

    #[test]
    fn a_child_that_accepts_nothing_fails_the_forward_fast() {
        let dir = crate::test_scratch::short_dir("a hop's socket");
        let sock = dir.path().join("wedged.sock");
        let _listener = aterm_uds::CtlListener::bind(&sock).expect("bind, never accept");
        let (_client_app, client_relay) = CtlStream::pair().expect("pair");
        let started = std::time::Instant::now();
        let mut relayed = false;
        let result = deliver_then_relay(
            hop_to(&sock, "tok-hex"),
            "text",
            &client_relay,
            &[],
            std::time::Duration::from_millis(400),
            |_, _, _| {
                relayed = true;
                Ok(())
            },
        );
        let error = result.expect_err("a child that accepts nothing is an error");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            error.to_string().contains("accepted no connection"),
            "{error}"
        );
        assert!(!relayed, "nothing is relayed to a child that never served");
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "the probe bound held: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_child_that_refuses_the_probe_never_receives_the_verb() {
        let dir = crate::test_scratch::short_dir("a hop's socket");
        let sock = dir.path().join("refuse.sock");
        let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
        let child = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut rdr = BufReader::new(conn.try_clone().unwrap());
            let mut first = String::new();
            rdr.read_line(&mut first).unwrap();
            conn.write_all(b"ERR auth\n").unwrap();
            let mut rest = String::new();
            let _ = rdr.read_to_string(&mut rest);
            rest
        });
        let (_client_app, client_relay) = CtlStream::pair().expect("pair");
        let result = deliver_then_relay(
            hop_to(&sock, "stale"),
            "key enter",
            &client_relay,
            &[],
            std::time::Duration::from_secs(5),
            |_, _, _| Ok(()),
        );
        assert!(result.is_err(), "a refused probe is a failed forward");
        assert_eq!(child.join().unwrap(), "", "the verb was never sent");
    }

    #[test]
    fn a_probe_surplus_is_refused_before_delivery() {
        for surplus in [true, false] {
            let dir = crate::test_scratch::short_dir("a hop's socket");
            let sock = dir.path().join("surplus.sock");
            let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
            let host = std::thread::spawn(move || {
                let (conn, _) = listener.accept().expect("accept");
                let mut rdr = BufReader::new(conn.try_clone().unwrap());
                let mut probe = String::new();
                rdr.read_line(&mut probe).unwrap();
                let answer: &[u8] = if surplus {
                    b"OK version=host\nOK unasked\n"
                } else {
                    b"OK version=host\n"
                };
                (&conn).write_all(answer).unwrap();
                let mut verb = String::new();
                let _ = rdr.read_line(&mut verb);
                verb
            });
            let (_client_app, client_relay) = CtlStream::pair().expect("pair");
            let mut relayed = false;
            let result = deliver_then_relay(
                hop_to(&sock, "tok"),
                "text",
                &client_relay,
                &[],
                aterm_spec::wait::MUST_HAPPEN,
                |_, _, _| {
                    relayed = true;
                    Ok(())
                },
            );
            let heard = host.join().unwrap();
            if surplus {
                let error = result.expect_err("a surplus fails the forward");
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{error}");
                assert_eq!(heard, "", "the verb was never sent");
                assert!(!relayed, "nothing is relayed past a surplus");
            } else {
                result.expect("a probe answered alone is a forward");
                assert_eq!(heard, "text\n", "the verb follows the probe");
                assert!(relayed, "the relay starts once the verb is sent");
            }
        }
    }

    /// A saturated host still answers: a listener with no lane free writes its busy
    /// line and closes without reading, so the probe's write fails (`EPIPE`) and the
    /// line left behind is the answer [`ForwardHost::serves`] takes as proof the
    /// host is alive.
    #[cfg(unix)]
    #[test]
    fn a_busy_line_left_behind_answers_a_probe_whose_write_failed() {
        let dir = crate::test_scratch::short_dir("a hop's socket");
        let sock = dir.path().join("busy.sock");
        let listener = aterm_uds::CtlListener::bind(&sock).expect("bind");
        let look = CtlStream::connect(&sock).expect("connect");
        let (conn, _) = listener.accept().expect("accept");
        (&conn)
            .write_all(b"ERR control server busy; retry\n")
            .unwrap();
        drop(conn);
        let answer = probe_answer(&look, "tok", std::time::Duration::from_secs(60));
        assert_eq!(
            answer.expect("the line left behind"),
            "ERR control server busy; retry"
        );
    }

    #[test]
    fn a_host_that_goes_away_under_a_forward_leaves_a_typed_line() {
        #[derive(Clone, Copy, Debug, PartialEq)]
        enum Host {
            Gone,
            Reopened,
            ServesOn,
        }
        for case in [Host::Gone, Host::Reopened, Host::ServesOn] {
            let dir = crate::test_scratch::short_dir("a hop's socket");
            let sock = dir.path().join("host.sock");
            let sock_s = sock.to_string_lossy().into_owned();
            let listener = aterm_uds::CtlListener::bind(&sock).expect("bind host");
            let (fresh_path, path) = (dir.path().join("fresh.sock"), sock.clone());
            let host = std::thread::spawn(move || {
                let (conn, _) = listener.accept().expect("accept");
                let mut rdr = BufReader::new(conn.try_clone().unwrap());
                let mut probe = String::new();
                rdr.read_line(&mut probe).unwrap();
                (&conn).write_all(b"OK version=host\n").unwrap();
                let mut verb = String::new();
                rdr.read_line(&mut verb).unwrap();
                let listener = match case {
                    Host::Gone => {
                        drop(listener);
                        drop(conn);
                        return;
                    }
                    Host::Reopened => {
                        let fresh = aterm_uds::CtlListener::bind(&fresh_path).expect("rebind");
                        std::fs::rename(&fresh_path, &path).expect("over the path");
                        drop(listener);
                        let _ = conn.shutdown(std::net::Shutdown::Both);
                        fresh
                    }
                    Host::ServesOn => listener,
                };
                drop(rdr);
                drop(conn);
                loop {
                    let (conn, _) = listener.accept().expect("accept a look");
                    let mut line = String::new();
                    let _ = BufReader::new(&conn).read_line(&mut line);
                    if line.starts_with("stop") {
                        return;
                    }
                    if line.ends_with("version\n") {
                        let _ = (&conn).write_all(b"OK version=host\n");
                    }
                }
            });
            let (client_app, client_relay) = CtlStream::pair().expect("pair");
            let hop = hop_to(&sock, "tok-hex");
            let relay = std::thread::spawn(move || relay_hop(hop, "turn hi", &client_relay, &[]));
            let told = String::from_utf8(read_to_end_of_file(&client_app, |read| {
                format!(
                    "{case:?}: the relay's EOF, after {:?}",
                    String::from_utf8_lossy(read)
                )
            }))
            .expect("the relay passes text");
            drop(client_app);
            relay
                .join()
                .unwrap()
                .expect("a delivered verb is never a failed forward");
            if case != Host::Gone
                && let Ok(conn) = CtlStream::connect(&sock)
            {
                let _ = (&conn).write_all(b"stop\n");
            }
            if case == Host::ServesOn || cfg!(windows) {
                assert_eq!(told, "", "a host that serves on ended it on purpose");
            } else {
                assert_eq!(
                    told,
                    format!("ERR gone pid={} sock={sock_s}\n", std::process::id()),
                    "{case:?}: the host's going is said before the EOF"
                );
            }
            host.join().unwrap();
        }
    }

    #[test]
    fn relay_hop_pipes_handshake_and_response() {
        let dir_guard = crate::test_scratch::short_dir("the child's socket");
        let sock = dir_guard.path().join("child.sock");
        let listener = aterm_uds::CtlListener::bind(&sock).expect("bind child");

        let child = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut rdr = BufReader::new(conn.try_clone().unwrap());
            let mut first = String::new();
            rdr.read_line(&mut first).unwrap();
            assert_eq!(first, "TOKEN tok-hex version\n", "handshake + probe");
            conn.write_all(b"OK version=child\n").unwrap();
            let mut verb = String::new();
            rdr.read_line(&mut verb).unwrap();
            assert_eq!(verb, "screen\n", "the verb follows the probe");
            conn.write_all(b"OK 1\n{\"k\":1}\n").unwrap();
            conn.write_all(&[0xffu8, b'\n']).unwrap();
            conn.flush().unwrap();
            let mut more = String::new();
            let _ = rdr.read_line(&mut more);
            let _ = conn.write_all(more.as_bytes());
            let _ = conn.shutdown(std::net::Shutdown::Both);
        });

        let (client_app, client_relay) = CtlStream::pair().expect("pair");
        let hop = hop_to(&sock, "tok-hex");
        let relay = std::thread::spawn(move || {
            relay_hop(hop, "screen", &client_relay, &[]).expect("relay");
        });

        let mut app_rdr = BufReader::new(client_app.try_clone().unwrap());
        let mut status = String::new();
        app_rdr.read_line(&mut status).unwrap();
        assert_eq!(status, "OK 1\n");
        let mut body = String::new();
        app_rdr.read_line(&mut body).unwrap();
        assert_eq!(body, "{\"k\":1}\n");
        let mut raw = Vec::new();
        let mut byte = [0u8; 2];
        std::io::Read::read_exact(&mut app_rdr, &mut byte).unwrap();
        raw.extend_from_slice(&byte);
        assert_eq!(raw, vec![0xff, b'\n']);

        (&client_app).write_all(b"ping\n").unwrap();
        (&client_app).flush().unwrap();
        let mut echo = String::new();
        app_rdr.read_line(&mut echo).unwrap();
        assert_eq!(echo, "ping\n");

        drop(app_rdr);
        hang_up(client_app);
        join_after_hang_up(relay, "the relay to end on its client's hang-up");
        join_after_hang_up(child, "the stand-in child to finish serving");
    }

    #[test]
    fn one_proxy_hop_preserves_guard_until_original_client_ack() {
        struct Guard(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }

        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (mut original, relay_client) = CtlStream::pair().unwrap();
        let (relay_child, mut child) = CtlStream::pair().unwrap();
        original
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();

        let relay = std::thread::spawn(move || {
            relay_bidirectional(&relay_client, &relay_child, unknown_host()).unwrap()
        });
        let child_alive = std::sync::Arc::clone(&alive);
        let service = std::thread::spawn(move || {
            let _guard = Guard(child_alive);
            let mut reader = BufReader::new(child.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert_eq!(request, "image shot.png\n");
            child.write_all(b"OK 1 1 /remote/shot.png\n").unwrap();
            child
                .write_all(b"ACK-CHALLENGE 00112233445566778899aabbccddeeff\n")
                .unwrap();
            child.flush().unwrap();

            let mut ack = String::new();
            reader.read_line(&mut ack).unwrap();
            assert_eq!(ack.trim_end(), "ACK 00112233445566778899aabbccddeeff");
            let _ = child.shutdown(std::net::Shutdown::Both);
        });

        original.write_all(b"image shot.png\n").unwrap();
        original.flush().unwrap();
        let mut reader = BufReader::new(original.try_clone().unwrap());
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        assert_eq!(response, "OK 1 1 /remote/shot.png\n");
        let mut challenge = String::new();
        reader.read_line(&mut challenge).unwrap();
        assert_eq!(
            challenge,
            "ACK-CHALLENGE 00112233445566778899aabbccddeeff\n"
        );
        assert!(
            alive.load(std::sync::atomic::Ordering::Acquire),
            "relay buffering/flushing the response cannot release the child guard"
        );

        original
            .write_all(
                format!(
                    "{}{}\n",
                    aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX,
                    "00112233445566778899aabbccddeeff"
                )
                .as_bytes(),
            )
            .unwrap();
        original.flush().unwrap();
        join_after_hang_up(service, "the child to read the ACK and release its guard");
        assert!(
            !alive.load(std::sync::atomic::Ordering::Acquire),
            "the original client's ACK crosses the proxy before guard release"
        );
        drop(reader);
        hang_up(original);
        join_after_hang_up(relay, "the relay to end on its client's hang-up");
    }

    /// Which of `relay_bidirectional`'s two error arms a teardown case drives.
    #[cfg(unix)]
    #[derive(Clone, Copy, Debug)]
    enum RelayFault {
        /// The child-bound socket loses its send half: the main thread's
        /// client-to-child write fails the moment the original client sends a
        /// request, while the worker is parked reading a child that never speaks.
        ChildBound,
        /// The client-bound socket loses its send half: the worker's
        /// child-to-client write fails the moment the child answers, while the
        /// main thread is parked reading an original client that never speaks.
        ClientBound,
    }

    /// Tear the real relay down under `fault` while both peers stay open and
    /// otherwise silent, and project what it left onto `RelayTeardown`'s
    /// variables.
    #[cfg(unix)]
    fn relay_teardown_under(
        fault: RelayFault,
    ) -> (std::collections::BTreeMap<&'static str, i64>, bool) {
        let (original, relay_client) = CtlStream::pair().expect("client pair");
        let (relay_child, child_peer) = CtlStream::pair().expect("child pair");
        let client_probe = relay_client.try_clone().expect("clone");
        let child_probe = relay_child.try_clone().expect("clone");
        let faulted = match fault {
            RelayFault::ChildBound => &relay_child,
            RelayFault::ClientBound => &relay_client,
        };
        faulted
            .shutdown(std::net::Shutdown::Write)
            .expect("inject the send fault");
        let (returned_tx, returned_rx) = std::sync::mpsc::channel();
        let relay = std::thread::spawn(move || {
            let result = relay_bidirectional(&relay_client, &relay_child, unknown_host());
            let _ = returned_tx.send(());
            result
        });

        let (mut speaker, said) = match fault {
            RelayFault::ChildBound => (&original, b"screen\n".as_slice()),
            RelayFault::ClientBound => (&child_peer, b"OK reply\n".as_slice()),
        };
        speaker.write_all(said).expect("speak");
        let done = read_to_end_of_file(speaker, |read| {
            format!(
                "{fault:?}: the speaker's end-of-file, after {:?}",
                String::from_utf8_lossy(read)
            )
        })
        .is_empty();
        let returned = returned_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .is_ok();
        let read_open = |probe: &CtlStream| {
            probe.set_nonblocking(true).expect("non-blocking probe");
            let mut byte = [0u8; 1];
            match (&*probe).read(&mut byte) {
                Ok(0) => 0,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 1,
                other => panic!("{fault:?}: unexpected probe read: {other:?}"),
            }
        };
        let observed = std::collections::BTreeMap::from([
            ("child_read_open", read_open(&child_probe)),
            ("client_read_open", read_open(&client_probe)),
            ("done", i64::from(done)),
        ]);
        hang_up(child_peer);
        hang_up(original);
        let _ = join_after_hang_up(relay, "the relay to end once both peers hung up");
        (observed, returned)
    }

    #[test]
    #[cfg(unix)]
    fn relay_teardown_conforms_to_readers_unblock_after_teardown() {
        use aterm_spec::verify::validate_transition_tiered;
        let model = aterm_models_session::relay_teardown_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        for fault in [RelayFault::ChildBound, RelayFault::ClientBound] {
            let (observed, returned) = relay_teardown_under(fault);
            let (ok, why) = validate_transition_tiered(
                &model,
                &[],
                &model.init_state(),
                &observed,
                Some("Teardown"),
                "RelayTeardown(relay_bidirectional)",
            );
            assert!(
                ok,
                "{fault:?}: the relay tore down to {observed:?}, not the model's Teardown \
                 (a read half a pump could still park on) — {why}"
            );
            assert!(
                returned,
                "{fault:?}: the relay did not return: a pump is still parked"
            );
            let (admitted, _) = validate_transition_tiered(
                &buggy,
                &[],
                &buggy.init_state(),
                &observed,
                Some("Teardown"),
                "RelayTeardown(Buggy=1)",
            );
            assert!(
                !admitted,
                "{fault:?}: the shutdown(Write)-only mutant admitted the real teardown, so \
                 this conformance cannot tell the two apart"
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn the_host_pump_ends_on_the_stop_flag_without_a_wake() {
        let (host, _silent_host_peer) = CtlStream::pair().expect("host pair");
        let (client, _client_peer) = CtlStream::pair().expect("client pair");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = std::sync::Arc::clone(&stop);
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let (ended_tx, ended_rx) = std::sync::mpsc::channel();
        let pump = std::thread::spawn(move || {
            let ready = || {
                let _ = waiting_tx.send(());
                readable_unless_stopped(&host, &stopped)
            };
            let leg = copy_host_to_client(&mut &host, &mut &client, ready);
            let _ = ended_tx.send(matches!(leg, HostLeg::HostEnded));
        });
        waiting_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the pump reaches its wait on the silent host");
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let ended = ended_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the pump ends on the stop flag with no wake from any shutdown");
        assert!(ended, "a stopped pump reads as the host's side ending");
        pump.join().expect("the pump");
    }

    /// What the socket `end` names holds for its reader, peeked without blocking
    /// or consuming: the bytes queued (up to 64), `Ok(0)` at its end of file, and
    /// `WouldBlock` for an open socket with nothing queued.
    #[cfg(unix)]
    fn peek_now(end: &CtlStream) -> std::io::Result<usize> {
        use std::os::fd::AsRawFd as _;
        let mut buf = [0u8; 64];
        // SAFETY: `end` is borrowed for the call, `buf` is writable, and `MSG_PEEK | MSG_DONTWAIT` does not block.
        let n = unsafe {
            libc::recv(
                end.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        usize::try_from(n).map_err(|_| std::io::Error::last_os_error())
    }

    #[test]
    #[cfg(unix)]
    fn a_child_that_half_closed_reads_eof_when_the_relay_ends_though_a_copy_survives() {
        let (original, relay_client) = CtlStream::pair().expect("client pair");
        let (relay_child, child) = CtlStream::pair().expect("child pair");
        let forked_copy = relay_child
            .try_clone()
            .expect("a surviving copy of the relay's end");
        let relay = std::thread::spawn(move || {
            relay_bidirectional(&relay_client, &relay_child, unknown_host())
        });

        (&child)
            .write_all(b"OK answer\n")
            .expect("the child answers");
        child
            .shutdown(std::net::Shutdown::Write)
            .expect("the child half-closes");
        let answer = read_to_end_of_file(&original, |read| {
            format!(
                "the answer, then the relay's half-close: {:?} read; the relay's end of \
                 the child {}; the relay {}",
                String::from_utf8_lossy(read),
                match peek_now(&forked_copy) {
                    Ok(0) => "had nothing left to read".to_string(),
                    Ok(n) => format!("still held {n} bytes the relay never read"),
                    Err(e) => format!("could not be peeked: {e}"),
                },
                if relay.is_finished() {
                    "had returned"
                } else {
                    "was still running"
                },
            )
        });
        assert_eq!(String::from_utf8_lossy(&answer), "OK answer\n");
        original
            .shutdown(std::net::Shutdown::Write)
            .expect("the client is done");
        join_after_hang_up(relay, "the relay to end once both sides finished").expect("the relay");

        child.set_nonblocking(true).expect("non-blocking");
        assert_eq!(
            (&child).read(&mut [0u8; 16]).map_err(|e| e.kind()),
            Ok(0),
            "the child read no end-of-file from a relay that ended while a copy of its end lived"
        );
        drop(forked_copy);
    }

    #[test]
    fn edge_tokens_file_is_0600_and_read_is_repeatable() {
        let dir = std::env::temp_dir().join(format!("aterm-edges-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sid = SessionId::generate();
        let (r, w, s) = ("aa".repeat(32), "bb".repeat(32), "cc".repeat(32));
        let file = write_edge_tokens(&dir, &sid, &r, &w, &s).expect("write");
        let path = file.path().to_string_lossy().into_owned();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "edge-token file must be 0600, got {mode:o}");
        }
        let first = read_edge_tokens(&path);
        let second = read_edge_tokens(&path);
        assert_eq!(first, Some((r.clone(), w.clone(), s.clone())), "first read");
        assert_eq!(
            second,
            Some((r, w, s)),
            "second read still succeeds (persists)"
        );
        drop(ChildProxy::new(None));
        assert!(read_edge_tokens(&path).is_some(), "only its own file");
        drop(ChildProxy::new(Some(file)));
        assert!(
            read_edge_tokens(&path).is_none(),
            "removed by owning parent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dead parent's edge file is swept, a live one's never. A parent holds
    /// its file's owner lock for the child's life; the sweep removes a file
    /// whose lock is free (here, its handle dropped without the clean retire's
    /// removal, as a crash leaves it).
    #[cfg(unix)]
    #[test]
    fn a_dead_parents_edge_file_is_swept_and_a_live_ones_never() {
        let dir = std::env::temp_dir().join(format!("aterm-edges-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let hex = |c: &str| c.repeat(32);
        let live = write_edge_tokens(
            &dir,
            &SessionId::generate(),
            &hex("aa"),
            &hex("bb"),
            &hex("cc"),
        )
        .expect("live");
        let crashed = write_edge_tokens(
            &dir,
            &SessionId::generate(),
            &hex("dd"),
            &hex("ee"),
            &hex("ff"),
        )
        .expect("crashed");
        let crashed_path = crashed.path().to_path_buf();
        let forked = crate::parked_fork::ParkedChild::fork();
        drop(crashed);
        let edges = dir.join("edges");
        let empty = edges.join(SessionId::generate().as_str());
        std::fs::write(&empty, b"").unwrap();
        let next = write_edge_tokens(
            &dir,
            &SessionId::generate(),
            &hex("11"),
            &hex("22"),
            &hex("33"),
        )
        .expect("next");
        assert!(crashed_path.exists(), "a provisioning sweeps nothing");
        sweep_dead_edge_files(&edges);
        assert!(
            forked.still_parked(),
            "the sweep ran before the fork's exec"
        );
        forked.release();
        assert!(!crashed_path.exists(), "the dead parent's file is swept");
        assert!(live.path().exists(), "a live parent's file is never swept");
        assert!(empty.exists(), "an empty file may be mid-write: left");
        assert!(next.path().exists());
        drop((live, next));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn publish_session_lifecycle_follows_bind_and_close() {
        let _guard = self_sock_test_guard();
        let dir = std::env::temp_dir().join(format!("aterm-pub-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mirror = std::env::temp_dir().join(format!("aterm-pub-mirror-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&mirror);
        set_mirror_dir_override(Some(mirror.clone()));
        let real_default = aterm_uds::control_socket_dir();
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();

        clear_self_sock();
        publish_session(&sid, &nonce);
        assert!(
            read_graph_entry(&dir, sid_of(&sid)).is_none(),
            "no entry pre-bind"
        );

        let sock = dir.join("aterm-88001.sock").to_string_lossy().into_owned();
        let canon_sock = published_sock_spelling(
            &std::fs::canonicalize(&dir)
                .unwrap()
                .join("aterm-88001.sock")
                .to_string_lossy(),
        );
        set_self_sock(&dir, &sock);
        publish_session(&sid, &nonce);
        assert!(
            flush_publications(aterm_spec::wait::MUST_HAPPEN),
            "the write lands"
        );
        let (got_sock, got_nonce) = read_graph_entry(&dir, sid_of(&sid)).expect("published");
        assert_eq!(got_sock, canon_sock);
        assert!(got_nonce.ct_eq(&nonce));

        assert!(
            read_graph_entry(&mirror, sid_of(&sid)).is_some(),
            "mirror lands in the scratch override dir"
        );
        if let Some(real) = real_default.as_ref() {
            assert!(
                read_graph_entry(real, sid_of(&sid)).is_none(),
                "the mirror must NEVER write into the user's real control dir"
            );
        }

        unpublish_session(&sid);
        assert!(
            flush_publications(aterm_spec::wait::MUST_HAPPEN),
            "the retire lands"
        );
        assert!(
            read_graph_entry(&dir, sid_of(&sid)).is_none(),
            "retired on close"
        );
        assert!(
            read_graph_entry(&mirror, sid_of(&sid)).is_none(),
            "mirror entry retired too"
        );
        if let Some(real) = real_default.as_ref() {
            assert!(
                read_graph_entry(real, sid_of(&sid)).is_none(),
                "still nothing in the real control dir after retire"
            );
        }
        clear_self_sock();
        set_mirror_dir_override(None);
        let _ = std::fs::remove_dir_all(&mirror);

        std::fs::write(dir.join("aterm-88001.token"), "  feed1234\n").unwrap();
        assert_eq!(read_sibling_token(&sock).as_deref(), Some("feed1234"));
        std::fs::write(dir.join("aterm-88001.token"), "\n").unwrap();
        assert_eq!(read_sibling_token(&sock), None);
        let other = dir.join("aterm-88002.sock").to_string_lossy().into_owned();
        assert_eq!(read_sibling_token(&other), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_publication_is_queued_off_the_caller_and_kept_in_order() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                clear_self_sock();
                let _ = flush_publications(aterm_spec::wait::MUST_HAPPEN);
                set_mirror_dir_override(None);
            }
        }
        let _guard = self_sock_test_guard();
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let (dir, mirror) = (scratch.path().join("own"), scratch.path().join("mirror"));
        for d in [&dir, &mirror] {
            std::fs::create_dir_all(d).expect("scratch dir");
        }
        set_mirror_dir_override(Some(mirror.clone()));
        let _reset = Reset;
        set_self_sock(&dir, &dir.join("aterm-88003.sock").to_string_lossy());

        let (held, release) = std::sync::mpsc::sync_channel(0);
        queue_publication(Publication::Flush(held));
        let (kept, closed) = (SessionId::generate(), SessionId::generate());
        publish_session(&kept, &LaunchNonce::generate());
        publish_session(&closed, &LaunchNonce::generate());
        unpublish_session(&closed);
        assert!(
            read_graph_entry(&dir, sid_of(&kept)).is_none(),
            "publish_session wrote in the caller's turn"
        );
        let stranger = SessionId::generate();
        let elsewhere = {
            let stranger = stranger.clone();
            std::thread::spawn(move || {
                publish_session(&stranger, &LaunchNonce::generate());
                (
                    self_sock_path(),
                    flush_publications(std::time::Duration::ZERO),
                )
            })
            .join()
            .expect("another test's thread")
        };
        assert_eq!(
            elsewhere,
            (None, true),
            "another test read this one's socket or waited behind its queue"
        );
        release
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the publication thread reached the held write");
        assert!(
            flush_publications(aterm_spec::wait::MUST_HAPPEN),
            "the queue drained"
        );
        assert!(
            read_graph_entry(&dir, sid_of(&kept)).is_some(),
            "the publish landed"
        );
        assert!(
            read_graph_entry(&mirror, sid_of(&kept)).is_some(),
            "and its mirror"
        );
        assert!(
            read_graph_entry(&dir, sid_of(&closed)).is_none()
                && read_graph_entry(&mirror, sid_of(&closed)).is_none(),
            "a session retired behind its publish leaves no entry"
        );
        assert!(
            read_graph_entry(&dir, sid_of(&stranger)).is_none(),
            "another test's session was published into this one's dir"
        );
    }

    /// A sibling's token file is read the way every control client reads its
    /// own: one planted as a symlink is refused rather than followed, and one
    /// larger than any token is refused rather than read whole.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_or_oversized_sibling_token_is_refused() {
        let dir = std::env::temp_dir().join(format!("aterm-sibtok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let sock = dir.join("aterm-88003.sock").to_string_lossy().into_owned();
        let token = dir.join("aterm-88003.token");
        let elsewhere = dir.join("elsewhere");
        std::fs::write(&elsewhere, "feed1234\n").expect("a token-shaped file");
        std::os::unix::fs::symlink(&elsewhere, &token).expect("plant");
        assert_eq!(read_sibling_token(&sock), None, "a symlink is followed");
        std::fs::remove_file(&token).expect("unplant");
        std::fs::write(&token, "f".repeat(64 * 1024)).expect("oversized");
        assert_eq!(read_sibling_token(&sock), None, "an oversized file is read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_quit_retires_only_its_own_graph_entries() {
        let dir = crate::test_scratch::short_dir("a hop's socket");
        let ours = dir.path().join("aterm-cand.sock");
        let theirs = dir.path().join("aterm-parent.sock");
        let (mine, parents) = (SessionId::generate(), SessionId::generate());
        write_graph_entry(
            dir.path(),
            &mine,
            ours.to_str().unwrap(),
            &LaunchNonce::generate(),
        );
        write_graph_entry(
            dir.path(),
            &parents,
            theirs.to_str().unwrap(),
            &LaunchNonce::generate(),
        );
        for sid in [&mine, &parents] {
            remove_own_graph_entry(dir.path(), sid, ours.to_str().unwrap());
        }
        let exists = |sid: &SessionId| entry_path(dir.path(), sid).is_some_and(|p| p.exists());
        assert!(!exists(&mine), "its own entry goes");
        assert!(exists(&parents), "the parent's entry stays with the parent");
    }
}
