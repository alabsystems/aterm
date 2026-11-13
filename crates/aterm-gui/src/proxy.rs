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
            return rendezvous_mirror_dir_with(sock_dir, Some(over));
        }
    }
    rendezvous_mirror_dir_with(sock_dir, aterm_uds::control_socket_dir())
}

fn rendezvous_mirror_dir_with(sock_dir: &Path, default_dir: Option<PathBuf>) -> Option<PathBuf> {
    let rv = default_dir?;
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
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    open_private_opts(path, true)
}

/// Open or create a private `0600` sidecar file without truncating before its lock is taken.
fn open_private_untruncated(path: &Path) -> std::io::Result<std::fs::File> {
    open_private_opts(path, false)
}

#[cfg(unix)]
fn open_private_opts(path: &Path, truncate: bool) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(truncate)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private sidecar is not a regular file",
        ));
    }
    Ok(file)
}

/// Windows twin of [`open_private_opts`] (ACL-inherited; see the Unix docs).
#[cfg(windows)]
fn open_private_opts(path: &Path, truncate: bool) -> std::io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(truncate)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private sidecar is not a regular file",
        ));
    }
    Ok(file)
}

const MAX_PRIVATE_TEXT_BYTES: u64 = 16 * 1024;

/// Read a bounded regular UTF-8 sidecar file (`graph/<sid>`, `edges/<sid>`) without following symlinks or blocking on a FIFO.
pub(crate) fn read_private_text(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private sidecar is a symlink",
        ));
    }
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > MAX_PRIVATE_TEXT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private sidecar is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(MAX_PRIVATE_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PRIVATE_TEXT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private sidecar exceeds size bound",
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private sidecar is not UTF-8",
        )
    })
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
    let Ok(body) = read_private_text(&path) else {
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
        if !ent.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let path = ent.path();
        if let Ok(body) = read_private_text(&path)
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
    let sid = Sid::parse(child_sid.as_str())?;
    let dir = sock_dir.join("edges");
    if crate::control_auth::ensure_private_dir(&dir).is_err() {
        return None;
    }
    let path = dir.join(sid.as_str());
    let body = format!("read {read_hex}\nwrite {write_hex}\nsignal {signal_hex}\n");
    let f = open_private_untruncated(&path).ok()?;
    let lock = take_owner_lock(f)?;
    let mut secret: &std::fs::File = owner_file(&lock);
    if secret
        .set_len(0)
        .and_then(|()| secret.write_all(body.as_bytes()))
        .is_err()
    {
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
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
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
    let body = read_private_text(Path::new(path)).ok()?;
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
    let body = read_private_text(&entry_path(sock_dir, sid)?).ok()?;
    aterm_types::control_socket::graph_entry_pid(&body)
}

/// Test-only: view `id` as a [`Sid`] for [`read_graph_entry`].
#[cfg(test)]
pub(crate) fn sid_of(id: &SessionId) -> Sid<'_> {
    Sid::parse(id.as_str()).expect("a minted id is a Sid")
}

/// Read `(sock_path, nonce)` from a session's discovery entry.
pub(crate) fn read_graph_entry(sock_dir: &Path, sid: Sid<'_>) -> Option<(String, LaunchNonce)> {
    let body = read_private_text(&graph_path(sock_dir, sid)).ok()?;
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

/// Hang detector for required admission and follow-up probes of a forwarding host.
const FORWARD_HANG: std::time::Duration = std::time::Duration::from_secs(60);

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
        FORWARD_HANG,
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
    started: impl FnOnce() -> std::time::Instant,
) -> std::io::Result<()> {
    let line = probe_answer(host, token, within, started, || false)?;
    if is_connection_refusal(&line) {
        return Err(std::io::Error::other(format!("the host answered {line:?}")));
    }
    host.set_read_timeout(None)
}

fn probe_answer(
    host: &CtlStream,
    token: &str,
    within: std::time::Duration,
    started: impl FnOnce() -> std::time::Instant,
    abandoned: impl Fn() -> bool,
) -> std::io::Result<String> {
    let mut hello = String::with_capacity(token.len() + 16);
    hello.push_str("TOKEN ");
    hello.push_str(token);
    hello.push_str(" version\n");
    let sent = (&*host)
        .write_all(hello.as_bytes())
        .and_then(|()| (&*host).flush());
    let answer = read_probe_line(host, within, started(), abandoned);
    match sent {
        Ok(()) => answer,
        Err(e) => answer.map_err(|_| e),
    }
}

