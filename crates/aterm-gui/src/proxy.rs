// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-process `@<sid>` proxy forward: one control address space across the
//! recursion tree, so an outer aterm reaches a session in an inner aterm it
//! spawned or in a sibling instance.
//!
//! Child hop: when `handle`/`serve` cannot resolve `@<sid>` locally, it consults
//! the [`ProxyTable`] of per-op edge tokens this aterm minted for each child it
//! spawned. The child's socket comes from the graph entry it wrote at bind
//! ([`read_graph_entry`]); the tokens bind to the launch nonce recorded there, so
//! a relaunched child fails closed at discovery rather than dialing a stranger.
//! The forward connects, proves the child is serving (`TOKEN <edge-hex> version`;
//! the child authorizes the token against the edges it installed from its injected
//! env, so it grants exactly the op the verb needs), sends the verb with the
//! child's selector inlined to `@.`, and relays bytes verbatim both ways (no
//! framing is parsed, so push streams and binary payloads pass). Only direct
//! children are forwarded and a child is never in its own table, so no cycle forms;
//! an edge-scoped connection never forwards (it resolves locally), so an edge cannot
//! be escalated beyond its grant.
//!
//! Sibling hop: every instance publishes a graph entry for every session it hosts
//! ([`publish_session`] / [`unpublish_session`], fed by [`set_self_sock`]) into
//! the shared runtime dir, which a nested instance inherits, so a grandchild
//! resolves this way too. An `@<sid>` that resolves neither locally nor as a child
//! is forwarded (Owner scope only, selector kept) to the hosting instance's socket,
//! authenticated with its per-launch token (`aterm-<pid>.token`, same-uid 0600),
//! which a same-uid `aterm-ctl --pid` could read itself. A stale entry pointing
//! back at the forwarder's own socket is refused, so a removed session degrades to
//! `ERR no such session`, never a loop.

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use aterm_session::{EdgeToken, LaunchNonce, Op, SessionId};
use aterm_uds::CtlStream;

#[cfg(unix)]
use crate::native_document_host::HeldAdvisoryLock;

/// The capability this aterm holds over one child it spawned: the child's launch
/// nonce (to validate the graph entry) plus the three per-op edge tokens minted
/// at spawn. The child's socket path is discovered live from its graph entry,
/// since an inner aterm binds it only once it starts.
#[derive(Clone)]
pub(crate) struct ProxyEntry {
    pub nonce: LaunchNonce,
    pub read: EdgeToken,
    pub write: EdgeToken,
    pub signal: EdgeToken,
}

impl ProxyEntry {
    /// The edge token to present for `op` (read/write/signal). `DeriveLoop`,
    /// `ConfigWrite` and `ClipboardWrite` have no provisioned edge and are
    /// refused (`None`): durable-config and clipboard authority are never
    /// carried by an inherited edge (only the instance Owner holds them).
    #[must_use]
    pub(crate) fn token_for(&self, op: Op) -> Option<&EdgeToken> {
        match op {
            Op::ReadScreen => Some(&self.read),
            Op::WriteInput => Some(&self.write),
            Op::Signal => Some(&self.signal),
            _ => None,
        }
    }
}

/// This aterm's map of spawned children → the capability it holds over each.
/// Shared between the spawn path (which inserts) and the control server (which
/// reads to forward). Empty until this aterm spawns a child.
pub(crate) type ProxyTable = Arc<RwLock<HashMap<SessionId, ProxyEntry>>>;

/// A fresh, empty proxy table.
#[must_use]
pub(crate) fn new_proxy_table() -> ProxyTable {
    Arc::new(RwLock::new(HashMap::new()))
}

/// The process-wide proxy table, one per aterm process (the spawn path inserts
/// a child's capability; the control server reads it to forward). A singleton
/// avoids threading the handle through every `spawn_session`/`serve` caller.
static PROXIES: std::sync::OnceLock<ProxyTable> = std::sync::OnceLock::new();

/// The process-wide [`ProxyTable`] (lazily initialized, cloned Arc).
#[must_use]
pub(crate) fn proxies() -> ProxyTable {
    PROXIES.get_or_init(new_proxy_table).clone()
}

/// Record the capability this aterm holds over a child it just spawned.
pub(crate) fn register_child(child: SessionId, entry: ProxyEntry) {
    proxies()
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .insert(child, entry);
}

/// Look up the capability for a child by session id (cloned out).
#[must_use]
pub(crate) fn lookup_child(sid: &SessionId) -> Option<ProxyEntry> {
    proxies()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(sid)
        .cloned()
}

