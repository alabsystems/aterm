// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-process `@<sid>` proxy forward: turns the per-process control socket
//! into one address space spanning the recursion tree, so an aterm can reach a
//! session any other same-uid aterm hosts: an inner aterm it spawned, that
//! one's own children, or an instance the user opened separately.
//!
//! ## How a forward works
//!
//! Every instance publishes a graph entry for every session it hosts, keyed by
//! sid and pointing at its own socket ([`publish_session`] /
//! [`unpublish_session`], fed by [`set_self_sock`] at bind), into the shared
//! runtime dir; a nested instance inherits its parent's. When an Owner-class
//! request names a session this process does not host, the control server
//! reads that entry ([`read_graph_entry`]), confines its socket to the runtime
//! dir, and the forward then:
//!
//! 1. connects to that socket and reads the hosting instance's own per-launch
//!    token beside it ([`connect_hop`]): the credential a same-uid `aterm ctl
//!    --pid` client reads directly, so the relay grants nothing the caller
//!    could not take itself. The connect is the liveness test: nothing
//!    listening there, or no token to read, is no hop, and the request is
//!    answered here (`ERR no such session`) with nothing sent;
//! 2. presents `TOKEN <token> version` and waits for its answer (the connect
//!    probe), then sends the caller's line as it came, selector kept, so the
//!    host resolves the session among its own tabs; and
//! 3. relays bytes transparently in both directions until either side closes.
//!
//! The relay never parses framing, so it carries the styled `screen` JSON, the
//! `subscribe cells/bytes` push streams, `feed-bin` payloads and every other
//! verb verbatim.
//!
//! ## One hop
//!
//! The host serves the line among its own sessions: the entry was published by
//! the instance at that socket, so the sid is hosted there, or the entry is
//! stale and names the host's own socket, which its self-dial guard refuses. A
//! stale entry pointing back at the forwarder's own socket is refused the same
//! way, so a removed session degrades to `ERR no such session`, never a loop.
//! An edge-scoped connection never forwards (it falls through to local
//! resolution), so an edge cannot be escalated beyond its grant.
//!
//! ## Spawned children
//!
//! A child aterm is reached like any other host, with its own token, so every
//! verb an Owner may send its own instance reaches it. The per-op edge tokens
//! this aterm mints for each child it spawns (`ChildProvision`) are written to
//! the 0600 file `ATERM_EDGE_TOKENS` names ([`write_edge_tokens`]), held for
//! the session's life ([`ChildProxy`]), which the child installs as edges onto
//! the session it adopts; the forward never presents them.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use aterm_session::{LaunchNonce, SessionId};
use aterm_types::control_request::Sid;
use aterm_uds::CtlStream;

#[cfg(unix)]
use crate::native_document_host::HeldAdvisoryLock;

/// The graph-entry filename for a session id, under `<sock_dir>/graph/`: the
/// one place a session id is joined into a path. Only a [`Sid`] (`s-` and
/// lower-case hex) reaches it, so the path it names stays inside `graph/`.
fn graph_path(sock_dir: &Path, sid: Sid<'_>) -> std::path::PathBuf {
    sock_dir.join("graph").join(sid.as_str())
}

/// [`graph_path`] for a session id this process holds, or `None` when the id
/// is no [`Sid`]: such an id is never written, read or removed as an entry.
fn entry_path(sock_dir: &Path, sid: &SessionId) -> Option<std::path::PathBuf> {
    Sid::parse(sid.as_str()).map(|sid| graph_path(sock_dir, sid))
}

/// This instance's discovery state: its own bound control socket, the thread
/// that writes its graph entries, and (under test) where its mirror goes. One
/// per process ([`PROCESS_DISCOVERY`]); a test may hold one of its own
/// ([`self_sock_test_guard`]).
struct Discovery {
    /// This instance's own bound control socket `(sock_dir, sock_path)`, recorded
    /// by the control server at bind time. `None` until (or unless) a socket is
    /// bound. An `RwLock<Option<..>>` (not a `OnceLock`): a quit takes it back
    /// ([`retire_own_sessions`]).
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

/// THE process's discovery state.
static PROCESS_DISCOVERY: Discovery = Discovery::new();

/// The discovery state this thread's calls answer to: [`PROCESS_DISCOVERY`], in
/// every build. A test thread may hold one of its own ([`self_sock_test_guard`])
/// and lend it to a thread it spawns ([`OwnDiscovery::lend`]); a publication
/// thread answers to the state that started it. So a test that binds or
/// publishes is seen by no other test: another test's sessions publish nothing
/// into its dir, and another test's Commit (`flush_publications`) never waits
/// behind its queue — the production rule, one state per process, unbent.
fn discovery() -> &'static Discovery {
    #[cfg(test)]
    if let Some(own) = OWN_DISCOVERY.get() {
        return own;
    }
    &PROCESS_DISCOVERY
}

#[cfg(test)]
thread_local! {
    /// TESTS ONLY: the state [`discovery`] answers on this thread, when not the
    /// process's.
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

/// Record this instance's bound control socket so session registration can
/// publish per-session graph entries ([`publish_session`]) and the sibling
/// forward can refuse to dial itself. Called once by the control server after a
/// successful bind.
///
/// The path is stored canonical: the self-dial guard compares it against
/// `confine_proxy_sock`'s `canonicalize(dir)`-rooted output, and a raw path under
/// a symlinked ancestor (`/var` → `/private/var`) would let a self-pointing entry
/// relay requests back into this server without bound. If canonicalization
/// fails, it falls back to a canonical dir joined with the file name.
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
    // Sessions publish from here on: their writes' thread starts now, on the
    // binder's thread, not in a tab's landing turn.
    let _ = publisher();
}

/// This instance's own bound socket path, or `None` when no socket is bound.
#[must_use]
pub(crate) fn self_sock_path() -> Option<String> {
    discovery()
        .self_sock
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|(_, p)| p.clone())
}

/// Test-only: clear the recorded self socket (this thread's state's,
/// [`discovery`]): no session registered after it publishes.
#[cfg(test)]
pub(crate) fn clear_self_sock() {
    *discovery()
        .self_sock
        .write()
        .unwrap_or_else(|p| p.into_inner()) = None;
}

/// TESTS ONLY: give this thread discovery state of its own until the guard
/// drops — its own self socket, publication queue and thread, and mirror
/// override ([`discovery`]) — so no other test sees what this one binds,
/// publishes or holds, and this one sees none of theirs. A thread the test
/// spawns that binds or publishes takes it along ([`OwnDiscovery::lend`]).
/// Such tests also run one at a time. The state is leaked (a few words per
/// test); its publication thread, if one started, ends after what was queued.
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

