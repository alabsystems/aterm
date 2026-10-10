// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-process `@<sid>` proxy forward: turns the per-process control socket
//! into one address space spanning the recursion tree, so an outer aterm can
//! reach a session inside an inner aterm it spawned, or in a sibling instance.
//!
//! ## How a child hop works
//!
//! When `handle`/`serve` cannot resolve a `@<sid>` selector in this process's
//! store, it consults the [`ProxyTable`]: the per-op capability tokens this
//! aterm minted for each child it spawned (`ChildProvision`). The child's live
//! socket path comes from the on-disk graph entry the inner aterm wrote at bind
//! time ([`read_graph_entry`]). The forward then:
//!
//! 1. `CtlStream::connect`s the child's socket,
//! 2. presents `TOKEN <edge-hex> <rewritten-verb>`, which the child authorizes
//!    against the edges it installed from its injected env, so it grants
//!    exactly the op the verb needs, and
//! 3. relays bytes transparently in both directions until either side closes.
//!
//! The relay never parses framing, so it carries the styled `screen` JSON, the
//! `subscribe cells/bytes` push streams, `feed-bin` payloads and every other
//! verb verbatim.
//!
//! ## Scope: one hop per kind
//!
//! The edge-authority hop forwards direct children only: the child's selector
//! is inlined to `@.` so it runs the verb on itself, and a child is never in
//! its own proxy table, so no cycle can form. An Owner-scope `@<grandchild>`
//! needs no transitive edge forwarding: every instance publishes a graph entry
//! for every session it hosts into the shared runtime dir (a nested instance
//! inherits the parent's), so the grandchild resolves through the sibling hop
//! below. An edge-scoped connection never forwards (it falls through to local
//! resolution), so an edge cannot be escalated beyond its grant.
//!
//! ## Identity binding
//!
//! The tokens bind to the child's launch nonce (recorded in the graph entry): a
//! child relaunched under a fresh nonce no longer matches the parent's table,
//! so a stale forward fails closed at discovery rather than dialing a stranger.
//!
//! ## Sibling instances
//!
//! Two instances the user opened separately are siblings: same uid, same trust
//! domain, no parent-minted edge. Every instance publishes a graph entry for
//! every session it hosts, keyed by sid and pointing at its own socket
//! ([`publish_session`] / [`unpublish_session`], fed by [`set_self_sock`] at
//! bind). An `@<sid>` that resolves neither locally nor as a spawned child is
//! forwarded to the hosting sibling's socket, authenticated with that
//! instance's own per-launch token (`aterm-<pid>.token`, same-uid 0600: the
//! credential a same-uid `aterm-ctl --pid` client reads directly, so the relay
//! grants nothing the caller could not take itself). Owner-scope only; the
//! original selector is kept so the sibling resolves the session among its own
//! tabs. A stale entry pointing back at the forwarder's own socket is refused
//! (the self-dial guard), so a removed session degrades to `ERR no such
//! session`, never a loop.

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use aterm_session::{EdgeToken, LaunchNonce, Op, SessionId};
use aterm_uds::CtlStream;

#[cfg(unix)]
use crate::native_document_host::HeldAdvisoryLock;