/// Drop the capability for a child (its session closed) so the process-wide table
/// does not grow for the process lifetime as tabs open and close.
pub(crate) fn deregister_child(child: &SessionId) {
    proxies()
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .remove(child);
}

/// The graph-entry filename for a child session id, under `<sock_dir>/graph/`.
fn graph_path(sock_dir: &Path, sid: &SessionId) -> std::path::PathBuf {
    sock_dir.join("graph").join(sid.as_str())
}

/// This instance's own bound control socket `(sock_dir, sock_path)`, recorded by
/// the control server at bind time. `None` until (or unless) a socket is bound.
/// An `RwLock<Option<..>>` (not a `OnceLock`) so tests can set and clear it.
static SELF_SOCK: RwLock<Option<(std::path::PathBuf, String)>> = RwLock::new(None);

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
    *SELF_SOCK.write().unwrap_or_else(|p| p.into_inner()) = Some((sock_dir.to_path_buf(), canon));
    // Sessions publish from here on: their writes' thread starts now, on the
    // binder's thread, not in a tab's landing turn.
    let _ = publisher();
}

/// This instance's own bound socket path, or `None` when no socket is bound.
#[must_use]
pub(crate) fn self_sock_path() -> Option<String> {
    SELF_SOCK
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|(_, p)| p.clone())
}

/// Test-only: clear the recorded self socket so tests that set it cannot leak
/// state into other tests in the same process.
#[cfg(test)]
pub(crate) fn clear_self_sock() {
    *SELF_SOCK.write().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Test-only: serializes every test that touches the process-global
/// [`SELF_SOCK`]. Acquire it first, hold it for the test's whole self-sock
/// window, and `clear_self_sock` before dropping it.
#[cfg(test)]
pub(crate) fn self_sock_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
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
    let guard = SELF_SOCK.read().unwrap_or_else(|p| p.into_inner());
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
        }
    }
}

/// The `aterm-publish` thread's queue, and how many writes on it have not
/// landed.
struct Publisher {
    tx: std::sync::mpsc::Sender<Publication>,
    pending: std::sync::atomic::AtomicUsize,
}

static PUBLISHER: std::sync::OnceLock<Option<Publisher>> = std::sync::OnceLock::new();