/// TESTS ONLY: the guard [`self_sock_test_guard`] returns.
#[cfg(test)]
pub(crate) struct OwnDiscovery {
    own: &'static Discovery,
    previous: Option<&'static Discovery>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(all(test, unix))]
impl OwnDiscovery {
    /// This test's discovery state, for a thread it spawns
    /// ([`LentDiscovery::enter`] there).
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

/// TESTS ONLY: a test's discovery state ([`OwnDiscovery::lend`]), sent to a
/// thread it spawns. Only the unix conformance suites lend one.
#[cfg(all(test, unix))]
#[derive(Clone, Copy)]
pub(crate) struct LentDiscovery(&'static Discovery);

#[cfg(all(test, unix))]
impl LentDiscovery {
    /// This thread answers to the lender's state until the guard drops.
    pub(crate) fn enter(self) -> EnteredDiscovery {
        EnteredDiscovery(OWN_DISCOVERY.replace(Some(self.0)))
    }
}

/// TESTS ONLY: the guard [`LentDiscovery::enter`] returns.
#[cfg(all(test, unix))]
pub(crate) struct EnteredDiscovery(Option<&'static Discovery>);

#[cfg(all(test, unix))]
impl Drop for EnteredDiscovery {
    fn drop(&mut self) {
        OWN_DISCOVERY.set(self.0);
    }
}

/// Publish the discovery graph entry for a session this instance hosts, so a
/// sibling instance (and the flagless `aterm-ctl` client) can resolve `@<sid>`
/// to our socket. No-op until the control socket is bound ([`set_self_sock`]);
/// best-effort. Uses [`publish_graph_entry`], so an instance on an explicit
/// `--control-sock` also lands its entry in the default rendezvous dir the
/// client reads. Called at the session-registration seam for every session.
///
/// The write is queued ([`queue_publication`]): on the event loop it cost
/// 0.5–1 ms of every tab's landing turn. The socket is read here, at the
/// registration, so the bind order holds as before (published ⟹ bound: a
/// registration that reads no socket is in `publish_discovery`'s snapshot),
/// and the one queue keeps a session's publish ahead of its
/// [`unpublish_session`]. It is queued while the socket's record is still
/// read-held, as the inline write was made: whatever clears the record
/// (a test's reset) finds it queued, and a [`flush_publications`] after that
/// waits for it.
pub(crate) fn publish_session(sid: &SessionId, nonce: &LaunchNonce) {
    let guard = discovery()
        .self_sock
        .read()
        .unwrap_or_else(|p| p.into_inner());
    let Some((dir, sock)) = guard.as_ref() else {
        return;
    };
    // On disk goes the spelling every reader can dial and dedupe. The
    // recorded path is canonical, which on Windows is the verbatim
    // `\\?\C:\…` form the bind-time publish does not use; un-verbatim it so
    // one instance is not listed twice. The in-memory record stays canonical.
    queue_publication(Publication::Publish {
        dir: dir.clone(),
        sid: sid.clone(),
        sock: published_sock_spelling(sock),
        nonce: *nonce,
    });
    drop(guard);
}

/// One discovery write, in the order its sessions registered and closed.
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

/// The `aterm-publish` thread's queue, and how many writes on it have not
/// landed.
struct Publisher {
    tx: std::sync::mpsc::Sender<Publication>,
    pending: std::sync::atomic::AtomicUsize,
}

/// The publication thread of this thread's discovery state ([`discovery`]),
/// started with the bind ([`set_self_sock`], on the control thread), so no
/// tab's landing pays for the thread. `None` when it could not start: then each
/// write runs where it is asked, as before.
fn publisher() -> Option<&'static Publisher> {
    let state = discovery();
    state
        .publisher
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Publication>();
            let spawned = crate::qos::spawn_lane_thread(
                "aterm-publish",
                // A new tab's shell finds its instance through its entry
                // (`aterm ctl`'s self-location), so it lands promptly.
                crate::qos::Role::Responsive,
                move || {
                    // Its writes read the state that started it (a test's own).
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

/// Hand `publication` to the publication thread; written here when there is
/// none.
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

/// Wait up to `within` for earlier queued operations to be processed. This
/// fences their order, not best-effort filesystem success. A Commit refuses
/// false; a quit queues its guarded retirements first, so they stay ordered
/// even if the wait expires and process exit leaves cleanup to the stale sweep.
/// Free when nothing is queued. `false` when the operations were still behind.
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

/// Stop session publication and queue this instance's guarded retirements.
/// Taking the socket's write lock waits for every earlier read-held enqueue;
/// later registrations see no socket. Drop that lock before enqueueing, since
/// the no-worker fallback writes inline and the owner guard reads self_sock_path.
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

/// The spelling of our socket path a graph entry carries: the canonical path
/// with a Windows verbatim-disk prefix removed (`\\?\C:\x` → `C:\x`). Every
/// Win32 file API and the client's dialer accept the shorter form, and it is
/// the form the bind-time publish and the readdir already use. Unix paths and
/// verbatim UNC paths pass through unchanged.
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

/// Remove a closed session's discovery entry (best-effort) so siblings stop
/// resolving it. No-op when no socket is bound. A leftover (crash, missed
/// close) is harmless: the forward's connect finds nothing listening, or the
/// instance at that socket re-checks its own store, so it can only produce
/// `ERR no such session`, never a wrong target. Queued behind the session's
/// publish ([`publish_session`]).
pub(crate) fn unpublish_session(sid: &SessionId) {
    let guard = discovery()
        .self_sock
        .read()
        .unwrap_or_else(|p| p.into_inner());
    let Some((dir, _)) = guard.as_ref() else {
        return;
    };
    // Queued under the read, as [`publish_session`] is.
    queue_publication(Publication::Retire {
        dir: dir.clone(),
        sid: sid.clone(),
    });
    drop(guard);
}

/// Read the per-launch auth token of the instance whose socket is `sock_path`
/// (`<dir>/aterm-<pid>.sock` → `<dir>/aterm-<pid>.token`; an explicit socket →
/// `<sock>.token`; 0600, same-uid). This is the credential a same-uid
/// `aterm-ctl --sock <path>` client reads, through the same function
/// ([`aterm_ctl::read_token_beside`]), so presenting it grants nothing the
/// caller could not obtain directly. `None` (fail closed) for an unreadable
/// token. The forward reads it once its connection to the host exists
/// ([`connect_hop`]).
///
/// The reader never follows a symlink at the token's name, takes only a
/// regular file and reads no more than a token's bound, since a
/// same-uid-writable directory can plant a link or a runaway file. Candidates
/// follow [`aterm_types::control_socket::token_names_for_sock`] in its order:
/// the per-socket file first, the shared `aterm.token` only when that one is
/// absent or unreadable; a per-socket file that exists but is malformed ends
/// the search. A foreign token grants nothing: the sibling compares `AUTH`
/// against the token it minted in memory.
#[must_use]
pub(crate) fn read_sibling_token(sock_path: &str) -> Option<String> {
    aterm_ctl::read_token_beside(sock_path).ok()
}

/// Write the discovery graph entry an inner aterm publishes at bind time so its
/// parent can reach it: `<sock_dir>/graph/<self-sid>` (0600) with three lines
/// `sock <abs-path>\nnonce <hex>\npid <n>\n`. The `pid` is this (hosting)
/// process's, so the flagless client's `instances`/`ls` can report a pid even
/// for an explicit-socket instance whose file name encodes none. Edge tokens
/// are never written here; they live only in the child's 0600 edge-token file
/// ([`write_edge_tokens`]), whose path rides `ATERM_EDGE_TOKENS`. Best-effort:
/// a write failure only means the parent cannot reach us by proxy, and an id
/// that is no [`Sid`] is never written ([`entry_path`]).
///
/// Writes one dir only; [`publish_graph_entry`] also mirrors into the default
/// rendezvous dir when the instance runs on an explicit `--control-sock`.
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
    // 0700 + owner-verified, like the sibling `images/` subdir (control_auth).
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

/// Test-only: redirect the rendezvous mirror target to `dir` (or clear it with
/// `None`). Production leaves it `None` and the mirror lands in
/// `aterm_uds::control_socket_dir()`; a test points it at a scratch dir so it
/// never touches the user's real control dir. It is the test's own discovery
/// state's ([`self_sock_test_guard`]), so no other test's [`rendezvous_mirror_dir`]
/// reads it.
#[cfg(test)]
pub(crate) fn set_mirror_dir_override(dir: Option<PathBuf>) {
    assert_own_discovery("redirects the mirror");
    *discovery()
        .mirror_dir_override
        .write()
        .unwrap_or_else(|p| p.into_inner()) = dir;
}

/// The default control dir (`aterm_uds::control_socket_dir`) to also publish a
/// graph entry into, or `None` when `sock_dir` already is it or the per-user
/// base cannot be resolved. The flagless `aterm-ctl` client reads only the
/// default dir, so an instance on an explicit `--control-sock` must mirror
/// here to stay discoverable.
fn rendezvous_mirror_dir(sock_dir: &Path) -> Option<PathBuf> {
    // Under test, a set override stands in for the real default dir so no unit
    // test ever writes into the user's real control dir (production: no override).
    #[cfg(test)]
    {
        // Receiver named on the acquisition line: the lock-order census
        // resolves identities from the `.read()` line's receiver.
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

/// Best-effort same-directory test: canonical when both resolve (so a symlinked
/// ancestor like `/var` → `/private/var` does not read as different), else
/// lexical. A false "different" costs one duplicate write; a false "same" skips
/// a redundant mirror.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Publish a session's discovery graph entry into the instance's own `sock_dir`
/// and, when that is not the default rendezvous dir (an explicit
/// `--control-sock`), into the default dir too, so the flagless `aterm-ctl`
/// client can self-locate and enumerate this instance. The entry carries the
/// absolute `sock <path>`. Best-effort and idempotent: a default instance
/// writes once, and an unwritable default dir degrades to the own-dir entry.
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

/// Retire a session's discovery entry from both its own dir and the mirrored
/// default rendezvous dir (the inverse of [`publish_graph_entry`]).
/// Best-effort hygiene: a forward to a stale entry finds no hop at its connect
/// and the client re-probes liveness.
pub(crate) fn retire_graph_entry(sock_dir: &Path, sid: &SessionId) {
    remove_graph_entry(sock_dir, sid);
    if let Some(rv) = rendezvous_mirror_dir(sock_dir) {
        remove_graph_entry(&rv, sid);
    }
}

/// Open (create/truncate) a private sidecar file for writing. Unix: mode
/// `0600`. Windows: a plain create; the file inherits the private dir's
/// per-user ACL.
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

/// Remove this session's graph entry (best-effort) on graceful exit so a dead
/// session's socket path is not left for a parent to dial. Hygiene only: a
/// forward to a stale entry finds no hop at its connect.
pub(crate) fn remove_graph_entry(sock_dir: &Path, sid: &SessionId) {
    if let Some(path) = entry_path(sock_dir, sid) {
        let _ = std::fs::remove_file(path);
    }
}

/// Retire `sid`'s discovery entry only while it names this instance's socket
/// (`our_sock`, in any spelling the entry could carry). An entry another live
/// process wrote (the parent of a rolled-back update candidate, which still
/// serves the carried ids) is left for its owner, as is an unreadable entry;
/// the stale sweep retires dead ones.
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

/// Sweep dead discovery entries: remove any `graph/<sid>` whose recorded socket
/// has no live listener (a crashed session that never ran
/// `remove_graph_entry`). Runs beside `control_auth::sweep_stale_instances` at
/// bind and after an update handoff, in the shared default dir; best-effort.
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

/// Write the parent→child edge-token secrets to a 0600 file at
/// `<sock_dir>/edges/<child-sid>` (lines `read <hex>`, `write <hex>`,
/// `signal <hex>`) and return it, or `None` if the private dir or file cannot be
/// created. The bearer tokens live only here, never in inheritable env.
///
/// The child inherits the path in `ATERM_EDGE_TOKENS`, pinned in its shell env,
/// and the file is not consumed on read, so a child aterm relaunched in the same
/// shell re-installs the parent edges. `ATERM_EDGE_TOKENS` stays deny-listed
/// across a new aterm hop. The parent owns the file: the [`ChildProxy`] that
/// records it removes it when the child's session ends.
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
    // No sweep here: this process's own files are live (it holds their
    // locks), so a sweep per spawn only ever found other processes' leftovers,
    // at one open+flock per file of every instance sharing the dir, on the
    // event loop. [`start_edge_sweeper`] does it off the loop.
    let path = dir.join(child_sid.as_str());
    let body = format!("read {read_hex}\nwrite {write_hex}\nsignal {signal_hex}\n");
    let f = open_private(&path).ok()?;
    // The owner's lock, taken before the secret is written: the parent holds it
    // for the child's life ([`EdgeFile`]), so a later sweep can tell a live
    // parent's file from a dead one's ([`sweep_dead_edge_files`]).
    let Some(lock) = take_owner_lock(f) else {
        // The open created the file, and no `ChildProxy` will record a path
        // this returns no name for, so nothing else would ever remove it.
        let _ = std::fs::remove_file(&path);
        return None;
    };
    let mut secret: &std::fs::File = owner_file(&lock);
    if secret.write_all(body.as_bytes()).is_err() {
        // A partial write left some secret in the file. Its name goes first,
        // then the lock (by `LOCK_UN`, the guard's drop).
        let _ = std::fs::remove_file(&path);
        drop(lock);
        return None;
    }
    Some(EdgeFile { path, _lock: lock })
}

/// One edge-token file this aterm wrote ([`write_edge_tokens`]) and the open
/// handle whose `flock` marks its owner alive. Held by the [`ChildProxy`] for the
/// child's life and released when it drops (after the file is removed) by
/// `LOCK_UN`, not by the close alone ([`OwnerLock`]): a child any thread forks
/// while it is held keeps a copy of the description until it execs, and a
/// close-only release would make the sweep read the dead parent as live.
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
    /// The file's absolute path — what rides the child's env.
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
fn owner_file(lock: &OwnerLock) -> &std::fs::File {
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
/// An empty file is left (its writer may be between create and lock), as is a
/// file the probe cannot read. A file never locked reads as a dead owner's.
/// Run by [`start_edge_sweeper`], never on a spawn.
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
/// A dead parent's file authorizes nothing (its child's `(sid, nonce)` is
/// never reissued), so only the directory's size waits on it.
#[cfg(unix)]
const EDGE_SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Sweep `<control dir>/edges` of dead parents' files ([`sweep_dead_edge_files`])
/// once now and then every [`EDGE_SWEEP_EVERY`], on one detached thread per
/// process at [`crate::qos::Role::Housekeeping`]: nothing waits on it, it holds
/// no lock anyone waits on (a probe's lock is let go before its close), and a
/// late pass only leaves files a while longer. It ran on every spawn, on the
/// event loop, until 2026-10-15: 14-15 us per file, with one file per tab of
/// every instance sharing the directory. Later calls do nothing.
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

/// Read the three edge-token hexes `(read, write, signal)` from the 0600 file at
/// `path` (written by [`write_edge_tokens`]), or `None` if absent or malformed.
/// Same-uid + 0600 is the access gate: the path is not secret, the file is.
/// Repeatable: the parent owns the file's removal ([`ChildProxy`]).
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

/// What this aterm holds for one child it provisioned: the 0600 edge-token file
/// it wrote ([`write_edge_tokens`]), when it could write one.
///
/// Dropping it removes the file. The session that spawned the child holds it,
/// so a closed tab retires its child's edge secrets and `edges/` does not grow
/// for the process lifetime (`aterm_models_session::proxy_registry_model`); a
/// spawn that fails after provisioning drops it on the way out, sharing the
/// same teardown. The file removed is the exact path written, never one
/// re-derived from a socket dir that may resolve differently by then.
///
/// On unix `edges/` is swept by owner lock ([`sweep_dead_edge_files`]), never by
/// graph liveness: a provisioned child has no discovery entry until it launches
/// (maybe never).
pub(crate) struct ChildProxy {
    edge_file: Option<EdgeFile>,
}

impl ChildProxy {
    /// Take ownership of `edge_file` (its owner lock included).
    pub(crate) fn new(edge_file: Option<EdgeFile>) -> Self {
        Self { edge_file }
    }
}

impl Drop for ChildProxy {
    fn drop(&mut self) {
        if let Some(file) = self.edge_file.take() {
            // Removed while still locked, so no sweep can take it in between,
            // then unlocked: off the event loop once `spawn_prep` runs, since
            // the removal and the close that reclaims the file are filesystem
            // work a closing tab held the loop for (about 1 ms).
            crate::spawn_prep::retire_edge_file(file);
        }
    }
}

/// The hosting pid a session's discovery entry records (`pid <n>`), or `None`
/// when there is no entry, it is unreadable, or it has no `pid` line.
///
/// Lets [`crate::identity_claim`] ask "is another live instance already
/// serving this id?" without dialing a same-uid-writable path; a pid alone
/// grants and reveals nothing.
pub(crate) fn graph_entry_host_pid(sock_dir: &Path, sid: &SessionId) -> Option<u32> {
    let body = std::fs::read_to_string(entry_path(sock_dir, sid)?).ok()?;
    aterm_types::control_socket::graph_entry_pid(&body)
}

/// Test-only: `id` as the [`Sid`] it was minted as, for [`read_graph_entry`].
#[cfg(test)]
pub(crate) fn sid_of(id: &SessionId) -> Sid<'_> {
    Sid::parse(id.as_str()).expect("a minted id is a Sid")
}

/// Read a session's discovery entry: `(sock_path, nonce)` or `None` if absent /
/// malformed. Pure parse split out for testing.
pub(crate) fn read_graph_entry(sock_dir: &Path, sid: Sid<'_>) -> Option<(String, LaunchNonce)> {
    let body = std::fs::read_to_string(graph_path(sock_dir, sid)).ok()?;
    parse_graph_entry(&body)
}

/// Parse a graph-entry body's `sock` and `nonce` lines. The `sock` line is the
/// shared on-disk format (`aterm_types::control_socket::graph_entry_sock`, also
/// read by `aterm-ctl`'s self-location); the nonce line is server-only.
fn parse_graph_entry(body: &str) -> Option<(String, LaunchNonce)> {
    let sock = aterm_types::control_socket::graph_entry_sock(body)?;
    let nonce = body
        .lines()
        .find_map(|l| l.strip_prefix("nonce "))
        .and_then(|h| LaunchNonce::from_hex(h.trim()))?;
    Some((sock, nonce))
}

/// Why a forward's connection to the host did not come up ([`connect_hop`]).
#[derive(Debug)]
pub(crate) enum ForwardError {
    /// The entry names no hop: nothing listens at its socket (`ENOENT`,
    /// `ECONNREFUSED`: a crashed or quit instance's leftover), or no token can
    /// be read beside it. Nothing was sent, and the request is answered here as
    /// if no entry named the session (`ERR no such session`).
    NotAHop,
    /// The host could not be reached for another reason (a connect error that
    /// does not say nobody listens): the caller answers `ERR forward`.
    Unreachable(std::io::Error),
}

/// A forward's connection to the instance hosting the session, made by
/// [`connect_hop`]: the stream, the socket it was reached at, the listener that
/// took it and the token read beside that socket once it was up. Nothing has
/// been sent on it yet.
pub(crate) struct Hop {
    stream: CtlStream,
    sock: String,
    token: String,
    listener: Option<aterm_uds::listener::ListenerId>,
}

/// Connect to the host's socket `sock` and take the token beside it.
///
/// The connect is the liveness test: there is no separate probe connection,
/// and a socket nothing listens at is [`ForwardError::NotAHop`]. The token is
/// read once the connection exists: an instance writes its token before it
/// binds, so a token read after the connect belongs to the listener that took
/// the connection (or a newer one); one read before could belong to an
/// instance an update's successor has since replaced, which the successor
/// refuses with `ERR auth`. No token to read is `NotAHop` too, and the
/// connection is dropped with nothing sent.
///
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
    // The listener that took the connection, read once it is up — what the
    // host is judged by when it hangs up ([`ForwardHost::serves`]).
    let listener = aterm_uds::listener::listener_at(sock);
    let token = read_sibling_token(sock).ok_or(ForwardError::NotAHop)?;
    Ok(Hop {
        stream,
        sock: sock.to_string(),
        token,
        listener,
    })
}

/// The one line a forward's handshake is equivalent to, `TOKEN <token>
/// <verb>`: the form the target's connect-time auth (`first_line_scope`) reads,
/// and so the form a conformance test hands it.
#[cfg(test)]
#[must_use]
pub(crate) fn forward_first_line(token: &str, verb: &str) -> String {
    format!("TOKEN {token} {verb}\n")
}

/// How long a forward waits for the host to prove it is serving the
/// connection (its answer to the connect probe) before telling the caller the
/// host accepted nothing. The same bound `aterm ctl` gives its own connect
/// phase, so a wedged listener (connections queued, never accepted) does not
/// hold the caller and this lane for a whole verb deadline.
pub(crate) const FORWARD_ACCEPT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// On `hop`, prove the host serves, send `line` (the caller's request as it
/// came) and then the `client_prebuffered` bytes (anything the server's
/// lane reader already read past the request line), and relay bytes
/// transparently in both directions until either side closes. Carries any
/// verb's framing (status lines, `OK <n>` bodies, subscribe push frames,
/// binary).
///
/// Returns `Ok(())` once the line was delivered, whatever the relay then met,
/// or an `io::Error` if the host was not shown to serve before any byte of the
/// line was sent (so the caller can answer `ERR forward`), including a host
/// that did not answer the connect probe within [`FORWARD_ACCEPT_DEADLINE`]
/// (`ErrorKind::TimedOut`).
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

/// Whether a connect-probe answer refuses the connection rather than answering
/// the probe: the token was refused (`ERR auth`, after which the server
/// closes), or no lane could take it (`ERR control server busy; retry`).
pub(crate) fn is_connection_refusal(line: &str) -> bool {
    line == "ERR auth" || line.starts_with("ERR control server busy")
}

/// The connect probe of a forward: authenticate with `token` and ask `version`
/// (answered by every aterm generation, for any scope, with one line and no
/// main-thread hop), then read that line within `within`. Only a host that
/// answers is sent the caller's verb, so a host whose listener queues
/// connections without accepting them costs seconds, not the verb's deadline,
/// and receives nothing. The reply is consumed here, never relayed.
///
/// The verb then travels as the connection's next request line, which the
/// host serves exactly as if it had been folded into the handshake.
fn prove_host_serving(
    host: &CtlStream,
    token: &str,
    within: std::time::Duration,
) -> std::io::Result<()> {
    let line = probe_answer(host, token, within)?;
    // A refusal ends the connection: the verb is never sent.
    if is_connection_refusal(&line) {
        return Err(std::io::Error::other(format!("the host answered {line:?}")));
    }
    host.set_read_timeout(None)
}

/// The connect probe's exchange: `TOKEN <token> version` written on `host`,
/// and the one line it answers within `within`, without its `\r?\n` — `OK
/// …`, or a refusal. `TimedOut` when nothing answers in time,
/// `UnexpectedEof` when the connection closes first.
///
/// A write that fails still reads the line the host left behind: a listener
/// with no lane free writes `ERR control server busy; retry` and closes
/// without reading, so the hello is what fails (as aterm-ctl's
/// `pending_status_line` reads it). Only with no line there is the write's
/// error the answer.
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

/// The one line [`probe_answer`] reads within `within`.
///
/// It reads in chunks, the read timeout re-armed before each refill with what
/// is left of the one deadline, so a host that dribbles cannot stretch it. The
/// host sends nothing past that line before it is sent the verb, so bytes
/// after it are refused (`InvalidData`): the forward answers `ERR forward` and
/// delivers nothing, rather than relay bytes the host sent unasked as the
/// verb's answer.
fn read_probe_line(host: &CtlStream, within: std::time::Duration) -> std::io::Result<String> {
    use std::io::{Error, ErrorKind};
    /// A probe answer's bound: a `version` line is far shorter.
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
        // (Darwin refuses the bound once the peer detached; its queued bytes
        // and EOF still read at once.)
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

/// The aterm at the far end of a forward, as its relay judges a hang-up that
/// comes from it ([`ForwardHost::gone_line`]): the socket it was reached at,
/// the token presented there, its pid (`None` where the platform cannot read a
/// peer's), and the listening socket the forward's connection went through
/// (`None` where none can be read: the pid alone then decides).
struct ForwardHost {
    sock: String,
    token: String,
    pid: Option<u32>,
    listener: Option<aterm_uds::listener::ListenerId>,
}

/// How long the relay waits for a host that hung up first to answer one look
/// ([`ForwardHost::gone_line`]): a live aterm answers `version` well within it,
/// and a process that is going cannot answer at all.
const HOST_LOOK: std::time::Duration = std::time::Duration::from_secs(2);

impl ForwardHost {
    /// What the client is told when the host hung up first: the host-gone line
    /// ([`aterm_types::control_verbs::host_gone_line`]) when the host no longer
    /// serves at its socket (nothing answers there, another process does, or the
    /// same pid does through another listener), and nothing when it does (it ended
    /// the exchange on purpose) or when its pid could not be read.
    fn gone_line(&self) -> Option<String> {
        let pid = self.pid?;
        (!self.serves(pid)).then(|| aterm_types::control_verbs::host_gone_line(pid, &self.sock))
    }

    /// Whether `pid` still answers at the host's socket through the listener the
    /// forward went through: a connection there is that process's, answers the
    /// connect probe with any line (`OK`, or a busy/auth refusal, which only a live
    /// listener gives), and the socket file still names the listener that accepted
    /// the forward, read after the answer.
    ///
    /// A pid alone does not name the host ([`aterm_uds::listener`]): the cold update
    /// lane shuts down every stream it accepted, and its `exec` keeps the pid and
    /// binds the same path, so judged by pid that host would seem to have ended the
    /// exchange on purpose.
    ///
    /// A busy answer often comes with no pid: the listener writes its line and
    /// closes at once, and Darwin answers `ENOTCONN` to `LOCAL_PEERPID` once the
    /// peer has closed. The recorded listener, unchanged after the answer, then
    /// names the host, as aterm-ctl's `answers_at` judges a successor; without one,
    /// an answer with no pid proves nothing.
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

/// [`relay_hop`] with its relay stage as a parameter: the reply-fidelity
/// contract (`reply_fidelity_model`) lives entirely here and in
/// [`connect_hop`], so the Tier-1 bind (`reply_fidelity_conformance`) drives it
/// with the real relay and with one that fails after delivery (a worker thread
/// the system refuses to start cannot be staged in a shared test process).
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
    // Prove the host is serving before it is handed the line, then send the
    // line as the next request.
    prove_host_serving(&child, &token, accept_within)?;
    // The host's pid is read NOW, while the connection is surely up: Darwin answers
    // `ENOTCONN` to `LOCAL_PEERPID` once the peer has closed, and the host this
    // exists for (an update's Commit `_exit`s it) hangs up as soon as it has read
    // the verb - read after the delivery, a relay that was descheduled between the
    // two found no pid, and the client a bare EOF instead of `ERR gone`.
    let pid = aterm_uds::fdpass::peer_pid(&child).ok();
    (&child).write_all(line.as_bytes())?;
    (&child).write_all(b"\n")?;
    if !client_prebuffered.is_empty() {
        (&child).write_all(client_prebuffered)?;
    }
    (&child).flush()?;
    // Past this point the line HAS been delivered to the host. A relay-stage
    // failure (a worker thread the system refuses to start) tears the
    // connection down but must NOT be reported as "forward failed" — that would
    // be a false negative for an op that already reached the host. Only a probe
    // or write error (the `?`s above, before the line was delivered) surfaces
    // as `Err` so the caller can honestly answer `ERR forward`. Nor is the host
    // going away under the delivered verb: the relay says THAT, in its own
    // words ([`ForwardHost::gone_line`]).
    let host = ForwardHost {
        sock,
        pid,
        token,
        listener,
    };
    let _ = relay(client, &child, host);
    Ok(())
}

/// Pump bytes both ways between two connected streams, preserving graceful
/// half-close order: the child's EOF becomes `Shutdown::Write` on the client only
/// after every preceding byte was copied and flushed, and the client's EOF then
/// becomes `Shutdown::Write` on the child. Guarded artifact replies need this: the
/// client's ACK must reach the child after the complete response. A real copy
/// error hangs up both sides to unblock the paired pump.
///
/// When the child's side ends while the client is still there, the host is looked
/// at once ([`ForwardHost::gone_line`]): one that no longer serves (an update's
/// Commit `_exit`ed it, or it crashed or quit) leaves the client the host-gone line
/// before its EOF, so a `subscribe` does not read the end as deliberate.
///
/// Every teardown hangs up each half on its own ([`aterm_uds::hangup::hang_up`]),
/// never a both-ways `shutdown`: by then one end has usually half-closed toward
/// the relay, Darwin refuses a both-ways `shutdown` of that end and shuts nothing,
/// and the peer then reads no EOF while any copy of the descriptor lives (a fork
/// holds one until it execs).
///
/// Every end of file the relay gives is a shutdown, never a close. Its two pumps
/// borrow `client` and `child` (a scoped worker, no `try_clone`; `&CtlStream`
/// reads and writes on every platform), so it holds no descriptor beyond the
/// client's and the host's connection, and no way out leaves a peer waiting on
/// the last close of a descriptor a fork elsewhere may still hold: a worker the
/// system refuses to start, a pump that unwinds ([`HangUpOnUnwind`]) and the
/// normal end all hang both sides up before the relay returns.
///
/// No exit waits on a wake from another thread: every hang-up of the child is
/// preceded by setting `stop`, and the worker polls the child's end
/// ([`crate::control::HANGUP_POLL`]) before each read, re-reading `stop` whenever a
/// poll times out, so no exit depends on a `shutdown(SHUT_RD)` waking a read
/// already parked, which through a sibling descriptor is not reliable. The
/// worker's hang-up of the client sets `client_stop` the same way for this
/// thread's client read.
fn relay_bidirectional(
    client: &CtlStream,
    child: &CtlStream,
    host: ForwardHost,
) -> std::io::Result<()> {
    use aterm_uds::hangup::hang_up;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

    // Set once this side found the client gone and tore both down: the
    // child's end the worker then reads is that, not the host's.
    let client_gone = &AtomicBool::new(false);
    // Set before every hang-up of the child: the worker stops at its next
    // poll timeout even if the hang-up's wake never reaches it.
    let stop = &AtomicBool::new(false);
    // Set before the worker's hang-up of the client: this thread's client
    // read stops at its next poll timeout, as the client's end of file.
    let client_stop = &AtomicBool::new(false);
    let hang_up_child = move || {
        stop.store(true, SeqCst);
        hang_up(child);
    };
    // A leg that broke, or a pump that unwinds: both sides, for the whole socket.
    let hang_up_both = move || {
        client_stop.store(true, SeqCst);
        hang_up(client);
        hang_up_child();
    };
    // child -> client on a worker; client -> child here. The worker says when
    // it is done (its sender drops even if it unwinds), so the hangup watch
    // below costs no latency on a relay that ends normally.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let relayed: std::io::Result<()> = std::thread::scope(|scope| {
        // An unwind out of this thread waits in the scope for the worker,
        // which ends only once `stop` is set: so both sides are hung up first.
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
            // The client's EOF: a half-close still wants the answer, but a client
            // that closed both halves has nobody to answer, so tear both sides down
            // and let the child's blocking verb and this lane end now.
            Ok(()) if !aterm_uds::hangup::peer_closed(client) => {
                let _ = child.shutdown(std::net::Shutdown::Write);
            }
            // A copy that failed with the client still there failed on the
            // host's side — it went away under a write: end that half only, and
            // the worker, reading the host's end, says so.
            Err(_) if !aterm_uds::hangup::peer_closed(client) => {
                hang_up_child();
            }
            Ok(()) | Err(_) => {
                client_gone.store(true, SeqCst);
                hang_up(client);
                hang_up_child();
            }
        }
        // A client that half-closed and then died is seen by nobody above (this
        // thread no longer reads it; the worker is parked on the child): watch for
        // that hangup while the answer is coming, so a dead client's lane is given
        // back within a poll rather than when the child's verb times out.
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
        // Joined here, so a worker that unwound ends the relay as one that
        // finished would, rather than re-raising its panic in this lane.
        let _ = worker.join();
        Ok(())
    });
    hang_up(client);
    hang_up_child();
    relayed
}

/// Runs its hang-up when an unwind drops it: a relay pump that panics still
/// ends both sides with a shutdown, so neither peer, nor the other pump, nor
/// the scope that joins them waits on it.
struct HangUpOnUnwind<F: Fn()>(F);

impl<F: Fn()> Drop for HangUpOnUnwind<F> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            (self.0)();
        }
    }
}

/// Wait until the relay's end `end` has something for a read (bytes, its end
/// of file or an error, any of which a read returns at once) or the relay has
/// hung it up (`stop`): `true` to read, `false` to stop. `stop` is re-read
/// after each [`crate::control::HANGUP_POLL`] poll that saw nothing, so the
/// pump ends within one poll of a hang-up whose wake was lost. A
/// `POLLHUP`/`POLLERR`/`POLLNVAL` lets the read run (it ends the pump), never
/// a spin; `EINTR` polls again. Off Unix the read itself is the wait.
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
                // A poll that cannot judge the descriptor: the read will.
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

/// How the host-to-client half of a forward ended.
enum HostLeg {
    /// The host's side ended: its end of file, or the connection it reset
    /// (Linux resets a socket closed with unread input) — the host closed it,
    /// on purpose or by going away.
    HostEnded,
    /// Anything else: the client's side failed, or an error that says
    /// nothing about the host.
    Broke,
}

/// Copy the host's bytes to the client in 32 KiB chunks until either side
/// ends, saying which ([`HostLeg`]). `ready` is asked before each read; its
/// `false` (the relay hung the host's end up) is that end ending, as a read
/// of it would have said.
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

/// Copy `reader` → `writer` in 32 KiB chunks until EOF or error. `ready` is
/// asked before each read; its `false` (the relay hung the reader's end up)
/// is that end's EOF, as a read of it would have said.
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