/// The capability this aterm holds over ONE child it spawned: the child's launch
/// nonce (to validate the graph entry) plus the three per-op edge tokens minted
/// at spawn (Item 4). The child's socket PATH is not stored here — it is
/// discovered live from the graph entry, since the child binds it only once it
/// (an inner aterm) actually starts.
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
/// successful bind (never for a disabled socket).
///
/// The path is stored canonical: the self-dial guard compares it against
/// `confine_proxy_sock`'s `canonicalize(dir)`-rooted output, and a raw path
/// under a symlinked ancestor (`$HOME`, `/var` → `/private/var`) would defeat
/// the guard and let a self-pointing entry relay requests back into this
/// server without bound. If canonicalization fails, it falls back to a
/// canonical dir joined with the file name, like `confine_proxy_sock`.
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
pub(crate) fn publish_session(sid: &SessionId, nonce: &LaunchNonce) {
    let guard = SELF_SOCK.read().unwrap_or_else(|p| p.into_inner());
    if let Some((dir, sock)) = guard.as_ref() {
        // On disk goes the spelling every reader can dial and dedupe. The
        // recorded path is canonical, which on Windows is the verbatim
        // `\\?\C:\…` form the bind-time publish does not use; un-verbatim it so
        // one instance is not listed twice. The in-memory record stays canonical.
        publish_graph_entry(dir, sid, &published_sock_spelling(sock), nonce);
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
/// such session`, never a wrong target.
pub(crate) fn unpublish_session(sid: &SessionId) {
    let guard = SELF_SOCK.read().unwrap_or_else(|p| p.into_inner());
    if let Some((dir, _)) = guard.as_ref() {
        retire_graph_entry(dir, sid);
    }
}

/// Read the per-launch auth token of the sibling instance whose socket is
/// `sock_path` (`<dir>/aterm-<pid>.sock` → `<dir>/aterm-<pid>.token`; an
/// explicit socket → `<sock>.token`; 0600, same-uid). This is the credential a
/// same-uid `aterm-ctl --sock <path>` client reads, through the same function
/// ([`aterm_ctl::read_token_beside`]), so presenting it grants nothing the
/// caller could not obtain directly. `None` (fail closed) for an unreadable
/// token. The forward reads it once its connection to the sibling exists
/// ([`ForwardToken::BesideSocket`]).
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
/// `remove_graph_entry`). Mirrors `control_auth::sweep_stale_instances`;
/// best-effort, keeping the dir bounded. Called at spawn.
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
/// `<sock_dir>/edges/<child-sid>` and return its absolute path, or `None` if
/// the private dir or file cannot be created. The bearer tokens live only here
/// (a 0600 file in the 0700 socket dir), never in inheritable env. Three
/// lines: `read <hex>` / `write <hex>` / `signal <hex>`.
///
/// The file persists for the parent session and is not consumed on read
/// ([`read_edge_tokens`] is repeatable): the child inherits the path in
/// `ATERM_EDGE_TOKENS` pinned in its shell env, so a child aterm relaunched in
/// the same shell must re-read the same secrets to re-install the parent
/// edges. The trust boundary (same-uid + 0600) matches the per-launch auth
/// token file. The parent owns the file: the [`ChildProxy`] that records its
/// path removes it when the child's session ends. `ATERM_EDGE_TOKENS` stays
/// deny-listed across a new aterm hop, so only a same-shell relaunch re-reads
/// it.
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
    #[cfg(unix)]
    sweep_dead_edge_files(&dir);
    let path = dir.join(child_sid.as_str());
    let body = format!("read {read_hex}\nwrite {write_hex}\nsignal {signal_hex}\n");
    let f = open_private(&path).ok()?;
    // THE OWNER'S LOCK, taken before the secret is written: the parent holds it
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
/// handle whose `flock` marks its owner alive: held by the [`ChildProxy`] for
/// the child's life, released when it drops (after the file is removed) — by
/// `LOCK_UN`, not by the close alone ([`OwnerLock`]): a child any thread
/// forks while it is held keeps a copy of its description until that child
/// execs, and the sweep read a parent gone that long as live.
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

/// Read the three edge-token hexes `(read, write, signal)` from the 0600 file at
/// `path` (written by [`write_edge_tokens`]), or `None` if absent / malformed.
/// Same-uid + 0600 is the access gate (the path is not secret; the file is).
///
/// Non-destructive and repeatable for the parent session's lifetime: a child
/// relaunched in the same shell re-reads the file to re-install the parent
/// edges. The parent owns the file's removal ([`ChildProxy`]).
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
/// Dropping it retires both. The session that spawned the child holds it, so
/// a closed tab deregisters its child and [`proxies`] does not grow for the
/// process lifetime (`aterm_spec::derive::proxy_registry_model`); a spawn that
/// fails after provisioning drops it on the way out, sharing the same
/// teardown. The file removed is the exact path written, never one
/// re-derived from a socket dir that may resolve differently by then.
///
/// There is no liveness-based sweep of `edges/`: a freshly provisioned child
/// has no discovery entry until it launches (maybe much later, or never), so
/// "no live graph entry" cannot tell a still-needed file from an orphan. A
/// file orphaned by a crashed parent is inert: its tokens authorize only
/// against the dead child's exact `(sid, nonce)`, both random and never
/// reissued.
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

/// Parse a graph-entry body (`sock <path>\nnonce <hex>\n`). The `sock` line is
/// the shared on-disk format (`aterm_types::control_socket::graph_entry_sock`,
/// also read by `aterm-ctl`'s self-location); the nonce line is server-only.
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
    /// What the client is told when the host hung up first: the host-gone
    /// line ([`aterm_types::control_verbs::host_gone_line`]) when the host no
    /// longer serves at its socket — nothing answers there, another process
    /// does (a successor that rebound it), or the same pid does through
    /// another listener (an `exec` that kept it) — and nothing when it does
    /// (it ended the exchange on purpose, as a stream whose sessions ended),
    /// or when its pid could not be read (a bare end of stream, as before).
    fn gone_line(&self) -> Option<String> {
        let pid = self.pid?;
        (!self.serves(pid)).then(|| aterm_types::control_verbs::host_gone_line(pid, &self.sock))
    }

    /// Whether `pid` still answers at the host's socket through the listener
    /// the forward went through: a connection there is that process's and
    /// answers the connect probe with any line — `OK`, or a refusal (busy,
    /// auth), which only a live listener gives — and the socket file is still
    /// the one that accepted the forward, read after the answer.
    ///
    /// A PID ALONE DOES NOT NAME THE HOST ([`aterm_uds::listener`]): the cold
    /// update lane shuts down every stream it accepted, this forward
    /// included, and its `exec` keeps the pid and binds the same path — or an
    /// `exec` that returned reopens it. Judged by pid, that process answering
    /// was the host ending the exchange on purpose: no line, and the client,
    /// finding this aterm still answering, read a bare EOF as a deliberate
    /// end (review of the round-eight fixes).
    ///
    /// A busy answer often comes with no pid: the listener writes its line and
    /// closes at once, and Darwin answers `ENOTCONN` to `LOCAL_PEERPID` once
    /// the peer has closed (aterm-ctl's `version_at`). The recorded listener,
    /// unchanged after the answer, then names the host, as aterm-ctl's
    /// `answers_at` judges a successor; without one, an answer with no pid
    /// proves nothing.
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
    // The host's pid is read NOW, while the connection is surely up: Darwin answers
    // `ENOTCONN` to `LOCAL_PEERPID` once the peer has closed, and the host this
    // exists for (an update's Commit `_exit`s it) hangs up as soon as it has read
    // the verb - read after the delivery, a relay that was descheduled between the
    // two found no pid, and the client a bare EOF instead of `ERR gone`.
    let pid = aterm_uds::fdpass::peer_pid(&child).ok();
    (&child).write_all(first_line.verb.as_bytes())?;
    (&child).write_all(b"\n")?;
    if !client_prebuffered.is_empty() {
        (&child).write_all(client_prebuffered)?;
    }
    (&child).flush()?;
    // Past this point the verb HAS been delivered to the child. A relay-stage
    // failure (e.g. a `try_clone` under fd exhaustion) tears the connection down
    // but must NOT be reported as "forward failed" — that would be a false
    // negative for an op that already reached the child. Only a connect/handshake
    // error (the `?`s above, before any byte was delivered) surfaces as `Err` so
    // the caller can honestly answer `ERR forward`. Nor is the host going away
    // under the delivered verb: the relay says THAT, in its own words
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
/// half-close order. EOF after a child response becomes `Shutdown::Write` on
/// the original client only after every preceding byte was copied and flushed;
/// client EOF then becomes `Shutdown::Write` on the child. Guarded artifact
/// replies need this: the client's post-response ACK must travel client →
/// child after the complete child → client response.
///
/// A real copy error still shuts both sockets down to unblock the paired pump.
///
/// THE HOST HANGING UP FIRST IS SAID (round eight of the update audit, finding
/// 12): when the child's side ends while the client is still there, the host
/// is looked at once ([`ForwardHost::gone_line`]), and one that no longer
/// serves — an update's Commit `_exit`ed it, or it crashed or quit — leaves the
/// client the host-gone line before its EOF. A bare EOF from this aterm, which
/// still answers, read to the client as an end on purpose: a `subscribe` exited
/// 0 ("session over") with the session alive in the host's successor.
///
/// Every teardown here is a HANG-UP of each half on its own
/// ([`aterm_uds::hangup::hang_up`]), never a both-ways `shutdown`: by then one
/// end has usually half-closed toward the relay (the child that answered, the
/// client that sent its request), and Darwin refuses a both-ways `shutdown` of
/// that end, shutting NOTHING — so the peer still waiting on it read no EOF
/// while any copy of the relay's descriptor lived (any thread's fork holds
/// one until it execs).
///
/// THE WORKER'S EXIT NEVER WAITS ON A WAKE FROM ANOTHER THREAD: every hang-up
/// of the child is preceded by setting `stop`, and the worker polls the
/// child's end ([`crate::control::HANGUP_POLL`]) before each read, re-reading
/// `stop` whenever a poll times out. Under load a read already parked on one
/// descriptor was not woken by a `shutdown(SHUT_RD)` made through a sibling
/// descriptor of the same socket, and the relay's join hung for the test's
/// whole two-minute detector. The same holds in reverse: the worker's one
/// hang-up of the client (its copy broke) sets `client_stop` first, and this
/// thread's client read waits the same way, so a silent client that never
/// half-closes cannot park it past a lost wake either.
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
    // Set before EVERY hang-up of the child: the worker stops at its next
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
/// relay. A freshly handshaked connection usually has none.
///
/// Uses [`BufReader::buffer`], never `fill_buf()`: `fill_buf` blocks on an
/// empty buffer, the common case for a one-line forward request (the client
/// sent `TOKEN <hex> @<sid> <verb>\n` and awaits the reply), and would
/// deadlock before the relay starts. `buffer()` returns pipelined leftovers or
/// an empty slice, with no syscall.
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

    /// A host whose pid was not read: a hang-up from it is relayed as a bare
    /// end of stream, as before the host-gone line.
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

    #[test]
    fn graph_entry_roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("aterm-graph-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        write_graph_entry(&dir, &sid, "/run/user/1000/aterm/aterm-42.sock", &nonce);
        let (sock, got_nonce) = read_graph_entry(&dir, &sid).expect("entry exists");
        assert_eq!(sock, "/run/user/1000/aterm/aterm-42.sock");
        assert!(got_nonce.ct_eq(&nonce));
        // A different sid has no entry.
        assert!(read_graph_entry(&dir, &SessionId::generate()).is_none());
        remove_graph_entry(&dir, &sid);
        assert!(read_graph_entry(&dir, &sid).is_none(), "removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An instance bound to an explicit `--control-sock` outside the default
    /// rendezvous dir mirrors its session graph entry into the default dir (the
    /// only one the flagless `aterm-ctl` client reads), and the client recovers
    /// the absolute socket path and nonce from that entry, so a flagless
    /// in-session call reaches the hosting instance. Parsed with the shared
    /// helper (`control_socket::graph_entry_sock`) the client's self-location
    /// uses.
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

    /// A wedged child, whose listener queues connections without accepting
    /// them, is reported within the connect-probe bound as having accepted
    /// nothing (an error before any relay, so the caller answers it), and the
    /// verb never reaches it. The served child in the test above is the
    /// negative control.
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

    /// A SATURATED HOST STILL ANSWERS. A listener with no lane free writes its
    /// busy line and closes without reading; a probe written after that close
    /// fails (`EPIPE`), and the line left behind is its answer — the proof
    /// [`ForwardHost::serves`] takes that the host is alive. FAILS WITHOUT THE
    /// FIX: the probe is `Err(BrokenPipe)`, and a relay told its client a
    /// live host went away.
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

    /// A HOST THAT GOES AWAY UNDER A FORWARD LEAVES ITS CLIENT A TYPED LINE
    /// (round eight of the update audit, finding 12). The aterm a forward
    /// reached (a sibling, a child) self-updates and `_exit`s at its Commit;
    /// the relay copied its EOF through and half-closed the client, which then
    /// found THIS aterm still answering and read the end as deliberate — a
    /// `subscribe` exited 0, a `turn` exited 1. Now, when the host hangs up
    /// first and no longer serves where it was reached, the client is told so
    /// before the EOF: `ERR gone pid=<host> sock=<its socket>`, a line of its
    /// own (never `ERR forward`: the verb was delivered). `Gone`: its listener
    /// closed, then the connection. `Reopened`: the cold update lane — the
    /// SAME pid on a fresh listener renamed over the path (an `exec` that kept
    /// the pid, or `Endpoint::reopen` after one that returned), then the
    /// forwarded connection shut (`Endpoint::retire`); judged by pid it read
    /// as the host serving on (review of the round-eight fixes). NEGATIVE
    /// CONTROL, `ServesOn`: a host that ends the exchange and goes on serving
    /// through the same listener leaves a bare EOF, as before. FAILS WITHOUT
    /// THE FIX: the client reads a bare EOF from `Gone` and from `Reopened`.
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
        // client→child reader hits EOF and the relay thread returns. Closing
        // its handles is not that while a sibling's fork holds a copy of one,
        // and the both-ways `shutdown` this made before is refused on Darwin,
        // shutting nothing, once the relay has half-closed toward the client.
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
        // The response and the challenge MUST cross the relay: the read bound
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
    /// `aterm_spec::derive::relay_teardown_model` (`RelayTeardown`): when the
    /// relay tears down, both read halves of both local sockets are shut, so a
    /// pump parked on a clone of either gets EOF and the worker joins, leaving
    /// no parked thread or held fd.
    ///
    /// `relay_bidirectional` has two error arms, one per pump, each driven by
    /// the fault that exercises it while both peers stay open and silent
    /// ([`RelayFault`]): a failed write toward the child (the main thread's
    /// arm) and toward the original client (the worker's arm). For each, the
    /// projection ([`relay_teardown_under`]) must be the model's `Teardown`
    /// step, and the relay must have returned.
    ///
    /// Negative control: `Buggy = 1` (`shutdown(Write)` only) must reject every
    /// real step. Turning either arm's hang-ups ([`hang_up`]: the write half,
    /// then the read half) into `shutdown(Write)` leaves both read halves open,
    /// the step only the mutant admits.
    #[test]
    #[cfg(unix)]
    fn relay_teardown_conforms_to_readers_unblock_after_teardown() {
        use aterm_spec::verify::validate_transition_tiered;
        let model = aterm_spec::derive::relay_teardown_model();
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

    /// THE HOST PUMP ENDS ON THE STOP FLAG ALONE: the lost wake, staged by
    /// sending none. The pump waits on a silent host whose socket nobody shuts;
    /// once the pump is about to wait (the handshake), only `stop` is set, and
    /// the pump must still end, as the host's side ending. `stop` is read only
    /// after a poll times out, so a flag set before the poll is entered still
    /// exercises the lost-wake path. Both waits are hang detectors. Negative control: drop the `stop` re-read after a poll
    /// timeout and the pump parks forever.
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

    /// A CHILD THAT HALF-CLOSED READS END-OF-FILE WHEN THE RELAY ENDS, though a
    /// copy of the relay's end of it survives — the copy any thread's fork
    /// holds until it execs (a `try_clone` here: one more descriptor on the
    /// same socket, which is all the kernel counts).
    ///
    /// The child answers and half-closes, still waiting for its client's EOF;
    /// the relay passes the answer and the half-close on; the client reads both
    /// and then half-closes in turn. Each read waits on a fact the real relay
    /// must reach, so its bound is a hang detector. Once the relay has
    /// returned, every shutdown it will ever make is made, so one non-blocking
    /// read of the child's end is the verdict.
    ///
    /// NEGATIVE CONTROL (run by restoring the both-ways `shutdown` calls): the
    /// relay's teardown of the child-side socket, whose read half the child's
    /// half-close had shut, was refused on Darwin (`ENOTCONN`) and shut nothing
    /// — so the child read `WouldBlock`, not its end-of-file, while the copy
    /// lived.
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

    /// F1 (revised): edge-token secrets round-trip through the 0600 file, the file
    /// is owner-only (0600), and the read is REPEATABLE — it PERSISTS for the
    /// session so a child re-launched in the same shell can re-read it. The PARENT
    /// removes it at teardown: the `ChildProxy` holding its path drops.
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
    /// its file's owner lock for the child's life; at the next provisioning a
    /// file whose lock is free (here, its handle dropped without the clean
    /// retire's removal, as a crash leaves it) is removed. Negative controls:
    /// a live parent's file (its `EdgeFile` held) and an empty file (a writer
    /// between create and lock) stay.
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
        // The lock released, the file left: a crashed parent. The drop
        // releases it by LOCK_UN ([`EdgeFile`]), so a child forked while it
        // was held — made here for real, and parked short of its exec through
        // the sweep — holds no copy of a lock the sweep would read the parent
        // as live by. Released by the close alone, it did, and the dead
        // parent's file stayed.
        let forked = crate::parked_fork::ParkedChild::fork();
        drop(crashed);
        let edges = dir.join("edges");
        let empty = edges.join(SessionId::generate().as_str());
        std::fs::write(&empty, b"").unwrap();
        assert!(
            crashed_path.exists(),
            "nothing sweeps before a provisioning"
        );
        let next = write_edge_tokens(
            &dir,
            &SessionId::generate(),
            &hex("11"),
            &hex("22"),
            &hex("33"),
        )
        .expect("next");
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
        // Redirect the rendezvous mirror into a scratch dir so publishing never
        // touches the user's real control dir: `dir` is not the default dir,
        // so `publish_session` would otherwise mirror into
        // `aterm_uds::control_socket_dir()`. The override is held under the
        // self-sock guard and cleared before it drops.
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

        // Post-bind: the entry appears, pointing at our socket, with the nonce.
        // `set_self_sock` stores the canonical path, so the published entry is
        // canonical even when the raw path went through a symlinked ancestor
        // (macOS temp: /var → /private/var).
        let sock = dir.join("aterm-88001.sock").to_string_lossy().into_owned();
        // …in the published spelling: Windows' canonical form is the verbatim
        // `\\?\C:\…`, which `publish_session` un-verbatims so one socket has
        // one spelling in the graph (`published_sock_spelling`, identity on Unix).
        let canon_sock = published_sock_spelling(
            &std::fs::canonicalize(&dir)
                .unwrap()
                .join("aterm-88001.sock")
                .to_string_lossy(),
        );
        set_self_sock(&dir, &sock);
        publish_session(&sid, &nonce);
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

    /// `drain_buffered` must not block when the BufReader has no buffered
    /// bytes and the peer is silent (the common one-line-forward case). A
    /// blocking drain makes this test hang.
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