/// The host's one-line probe answer, waited for until `within` after `start`. The wait is cut
/// into [`HANGUP_POLL`](crate::control::HANGUP_POLL) slices and `abandoned` is asked before each:
/// once it says so the read ends (`ConnectionAborted`) without sitting out `within`.
fn read_probe_line(
    host: &CtlStream,
    within: std::time::Duration,
    start: std::time::Instant,
    abandoned: impl Fn() -> bool,
) -> std::io::Result<String> {
    use std::io::{Error, ErrorKind};
    const MAX_PROBE_LINE: usize = 4096;
    let deadline = start
        .checked_add(within)
        .unwrap_or_else(|| start + std::time::Duration::from_secs(365 * 24 * 3600));
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
        if abandoned() {
            return Err(Error::new(
                ErrorKind::ConnectionAborted,
                "the answer has nobody left to read it",
            ));
        }
        let slice = (deadline - now).min(crate::control::HANGUP_POLL);
        let _ = host.set_read_timeout(Some(slice));
        let read = match (&*host).read(&mut chunk) {
            Ok(0) => {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "the host closed the connection before answering",
                ));
            }
            Ok(n) => &chunk[..n],
            // A slice that ran out is judged above: the deadline, then `abandoned`.
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::Interrupted | ErrorKind::WouldBlock | ErrorKind::TimedOut
                ) =>
            {
                continue;
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
    /// How long a look waits for the host's probe answer: what its admission was given.
    within: std::time::Duration,
}

impl ForwardHost {
    /// The line that tells a client its host went away, or `None` while the host serves on.
    /// A look waits `within` on a host that queues its connect and never answers. `client_left`
    /// ends that wait within [`HANGUP_POLL`](crate::control::HANGUP_POLL): the line then has
    /// nobody to read it, and there is none.
    fn gone_line(&self, client_left: impl Fn() -> bool) -> Option<String> {
        self.gone_line_with_clock(std::time::Instant::now, client_left)
    }

    fn gone_line_with_clock(
        &self,
        started: impl FnOnce() -> std::time::Instant,
        client_left: impl Fn() -> bool,
    ) -> Option<String> {
        let pid = self.pid?;
        if client_left() {
            return None;
        }
        let serves = self.serves(pid, started, &client_left);
        (!serves && !client_left())
            .then(|| aterm_types::control_verbs::host_gone_line(pid, &self.sock))
    }

    fn serves(
        &self,
        pid: u32,
        started: impl FnOnce() -> std::time::Instant,
        abandoned: impl Fn() -> bool,
    ) -> bool {
        let Ok(look) = CtlStream::connect(&self.sock) else {
            return false;
        };
        let answered = aterm_uds::fdpass::peer_pid(&look).ok();
        if answered.is_some_and(|answered| answered != pid) {
            return false;
        }
        probe_answer(&look, &self.token, self.within, started, abandoned).is_ok()
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
    deliver_then_relay_with_clock(
        hop,
        line,
        client,
        client_prebuffered,
        accept_within,
        relay,
        std::time::Instant::now,
    )
}

fn deliver_then_relay_with_clock(
    hop: Hop,
    line: &str,
    client: &CtlStream,
    client_prebuffered: &[u8],
    accept_within: std::time::Duration,
    relay: impl FnOnce(&CtlStream, &CtlStream, ForwardHost) -> std::io::Result<()>,
    started: impl FnOnce() -> std::time::Instant,
) -> std::io::Result<()> {
    let Hop {
        stream: child,
        sock,
        token,
        listener,
    } = hop;
    prove_host_serving(&child, &token, accept_within, started)?;
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
        within: accept_within,
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
                    // Asked before the look and through its wait: a client that hangs up
                    // meanwhile ends the relay now, not when the look's wait runs out.
                    let client_left =
                        || client_gone.load(SeqCst) || aterm_uds::hangup::peer_closed(client);
                    if let Some(line) = host.gone_line(client_left) {
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
#[path = "proxy_tests.rs"]
mod tests;