    /// A host whose pid was not read: a hang-up from it is relayed as a bare end of
    /// stream.
    fn unknown_host() -> ForwardHost {
        ForwardHost {
            sock: String::new(),
            token: String::new(),
            pid: None,
            listener: None,
        }
    }

    /// Plant `token` beside the host socket `sock`, where [`connect_hop`] reads
    /// it ([`read_sibling_token`]).
    fn token_beside(sock: &Path, token: &str) {
        let name = aterm_types::control_socket::token_name_for_sock(
            &sock.file_name().expect("a socket file").to_string_lossy(),
        );
        std::fs::write(sock.with_file_name(name), format!("{token}\n")).expect("the host's token");
    }

    /// A connection to the host listening at `sock`, its token planted first.
    fn hop_to(sock: &Path, token: &str) -> Hop {
        token_beside(sock, token);
        connect_hop(&sock.to_string_lossy())
            .unwrap_or_else(|e| panic!("{}: no hop: {e:?}", sock.display()))
    }

    /// The published spelling of a Windows socket path drops the verbatim
    /// prefix and nothing else, so the bind-time entry and every later
    /// session entry spell one socket the same way.
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

    /// An instance on an explicit `--control-sock` mirrors its graph entry into the
    /// default rendezvous dir (the only one the flagless `aterm-ctl` reads), and the
    /// client's own parse (`control_socket::graph_entry_sock`) recovers the absolute
    /// socket path and nonce from it.
    #[test]
    fn explicit_socket_instance_resolvable_from_default_dir_entry() {
        // `default_dir` stands in for `aterm_uds::control_socket_dir()`.
        let default_dir =
            std::env::temp_dir().join(format!("aterm-defaultdir-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&default_dir);
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        // The instance's actual socket lives in a wholly separate, non-default dir.
        let explicit_sock = "/some/explicit/place/myapp-control.sock";

        write_graph_entry(&default_dir, &sid, explicit_sock, &nonce);

        // The client reads the default dir and resolves the explicit socket.
        let (sock, got_nonce) =
            read_graph_entry(&default_dir, sid_of(&sid)).expect("entry in default dir");
        assert_eq!(
            sock, explicit_sock,
            "the default-dir entry names the explicit out-of-dir socket verbatim"
        );
        assert!(got_nonce.ct_eq(&nonce));
        // Exactly the client's self-location parse (`graph_entry_sock` on the body).
        let body =
            std::fs::read_to_string(default_dir.join("graph").join(sid.as_str())).expect("body");
        assert_eq!(
            aterm_types::control_socket::graph_entry_sock(&body).as_deref(),
            Some(explicit_sock)
        );
        // The entry carries the hosting pid so `instances`/`ls` can report one even
        // for an explicit socket whose filename encodes none.
        assert_eq!(
            aterm_types::control_socket::graph_entry_pid(&body),
            Some(std::process::id())
        );
        let _ = std::fs::remove_dir_all(&default_dir);
    }

    /// The mirror targets the default rendezvous dir only for an instance whose
    /// own socket dir is not the default; a default instance already lives
    /// there and writes once. Reads the environment-derived default dir without
    /// mutating the environment, on a discovery state of its own
    /// ([`self_sock_test_guard`]) with no mirror override.
    #[test]
    fn mirror_dir_only_for_out_of_default_instances() {
        let _guard = self_sock_test_guard();
        let Some(def) = aterm_uds::control_socket_dir() else {
            aterm_spec::host::could_not_run("no per-user control socket dir resolves here");
        };
        // A default per-instance instance: sock_dir == the rendezvous dir → no mirror.
        assert!(
            rendezvous_mirror_dir(&def).is_none(),
            "a default instance must not double-write"
        );
        // An explicit-socket instance in some other dir mirrors into the default dir.
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

    /// A session id is joined into a path only as a [`Sid`]: an id that is no
    /// session id (`../x` would name `<dir>/x`, `*` a wildcard entry) is never
    /// written, read or removed as a graph entry. Negative control: a minted
    /// id's entry is written where the reader finds it.
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

    /// A wedged child, whose listener queues connections without accepting them, is
    /// reported within the connect-probe bound as having accepted nothing (an error
    /// before any relay), and the verb never reaches it. The served child in
    /// `relay_hop_pipes_handshake_and_response` is the negative control.
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

    /// A host that refuses the probe (a token it did not mint: `ERR auth`) fails
    /// the forward before the verb is sent.
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

    /// A HOST'S UNASKED BYTES ARE NEVER RELAYED AS AN ANSWER. A host sends
    /// nothing past its probe answer before it is sent the verb, so bytes after
    /// that line (here one more line, in the write that carries the answer)
    /// fail the forward before delivery: the caller answers `ERR forward`, the
    /// verb never reaches the host and the relay never starts. NEGATIVE
    /// CONTROL: the same host answering the probe line alone is sent the verb
    /// and relayed. FAILS WITHOUT THE FIX: the byte-at-a-time probe stopped at
    /// the first line and sent the verb, and the relay handed the unasked line
    /// to the client as the verb's answer.
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
                // The verb, or the end of a forward that sent none.
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
    ///
    /// Unix only: afunix on Windows takes the probe's write and then resets the
    /// connection, discarding the line left unread (`WSAECONNRESET` to the read),
    /// and there no host's pid is read, so [`ForwardHost::serves`] is never asked.
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

    /// A host that goes away under a forward leaves its client a typed line before
    /// the EOF, `ERR gone pid=<host> sock=<its socket>` (never `ERR forward`: the
    /// verb was delivered). `Gone`: its listener closed, then the connection.
    /// `Reopened`: the same pid on a fresh listener renamed over the path, then the
    /// forwarded connection shut (the cold update lane). Negative control,
    /// `ServesOn`: a host that ends the exchange and serves on through the same
    /// listener leaves a bare EOF.
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
                        // Gone: its listener first, then the connection.
                        drop(listener);
                        drop(conn);
                        return;
                    }
                    Host::Reopened => {
                        // A fresh socket renamed over the path, the old one
                        // closed, every accepted stream shut down.
                        let fresh = aterm_uds::CtlListener::bind(&fresh_path).expect("rebind");
                        std::fs::rename(&fresh_path, &path).expect("over the path");
                        drop(listener);
                        let _ = conn.shutdown(std::net::Shutdown::Both);
                        fresh
                    }
                    Host::ServesOn => listener,
                };
                // This exchange ended; the host goes on answering.
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
            // Read in slices ([`read_to_end_of_file`]), never one read parked on
            // the relay's half-close.
            let told = String::from_utf8(read_to_end_of_file(&client_app, |read| {
                format!(
                    "{case:?}: the relay's EOF, after {:?}",
                    String::from_utf8_lossy(read)
                )
            }))
            .expect("the relay passes text");
            // Gone, as a client process that read its answer is: closed.
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
            // Where the connecting end cannot read its host's pid (afunix on Windows
            // answers it no pid, `aterm_uds::fdpass::peer_pid`), the relay cannot
            // judge the host and leaves the bare EOF, as for one that serves on.
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

    /// The relay carries an arbitrary request → response (incl. a multi-line body
    /// and raw non-UTF-8 bytes) transparently, presenting the TOKEN handshake
    /// with the token beside the host's socket. A throwaway CtlListener stands
    /// in for the hosting aterm.
    #[test]
    fn relay_hop_pipes_handshake_and_response() {
        let dir_guard = crate::test_scratch::short_dir("the child's socket");
        let sock = dir_guard.path().join("child.sock");
        let listener = aterm_uds::CtlListener::bind(&sock).expect("bind child");

        // The fake child: answer the TOKEN handshake's connect probe, read the
        // verb, then reply with a framed body that includes a raw 0xff byte,
        // then echo one more line, then close.
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
            // Read whatever the client sends next, echo it, then hang up.
            let mut more = String::new();
            let _ = rdr.read_line(&mut more);
            let _ = conn.write_all(more.as_bytes());
            let _ = conn.shutdown(std::net::Shutdown::Both);
        });