/// The publication thread, started with the bind ([`set_self_sock`], on the
/// control thread), so no tab's landing pays for the thread. `None` when it
/// could not start: then each write runs where it is asked, as before.
fn publisher() -> Option<&'static Publisher> {
    PUBLISHER
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Publication>();
            let spawned = crate::qos::spawn_lane_thread(
                "aterm-publish",
                // A new tab's shell finds its instance through its entry
                // (`aterm ctl`'s self-location), so it lands promptly.
                crate::qos::Role::Responsive,
                move || {
                    for publication in rx {
                        publication.write();
                        if let Some(Some(p)) = PUBLISHER.get() {
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
    let Some(Some(p)) = PUBLISHER.get() else {
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
        let mut guard = SELF_SOCK.write().unwrap_or_else(|p| p.into_inner());
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
/// close) is harmless: the sibling forward re-checks socket liveness and the
/// hosting instance re-checks its own store, so it can only produce `ERR no
/// such session`, never a wrong target. Queued behind the session's publish
/// ([`publish_session`]).
pub(crate) fn unpublish_session(sid: &SessionId) {
    let guard = SELF_SOCK.read().unwrap_or_else(|p| p.into_inner());
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

/// Read the per-launch auth token of the sibling instance whose socket is
/// `sock_path` (`aterm-<pid>.sock` → `aterm-<pid>.token`; an explicit socket →
/// `<sock>.token`; 0600, same-uid), through the same function a same-uid
/// `aterm-ctl --sock <path>` uses ([`aterm_ctl::read_token_beside`]), so
/// presenting it grants nothing the caller could not take directly, and a planted
/// foreign token grants nothing (the sibling checks it against the token it holds
/// in memory). `None` (fail closed) for an unreadable token. The forward reads it
/// once its connection exists ([`ForwardToken::BesideSocket`]).
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
/// a write failure only means the parent cannot reach us by proxy.
///
/// Writes one dir only; [`publish_graph_entry`] also mirrors into the default
/// rendezvous dir when the instance runs on an explicit `--control-sock`.
pub(crate) fn write_graph_entry(
    sock_dir: &Path,
    sid: &SessionId,
    sock_path: &str,
    nonce: &LaunchNonce,
) {
    let dir = sock_dir.join("graph");
    // 0700 + owner-verified, like the sibling `images/` subdir (control_auth).
    if crate::control_auth::ensure_private_dir(&dir).is_err() {
        return;
    }
    let path = graph_path(sock_dir, sid);
    let body = format!(
        "sock {sock_path}\nnonce {}\npid {}\n",
        nonce.to_hex(),
        std::process::id()
    );
    if let Ok(mut f) = open_private(&path) {
        let _ = f.write_all(body.as_bytes());
    }
}

/// Test-only override for the mirror target dir. Production leaves it `None`
/// and the mirror lands in `aterm_uds::control_socket_dir()`; a test points it
/// at a scratch dir so it never touches the user's real control dir.
/// Serialized with the self-sock state via [`self_sock_test_guard`], so a
/// concurrent [`rendezvous_mirror_dir`] reader never sees a transient override.
#[cfg(test)]
static MIRROR_DIR_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Test-only: redirect the rendezvous mirror target to `dir` (or clear it with
/// `None`). The caller must hold [`self_sock_test_guard`] and clear it before
/// dropping the guard.
#[cfg(test)]
pub(crate) fn set_mirror_dir_override(dir: Option<PathBuf>) {
    *MIRROR_DIR_OVERRIDE
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
        let over_guard = MIRROR_DIR_OVERRIDE.read();
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
/// Best-effort hygiene: the nonce guard fails a stale dial closed and the
/// client re-probes liveness.
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
/// session's socket path is not left for a parent to dial. Hygiene only: the
/// nonce guard fails a stale dial closed.
pub(crate) fn remove_graph_entry(sock_dir: &Path, sid: &SessionId) {
    let _ = std::fs::remove_file(graph_path(sock_dir, sid));
}

/// Retire `sid`'s discovery entry only while it names this instance's socket
/// (`our_sock`, in any spelling the entry could carry). An entry another live
/// process wrote (the parent of a rolled-back update candidate, which still
/// serves the carried ids) is left for its owner, as is an unreadable entry;
/// the stale sweep retires dead ones.
pub(crate) fn remove_own_graph_entry(sock_dir: &Path, sid: &SessionId, our_sock: &str) {
    let path = graph_path(sock_dir, sid);
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

/// What this aterm holds for one child it provisioned: the proxy-table key its
/// [`ProxyEntry`] was registered under and the 0600 edge-token file it wrote
/// ([`write_edge_tokens`]), when it could write one.
///
/// Dropping it retires both, so a closed tab deregisters its child and
/// [`proxies`] does not grow for the process lifetime
/// (`aterm_models_session::proxy_registry_model`); a spawn that fails after
/// provisioning drops it on the way out. The file removed is the exact path
/// written, never one re-derived from a socket dir that may resolve differently.
///
/// On unix `edges/` is swept by owner lock ([`sweep_dead_edge_files`]), never by
/// graph liveness: a provisioned child has no discovery entry until it launches
/// (maybe never).
pub(crate) struct ChildProxy {
    sid: SessionId,
    edge_file: Option<EdgeFile>,
}

impl ChildProxy {
    /// Register `entry` as this aterm's capability over child `sid` and take
    /// ownership of that registration and of `edge_file` (its owner lock
    /// included).
    pub(crate) fn register(sid: SessionId, entry: ProxyEntry, edge_file: Option<EdgeFile>) -> Self {
        register_child(sid.clone(), entry);
        Self { sid, edge_file }
    }

    /// The child's session id (the proxy-table key).
    #[cfg(test)]
    pub(crate) fn sid(&self) -> &SessionId {
        &self.sid
    }
}

impl Drop for ChildProxy {
    fn drop(&mut self) {
        deregister_child(&self.sid);
        if let Some(file) = &self.edge_file {
            // Removed while still locked: no sweep can take it in between.
            let _ = std::fs::remove_file(file.path());
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
    let body = std::fs::read_to_string(graph_path(sock_dir, sid)).ok()?;
    aterm_types::control_socket::graph_entry_pid(&body)
}

/// Read a child's discovery entry: `(sock_path, nonce)` or `None` if absent /
/// malformed. Pure parse split out for testing.
pub(crate) fn read_graph_entry(sock_dir: &Path, sid: &SessionId) -> Option<(String, LaunchNonce)> {
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

/// The token a forward presents in its `TOKEN` handshake, and when it is taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ForwardToken {
    /// A spawned child's per-op edge token, hex: minted by this process at
    /// spawn and held in memory, so the plan carries it.
    Edge(String),
    /// A sibling instance's own per-launch token: the file beside the socket
    /// the forward dials ([`read_sibling_token`]), read once the connection
    /// exists, never when the forward is planned. An instance writes its token
    /// before it binds, so a token read after the connect belongs to the
    /// listener that took the connection (or a newer one); one read at plan
    /// time may belong to an instance an update's successor has since
    /// replaced, which the successor refuses with `ERR auth`.
    BesideSocket,
}

/// What a planned forward presents on its connection: `TOKEN <token> version`,
/// the connect probe ([`prove_child_serving`]), then `verb` as the next
/// request line, which the target serves exactly as it would the single line
/// `TOKEN <token> <verb>`. For a child, `verb` has the child's own selector
/// inlined to `@.` (direct children only, so no cycle can form). For a
/// sibling, it is the caller's line as it came.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForwardLine {
    pub(crate) token: ForwardToken,
    pub(crate) verb: String,
}

impl ForwardLine {
    /// A child hop's: the edge token this process minted for the op.
    #[must_use]
    pub(crate) fn edge(edge_hex: String, verb: String) -> Self {
        Self {
            token: ForwardToken::Edge(edge_hex),
            verb,
        }
    }

    /// A sibling hop's: the sibling's own token, read beside its socket once
    /// connected.
    #[must_use]
    pub(crate) fn sibling(verb: String) -> Self {
        Self {
            token: ForwardToken::BesideSocket,
            verb,
        }
    }

    /// The token to present on a connection to `sock` that already exists: a
    /// child's edge token as planned, a sibling's read beside `sock` now.
    ///
    /// # Errors
    ///
    /// A sibling token that cannot be read (fail closed: nothing is sent).
    pub(crate) fn token_at(&self, sock: &str) -> std::io::Result<String> {
        match &self.token {
            ForwardToken::Edge(hex) => Ok(hex.clone()),
            ForwardToken::BesideSocket => read_sibling_token(sock).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "no readable token beside the sibling's socket",
                )
            }),
        }
    }
}

/// The one line a forward's handshake is equivalent to, `TOKEN <token>
/// <verb>`: the form the target's connect-time auth (`first_line_scope`) reads,
/// and so the form a conformance test hands it.
#[cfg(test)]
#[must_use]
pub(crate) fn forward_first_line(token: &str, verb: &str) -> String {
    format!("TOKEN {token} {verb}\n")
}

/// How long a forward waits for the child to prove it is serving the
/// connection (its answer to the connect probe) before telling the caller the
/// child accepted nothing. The same bound `aterm ctl` gives its own connect
/// phase, so a wedged listener (connections queued, never accepted) does not
/// hold the caller and this lane for a whole verb deadline.
pub(crate) const FORWARD_ACCEPT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Connect to `child_sock`, present `first_line`, and relay bytes transparently
/// in both directions until either side closes. Carries any verb's framing
/// (status lines, `OK <n>` bodies, subscribe push frames, binary). The
/// `client_prebuffered` bytes (anything the server's `BufReader` already read
/// past the request line) are forwarded to the child first.
///
/// Returns `Ok(())` on a clean close, or an `io::Error` if the dial or
/// handshake failed before any relay (so the caller can answer `ERR`),
/// including a sibling whose token cannot be read once connected and a child
/// that did not answer the connect probe within [`FORWARD_ACCEPT_DEADLINE`]
/// (`ErrorKind::TimedOut`).
pub(crate) fn connect_and_relay(
    child_sock: &str,
    first_line: &ForwardLine,
    client: &CtlStream,
    client_prebuffered: &[u8],
) -> std::io::Result<()> {
    deliver_then_relay(
        child_sock,
        first_line,
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
/// main-thread hop), then read that line within `within`. Only a child that
/// answers is sent the caller's verb, so a child whose listener queues
/// connections without accepting them costs seconds, not the verb's deadline,
/// and receives nothing. The reply is consumed here, never relayed.
///
/// The verb then travels as the connection's next request line, which the
/// child serves exactly as if it had been folded into the handshake.
fn prove_child_serving(
    child: &CtlStream,
    token: &str,
    within: std::time::Duration,
) -> std::io::Result<()> {
    let line = probe_answer(child, token, within)?;
    // A refusal ends the connection: the verb is never sent.
    if is_connection_refusal(&line) {
        return Err(std::io::Error::other(format!(
            "the child answered {line:?}"
        )));
    }
    child.set_read_timeout(None)
}

/// The connect probe's exchange: `TOKEN <token> version` written on `child`,
/// and the one line it answers within `within`, without its `\r?\n` — `OK
/// …`, or a refusal. `TimedOut` when nothing answers in time,
/// `UnexpectedEof` when the connection closes first.
///
/// A write that fails still reads the line the child left behind: a listener
/// with no lane free writes `ERR control server busy; retry` and closes
/// without reading, so the hello is what fails (as aterm-ctl's
/// `pending_status_line` reads it). Only with no line there is the write's
/// error the answer.
fn probe_answer(
    child: &CtlStream,
    token: &str,
    within: std::time::Duration,
) -> std::io::Result<String> {
    let mut hello = String::with_capacity(token.len() + 16);
    hello.push_str("TOKEN ");
    hello.push_str(token);
    hello.push_str(" version\n");
    match (&*child)
        .write_all(hello.as_bytes())
        .and_then(|()| (&*child).flush())
    {
        Ok(()) => read_probe_line(child, within),
        Err(e) => read_probe_line(child, within).map_err(|_| e),
    }
}

/// The one line [`probe_answer`] reads within `within`.
fn read_probe_line(child: &CtlStream, within: std::time::Duration) -> std::io::Result<String> {
    use std::io::{Error, ErrorKind};
    // Byte at a time: nothing past the one line may be consumed here (the
    // child sends nothing more before the verb, but a relay must never lose a
    // byte it did not mean to eat).
    let deadline = std::time::Instant::now() + within;
    let mut line = Vec::with_capacity(160);
    let mut byte = [0u8; 1];
    let not_accepted = || {
        Error::new(
            ErrorKind::TimedOut,
            format!(
                "the child accepted no connection within {}s",
                within.as_secs().max(1)
            ),
        )
    };
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(not_accepted());
        }
        // One deadline across every read, so a child that dribbles cannot
        // stretch it. (Darwin refuses the bound once the peer detached; its
        // queued bytes and EOF still read at once.)
        let _ = child.set_read_timeout(Some(deadline - now));
        match (&*child).read(&mut byte) {
            Ok(0) => {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "the child closed the connection before answering",
                ));
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if line.len() >= 4096 => {
                return Err(Error::new(ErrorKind::InvalidData, "runaway probe reply"));
            }
            Ok(_) => line.push(byte[0]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(not_accepted());
            }
            Err(e) => return Err(e),
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

/// [`connect_and_relay`] with its relay stage as a parameter: the reply-fidelity
/// contract (`reply_fidelity_model`) lives entirely here, so the Tier-1 bind
/// (`reply_fidelity_conformance`) drives it with the real relay and with one
/// that fails after delivery (a `try_clone` under fd exhaustion cannot be
/// staged in a shared test process).
fn deliver_then_relay(
    child_sock: &str,
    first_line: &ForwardLine,
    client: &CtlStream,
    client_prebuffered: &[u8],
    accept_within: std::time::Duration,
    relay: impl FnOnce(&CtlStream, &CtlStream, ForwardHost) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let child = CtlStream::connect(child_sock)?;
    // The listener that took the connection, read once it is up — what the
    // host is judged by when it hangs up ([`ForwardHost::serves`]).
    let listener = aterm_uds::listener::listener_at(child_sock);
    // The token is taken once this connection exists: a sibling's is read
    // beside the socket now, so it is the listener's that took the connection.
    let token = first_line.token_at(child_sock)?;
    // Prove the child is serving before it is handed the verb, then send the
    // rewritten verb as the next request line.
    prove_child_serving(&child, &token, accept_within)?;
    // The host's pid is read now, while the connection is surely up: Darwin answers
    // `ENOTCONN` to `LOCAL_PEERPID` once the peer has closed, and an updating host
    // (its Commit `_exit`s) hangs up as soon as it has read the verb.
    let pid = aterm_uds::fdpass::peer_pid(&child).ok();
    (&child).write_all(first_line.verb.as_bytes())?;
    (&child).write_all(b"\n")?;
    if !client_prebuffered.is_empty() {
        (&child).write_all(client_prebuffered)?;
    }
    (&child).flush()?;
    // Past this point the verb has been delivered. A relay-stage failure (e.g. a
    // `try_clone` under fd exhaustion) must not read as a failed forward: only the
    // connect/handshake errors above surface as `Err` (the caller's `ERR forward`).
    // A host going away under the delivered verb is the relay's to say
    // ([`ForwardHost::gone_line`]).
    let host = ForwardHost {
        sock: child_sock.to_string(),
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
/// No exit waits on a wake from another thread: every hang-up of the child is
/// preceded by setting `stop`, and the worker polls the child's end
/// ([`crate::control::HANGUP_POLL`]) before each read, re-reading `stop` whenever a
/// poll times out, because a read parked on one descriptor is not reliably woken by
/// a `shutdown(SHUT_RD)` through a sibling descriptor. The worker's hang-up of the
/// client sets `client_stop` the same way for this thread's client read.
fn relay_bidirectional(
    client: &CtlStream,
    child: &CtlStream,
    host: ForwardHost,
) -> std::io::Result<()> {
    use aterm_uds::hangup::hang_up;

    let mut c2s_r = client.try_clone()?;
    let mut c2s_w = child.try_clone()?;
    let s2c_r = child.try_clone()?;
    let mut s2c_w = client.try_clone()?;
    let w_client = client.try_clone()?;
    let w_child = child.try_clone()?;
    // Set once this side found the client gone and tore both down: the
    // child's end the worker then reads is that, not the host's.
    let client_gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let torn = Arc::clone(&client_gone);
    // Set before every hang-up of the child: the worker stops at its next
    // poll timeout even if the hang-up's wake never reaches it.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    // Set before the worker's hang-up of the client: this thread's client
    // read stops at its next poll timeout, as the client's end of file.
    let client_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client_stopped = Arc::clone(&client_stop);
    let hang_up_child = |child: &CtlStream| {
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        hang_up(child);
    };
    // child -> client on a worker; client -> child here. The worker says when
    // it is done (its sender drops even if it unwinds), so the hangup watch
    // below costs no latency on a relay that ends normally.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        let ready = || readable_unless_stopped(&s2c_r, &stopped);
        match copy_host_to_client(&mut &s2c_r, &mut s2c_w, ready) {
            HostLeg::HostEnded => {
                if !torn.load(std::sync::atomic::Ordering::SeqCst)
                    && !aterm_uds::hangup::peer_closed(&w_client)
                    && let Some(line) = host.gone_line()
                {
                    let _ = (&w_client).write_all(line.as_bytes());
                    let _ = (&w_client).flush();
                }
                let _ = w_client.shutdown(std::net::Shutdown::Write);
            }
            HostLeg::Broke => {
                client_stopped.store(true, std::sync::atomic::Ordering::SeqCst);
                hang_up(&w_client);
                stopped.store(true, std::sync::atomic::Ordering::SeqCst);
                hang_up(&w_child);
            }
        }
        let _ = done_tx.send(());
    });
    let client_ready = || readable_unless_stopped(client, &client_stop);
    match copy_until_eof(&mut c2s_r, &mut c2s_w, client_ready) {
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
            hang_up_child(child);
        }
        Ok(()) | Err(_) => {
            client_gone.store(true, std::sync::atomic::Ordering::SeqCst);
            hang_up(client);
            hang_up_child(child);
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
            client_gone.store(true, std::sync::atomic::Ordering::SeqCst);
            hang_up(client);
            hang_up_child(child);
            break;
        }
    }
    let _ = worker.join();
    hang_up(client);
    hang_up_child(child);
    Ok(())
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

/// Drain whatever a server-side `BufReader` already buffered past the request
/// line, so [`connect_and_relay`] can forward it to the child before the raw
/// relay. Uses [`BufReader::buffer`], never `fill_buf()`, which blocks on an
/// empty buffer (the common case: the client sent one line and awaits the reply)
/// and would deadlock before the relay starts.
#[must_use]
pub(crate) fn drain_buffered<R: Read>(reader: &mut std::io::BufReader<R>) -> Vec<u8> {
    let buffered = reader.buffer().to_vec();
    let n = buffered.len();
    reader.consume(n);
    buffered
}

#[cfg(all(test, unix))]
#[path = "reply_fidelity_conformance.rs"]
mod reply_fidelity_conformance;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_socket::{hang_up, join_after_hang_up};
    use std::io::BufReader;

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

    /// A capability with fresh tokens, for tests that only need an entry.
    fn test_entry() -> ProxyEntry {
        ProxyEntry {
            nonce: LaunchNonce::generate(),
            read: EdgeToken::generate(),
            write: EdgeToken::generate(),
            signal: EdgeToken::generate(),
        }
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
        let (sock, got_nonce) = read_graph_entry(&default_dir, &sid).expect("entry in default dir");
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
    /// mutating the environment, holding [`self_sock_test_guard`] so a
    /// concurrent publish test's `MIRROR_DIR_OVERRIDE` cannot shadow it.
    #[test]
    fn mirror_dir_only_for_out_of_default_instances() {
        let _guard = self_sock_test_guard();
        let Some(def) = aterm_uds::control_socket_dir() else {
            return; // no per-user base resolvable in this environment
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

    #[test]
    fn token_for_maps_op_to_its_edge() {
        let e = ProxyEntry {
            nonce: LaunchNonce::generate(),
            read: EdgeToken::generate(),
            write: EdgeToken::generate(),
            signal: EdgeToken::generate(),
        };
        assert!(e.token_for(Op::ReadScreen).unwrap().ct_eq(&e.read));
        assert!(e.token_for(Op::WriteInput).unwrap().ct_eq(&e.write));
        assert!(e.token_for(Op::Signal).unwrap().ct_eq(&e.signal));
        assert!(e.token_for(Op::DeriveLoop).is_none());
        // The durable-config + clipboard-exfil authorities are never carried by an
        // inherited edge — a child holds only the three base op tokens.
        assert!(e.token_for(Op::ConfigWrite).is_none());
        assert!(e.token_for(Op::ClipboardWrite).is_none());
    }

    /// A wedged child, whose listener queues connections without accepting them, is
    /// reported within the connect-probe bound as having accepted nothing (an error
    /// before any relay), and the verb never reaches it. The served child in
    /// `connect_and_relay_pipes_handshake_and_response` is the negative control.
    #[test]
    fn a_child_that_accepts_nothing_fails_the_forward_fast() {
        let dir = aterm_tempfile::TempDir::new_in("/tmp").expect("scratch");
        let sock = dir.path().join("wedged.sock");
        let _listener = aterm_uds::CtlListener::bind(&sock).expect("bind, never accept");
        let (_client_app, client_relay) = CtlStream::pair().expect("pair");
        let started = std::time::Instant::now();
        let mut relayed = false;
        let result = deliver_then_relay(
            &sock.to_string_lossy(),
            &ForwardLine::edge("tok-hex".into(), "@. text".into()),
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

    /// A child that refuses the probe (a stale edge token: `ERR auth`) fails the
    /// forward before the verb is sent.
    #[test]
    fn a_child_that_refuses_the_probe_never_receives_the_verb() {
        let dir = aterm_tempfile::TempDir::new_in("/tmp").expect("scratch");
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
            &sock.to_string_lossy(),
            &ForwardLine::edge("stale".into(), "@. key enter".into()),
            &client_relay,
            &[],
            std::time::Duration::from_secs(5),
            |_, _, _| Ok(()),
        );
        assert!(result.is_err(), "a refused probe is a failed forward");
        assert_eq!(child.join().unwrap(), "", "the verb was never sent");
    }

    /// A saturated host still answers: a listener with no lane free writes its busy
    /// line and closes without reading, so the probe's write fails (`EPIPE`) and the
    /// line left behind is the answer [`ForwardHost::serves`] takes as proof the
    /// host is alive.
    #[test]
    fn a_busy_line_left_behind_answers_a_probe_whose_write_failed() {
        let dir = aterm_tempfile::TempDir::new_in("/tmp").expect("scratch");
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
            let dir = aterm_tempfile::TempDir::new_in("/tmp").expect("scratch");
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
            let target = sock_s.clone();
            let relay = std::thread::spawn(move || {
                connect_and_relay(
                    &target,
                    &ForwardLine::edge("tok-hex".into(), "@. turn hi".into()),
                    &client_relay,
                    &[],
                )
            });
            let mut told = String::new();
            BufReader::new(&client_app)
                .read_to_string(&mut told)
                .expect("read to the relay's EOF");
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
            if case == Host::ServesOn {
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
    /// and raw non-UTF-8 bytes) transparently, presenting the TOKEN handshake to
    /// the "child" socket. A throwaway CtlListener stands in for the inner aterm.
    #[test]
    fn connect_and_relay_pipes_handshake_and_response() {
        let dir = std::env::temp_dir().join(format!("aterm-relay-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("child.sock");
        let sock_s = sock.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&sock);
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
        let first_line = ForwardLine::edge("tok-hex".into(), "screen".into());
        let relay = std::thread::spawn(move || {
            connect_and_relay(&sock_s, &first_line, &client_relay, &[]).expect("relay");
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
        let _ = std::fs::remove_dir_all(&dir);
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
        // detector, never a latency budget: a loaded host only slows them.
        speaker
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("deadline");
        let mut byte = [0u8; 1];
        let done = matches!(speaker.read(&mut byte), Ok(0));
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
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
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

    /// A child that half-closed reads end-of-file when the relay ends, though a copy
    /// of the relay's end of it survives (a `try_clone` here, as a fork holds one
    /// until it execs).
    ///
    /// The child answers and half-closes; the relay passes both on; the client reads
    /// them and half-closes in turn. Once the relay has returned, one non-blocking
    /// read of the child's end is the verdict. Negative control: restore both-ways
    /// `shutdown` calls, which Darwin refuses (`ENOTCONN`) on a socket whose read
    /// half the child's half-close shut, and the child reads `WouldBlock`.
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
        original
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("hang detector");
        let mut answer = String::new();
        (&original)
            .read_to_string(&mut answer)
            .expect("the answer, then the relay's half-close");
        assert_eq!(answer, "OK answer\n");
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
        drop(ChildProxy::register(
            SessionId::generate(),
            test_entry(),
            None,
        ));
        assert!(read_edge_tokens(&path).is_some(), "only its own file");
        // The parent owns removal: retiring the child removes exactly this file.
        drop(ChildProxy::register(sid, test_entry(), Some(file)));
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
        assert!(read_graph_entry(&dir, &sid).is_none(), "no entry pre-bind");

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
        let (got_sock, got_nonce) = read_graph_entry(&dir, &sid).expect("published");
        assert_eq!(got_sock, canon_sock);
        assert!(got_nonce.ct_eq(&nonce));

        // The mirror landed in the scratch dir, not the real default dir.
        assert!(
            read_graph_entry(&mirror, &sid).is_some(),
            "mirror lands in the scratch override dir"
        );
        if let Some(real) = real_default.as_ref() {
            assert!(
                read_graph_entry(real, &sid).is_none(),
                "the mirror must NEVER write into the user's real control dir"
            );
        }

        // Close: the entry is retired from both our dir and the scratch mirror.
        unpublish_session(&sid);
        assert!(
            flush_publications(aterm_spec::wait::MUST_HAPPEN),
            "the retire lands"
        );
        assert!(read_graph_entry(&dir, &sid).is_none(), "retired on close");
        assert!(
            read_graph_entry(&mirror, &sid).is_none(),
            "mirror entry retired too"
        );
        if let Some(real) = real_default.as_ref() {
            assert!(
                read_graph_entry(real, &sid).is_none(),
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
    /// retired after it, so no entry is left behind.
    #[test]
    fn a_session_publication_is_queued_off_the_caller_and_kept_in_order() {
        /// The process-global discovery state, cleared on every exit path: the
        /// queue drained while the scratch mirror still stands.
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
            read_graph_entry(&dir, &kept).is_none(),
            "publish_session wrote in the caller's turn"
        );
        release
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the publication thread reached the held write");
        assert!(
            flush_publications(aterm_spec::wait::MUST_HAPPEN),
            "the queue drained"
        );
        assert!(
            read_graph_entry(&dir, &kept).is_some(),
            "the publish landed"
        );
        assert!(read_graph_entry(&mirror, &kept).is_some(), "and its mirror");
        assert!(
            read_graph_entry(&dir, &closed).is_none()
                && read_graph_entry(&mirror, &closed).is_none(),
            "a session retired behind its publish leaves no entry"
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

    /// `drain_buffered` does not block on an empty buffer and a silent peer (the
    /// common one-line-forward case).
    #[test]
    fn drain_buffered_never_blocks_on_empty_buffer() {
        let (a, _b) = CtlStream::pair().expect("pair"); // _b stays open + silent
        let mut r = std::io::BufReader::new(a);
        assert!(
            drain_buffered(&mut r).is_empty(),
            "empty buffer drains to nothing, no block"
        );
    }

    /// `drain_buffered` returns exactly the bytes pipelined past the request line
    /// (so the relay forwards them first) and consumes them from the buffer.
    #[test]
    fn drain_buffered_returns_pipelined_leftovers() {
        use std::io::BufRead;
        let (a, b) = CtlStream::pair().expect("pair");
        // Peer sends a request line + pipelined trailing bytes in one write.
        (&b).write_all(b"verb line\nLEFTOVER").unwrap();
        drop(b);
        let mut r = std::io::BufReader::new(a);
        let mut line = String::new();
        r.read_line(&mut line).unwrap(); // consume the request line
        assert_eq!(line, "verb line\n");
        // fill the buffer (a real serve loop's next read would); then drain it.
        let _ = r.fill_buf().unwrap();
        assert_eq!(drain_buffered(&mut r), b"LEFTOVER");
        // Second drain is empty (consumed).
        assert!(drain_buffered(&mut r).is_empty());
    }

    /// An instance's quit retires only the discovery entries that name its
    /// socket: a rolled-back update candidate carries its live parent's session
    /// ids, whose entries must survive. Negative control: its own entry goes.
    #[test]
    fn a_quit_retires_only_its_own_graph_entries() {
        let dir = aterm_tempfile::TempDir::new_in("/tmp").unwrap();
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
        assert!(
            !graph_path(dir.path(), &mine).exists(),
            "its own entry goes"
        );
        assert!(
            graph_path(dir.path(), &parents).exists(),
            "the parent's entry stays with the parent"
        );
    }
}