        // The "client" is one end of a socketpair; the relay drives the other end.
        let (client_app, client_relay) = CtlStream::pair().expect("pair");
        let hop = hop_to(&sock, "tok-hex");
        let relay = std::thread::spawn(move || {
            relay_hop(hop, "screen", &client_relay, &[]).expect("relay");
        });

        // The app side: read the child's framed response through the relay.
        let mut app_rdr = BufReader::new(client_app.try_clone().unwrap());
        let mut status = String::new();
        app_rdr.read_line(&mut status).unwrap();
        assert_eq!(status, "OK 1\n");
        let mut body = String::new();
        app_rdr.read_line(&mut body).unwrap();
        assert_eq!(body, "{\"k\":1}\n");
        let mut raw = Vec::new();
        // The 0xff + newline byte survives the relay verbatim.
        let mut byte = [0u8; 2];
        std::io::Read::read_exact(&mut app_rdr, &mut byte).unwrap();
        raw.extend_from_slice(&byte);
        assert_eq!(raw, vec![0xff, b'\n']);

        // Send a follow-up line; the child echoes it back through the relay.
        (&client_app).write_all(b"ping\n").unwrap();
        (&client_app).flush().unwrap();
        let mut echo = String::new();
        app_rdr.read_line(&mut echo).unwrap();
        assert_eq!(echo, "ping\n");

        // Hang the client up for the whole socket ([`hang_up`]) so the relay's
        // client→child reader hits EOF: closing its handles is not enough while a
        // sibling's fork holds a copy, and Darwin refuses a both-ways `shutdown`
        // once the relay has half-closed toward the client.
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
        // The response and the challenge must cross the relay: the read bound
        // is a hang detector, never a latency budget a loaded host outruns.
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
        // The relay has half-closed toward `original` (its child hung up), so
        // only a hang-up of each half ends it ([`hang_up`]).
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
    /// variables. `done` is teardown observed by the peer whose bytes the
    /// faulted arm was relaying (the other peer reads EOF from the injected
    /// fault alone). Each read half is observed on a clone of that local socket
    /// taken beforehand (a non-blocking read answers EOF when shut,
    /// `WouldBlock` when a pump could still park on it). The second value is
    /// whether `relay_bidirectional` returned: its worker joined.
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

        // The peer whose bytes the faulted arm relays speaks, and is the one
        // witness of teardown: the other peer reads EOF from the fault itself.
        let (mut speaker, said) = match fault {
            RelayFault::ChildBound => (&original, b"screen\n".as_slice()),
            RelayFault::ClientBound => (&child_peer, b"OK reply\n".as_slice()),
        };
        speaker.write_all(said).expect("speak");
        // Both are facts the real relay must reach, so each wait is a hang
        // detector, never a latency budget: a loaded host only slows them. The
        // end-of-file is waited for in slices, never on one parked read's wake
        // ([`read_to_end_of_file`]).
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
        // Release anything still parked before judging, so a failure cannot
        // hang: hung up for the whole socket ([`hang_up`]), since a sibling's
        // fork may hold a copy of either peer's end.
        hang_up(child_peer);
        hang_up(original);
        let _ = join_after_hang_up(relay, "the relay to end once both peers hung up");
        (observed, returned)
    }

    /// Tier-1 conformance of the real relay to
    /// `aterm_models_session::relay_teardown_model` (`RelayTeardown`): on teardown both
    /// read halves of both local sockets are shut, so a pump parked on a clone of
    /// either gets EOF and the worker joins.
    ///
    /// Each of `relay_bidirectional`'s two error arms is driven by its own fault
    /// ([`RelayFault`]), and its projection ([`relay_teardown_under`]) must be the
    /// model's `Teardown` step with the relay returned. Negative control: `Buggy = 1`
    /// (`shutdown(Write)` only, which leaves both read halves open) must reject every
    /// real step.
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

    /// The host pump ends on the stop flag alone: the lost wake, staged by sending
    /// none. Once the pump waits on a silent host nobody shuts, only `stop` is set,
    /// and the pump must still end, as the host's side ending. Negative control: drop
    /// the `stop` re-read after a poll timeout and the pump parks forever.
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
        // SAFETY: `end` is borrowed for the call, so its fd is open; `buf` is
        // writable for its whole length, and `MSG_PEEK | MSG_DONTWAIT` neither
        // consumes nor blocks.
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

    /// A child that half-closed reads end-of-file when the relay ends, though a copy
    /// of the relay's end of it survives (a `try_clone` here, as a fork holds one
    /// until it execs).
    ///
    /// The child answers and half-closes; the relay passes both on; the client reads
    /// them and half-closes in turn. Once the relay has returned, one non-blocking
    /// read of the child's end is the verdict. Negative control: restore both-ways
    /// `shutdown` calls, which Darwin refuses (`ENOTCONN`) on a socket whose read
    /// half the child's half-close shut, and the child reads `WouldBlock`.
    ///
    /// The client's read is a hang detector that says how far the relay got when
    /// it fires (once, in a full gate on m3 under load, 2026-10-16): the bytes the
    /// client had, whether the relay had read the child's answer, and whether it
    /// had returned.
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
        // Read in slices ([`read_to_end_of_file`]): a read parked on the wake can
        // sleep through the relay's half-close on Darwin, the 2026-10-16 failure.
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

    /// Edge-token secrets round-trip through a 0600 file, and the read is repeatable
    /// (a child relaunched in the same shell re-reads it). The parent removes it when
    /// the `ChildProxy` holding its path drops.
    #[test]
    fn edge_tokens_file_is_0600_and_read_is_repeatable() {
        let dir = std::env::temp_dir().join(format!("aterm-edges-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sid = SessionId::generate();
        let (r, w, s) = ("aa".repeat(32), "bb".repeat(32), "cc".repeat(32));
        let file = write_edge_tokens(&dir, &sid, &r, &w, &s).expect("write");
        let path = file.path().to_string_lossy().into_owned();
        // 0600 — owner read/write only (no group/other bits). POSIX-mode
        // assert; on Windows the file inherits the private dir's ACL instead.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "edge-token file must be 0600, got {mode:o}");
        }
        // Two reads of the same file both succeed (a same-shell relaunch
        // re-reads it).
        let first = read_edge_tokens(&path);
        let second = read_edge_tokens(&path);
        assert_eq!(first, Some((r.clone(), w.clone(), s.clone())), "first read");
        assert_eq!(
            second,
            Some((r, w, s)),
            "second read still succeeds (persists)"
        );
        // A child that never had a file retires without touching this one.
        drop(ChildProxy::new(None));
        assert!(read_edge_tokens(&path).is_some(), "only its own file");
        // The parent owns removal: retiring the child removes exactly this file.
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
    /// removal, as a crash leaves it). A provisioning sweeps nothing: that is
    /// the sweeper thread's work, off the event loop. Negative controls: a live
    /// parent's file (its `EdgeFile` held) and an empty file (a writer between
    /// create and lock) stay.
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
        // The lock released, the file left: a crashed parent. A child forked while
        // the lock was held, parked short of its exec through the sweep, must not
        // make the dead parent read as live: the drop releases by LOCK_UN
        // ([`EdgeFile`]), not by the close alone.
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

    /// Sibling discovery lifecycle: before the control socket binds,
    /// `publish_session` is a safe no-op; after `set_self_sock`, it writes the
    /// session's graph entry pointing at our instance socket, and
    /// `unpublish_session` retires it on close. `read_sibling_token` pairs the
    /// per-instance socket with its 0600 token file and fails closed on
    /// absent/empty tokens.
    #[test]
    fn publish_session_lifecycle_follows_bind_and_close() {
        let _guard = self_sock_test_guard();
        let dir = std::env::temp_dir().join(format!("aterm-pub-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // Redirect the rendezvous mirror into a scratch dir: `dir` is not the
        // default dir, so `publish_session` would otherwise mirror into the user's
        // real control dir. Cleared before the self-sock guard drops.
        let mirror = std::env::temp_dir().join(format!("aterm-pub-mirror-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&mirror);
        set_mirror_dir_override(Some(mirror.clone()));
        // The real default dir must be untouched by this test — capture it so we
        // can assert our (randomly-generated) sid never lands there.
        let real_default = aterm_uds::control_socket_dir();
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();

        // Pre-bind: publishing is a no-op (no panic, no entry).
        clear_self_sock();
        publish_session(&sid, &nonce);
        assert!(
            read_graph_entry(&dir, sid_of(&sid)).is_none(),
            "no entry pre-bind"
        );

        // Post-bind: the entry names our socket, canonical (macOS temp: /var →
        // /private/var) and in its published spelling, with the nonce.
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

        // The mirror landed in the scratch dir, not the real default dir.
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

        // Close: the entry is retired from both our dir and the scratch mirror.
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

        // read_sibling_token: pairs aterm-<pid>.sock with aterm-<pid>.token.
        std::fs::write(dir.join("aterm-88001.token"), "  feed1234\n").unwrap();
        assert_eq!(read_sibling_token(&sock).as_deref(), Some("feed1234"));
        // Empty and absent tokens fail closed.
        std::fs::write(dir.join("aterm-88001.token"), "\n").unwrap();
        assert_eq!(read_sibling_token(&sock), None);
        let other = dir.join("aterm-88002.sock").to_string_lossy().into_owned();
        assert_eq!(read_sibling_token(&other), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A session's discovery write is queued, not made in the registration's
    /// turn: held behind a write the thread cannot finish, a publish has not
    /// landed when `publish_session` returns, and lands once the thread moves.
    /// The queue keeps order: a session closed while its publish waits is
    /// retired after it, so no entry is left behind. The queue is this test's
    /// own (`self_sock_test_guard`): another test, on the process's state,
    /// publishes nothing into it and its Commit never waits behind it.
    #[test]
    fn a_session_publication_is_queued_off_the_caller_and_kept_in_order() {
        /// This test's discovery state, cleared on every exit path: the queue
        /// drained while the scratch mirror still stands.
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

        // The thread holds a write until this test takes its answer.
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
        // ANOTHER TEST'S APP, on a thread of its own (the process's state),
        // sees none of this while the queue is held: a session it registers
        // publishes nowhere, and its Commit's flush is free — never "discovery
        // publications did not settle" over a write it never queued.
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

    /// An instance's quit retires only the discovery entries that name its
    /// socket: a rolled-back update candidate carries its live parent's session
    /// ids, whose entries must survive. Negative control: its own entry goes.
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
