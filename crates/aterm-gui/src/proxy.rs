// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cross-process `@grandchild` PROXY forward (Item 5b) — the layer that turns the
//! per-process control socket into a UNIFIED address space spanning the recursion
//! tree, so an outer aterm can reach a session inside an inner aterm it spawned.
//!
//! ## How a hop works
//!
//! When `handle`/`serve` cannot resolve a `@<sid>` selector in THIS process's
//! store, it consults the [`ProxyTable`] — the per-op capability tokens this aterm
//! minted for each child when it spawned it (Item 4's `ChildProvision`). The
//! child's live socket path is discovered from the on-disk graph entry the inner
//! aterm wrote at bind time ([`read_graph_entry`]). The forward then:
//!
//! 1. `CtlStream::connect`s the child's socket,
//! 2. presents `TOKEN <edge-hex> <rewritten-verb>` — the child authorizes it
//!    against the edges it installed from its injected env (Item 4), so the op the
//!    parent granted is exactly the op the verb needs, and
//! 3. RELAYS bytes transparently in both directions until either side closes.
//!
//! The relay is format-agnostic — it never parses framing — so it carries the
//! styled `screen` JSON, the `subscribe cells/bytes` push streams, `feed-bin`
//! binary payloads, and every other verb verbatim. Authority is the parent's
//! per-op edge over the child it spawned (presented on the dial), so the child
//! authorizes the EXACT op the verb needs.
//!
//! ## Scope: one hop per kind
//!
//! The EDGE-authority hop forwards DIRECT children only, by design — the child's
//! own selector is inlined to `@.` so it runs the verb on itself, and a child is
//! never in its own proxy table, so no cycle can form. An Owner-scope
//! `@<grandchild>` needs no transitive edge forwarding: every instance publishes
//! a graph entry for every session it hosts into the shared runtime dir (a
//! nested instance inherits the parent's), so the grandchild resolves through
//! the SIBLING hop below and is dialed directly with its own instance's token.
//! Transitive edge-token forwarding is intentionally absent: an edge-scoped
//! connection never forwards (it falls through to local resolution), so an edge
//! cannot be escalated into reach its grant did not name.
//!
//! ## Identity binding
//!
//! The tokens bind to the child's launch NONCE (recorded in the graph entry): a
//! child relaunch under a fresh nonce makes the graph nonce mismatch the table
//! the parent retained, so a stale forward fails closed at discovery rather than
//! dialing a re-launched stranger.
//!
//! ## Sibling instances (the second hop kind)
//!
//! Spawned children are not the only "other aterm" a user has: two terminals the
//! user opened SEPARATELY (two windows / two instances) are SIBLINGS — same uid,
//! same trust domain, no parent-minted edge between them. For those, every
//! instance publishes a graph entry for EVERY session it hosts (not just its
//! root), keyed by sid, pointing at its own instance socket ([`publish_session`]
//! / [`unpublish_session`], fed by the process-wide [`set_self_sock`] recorded at
//! bind). An `@<sid>` that resolves neither locally nor as a spawned child is
//! then forwarded to the hosting sibling's socket, authenticated with that
//! instance's OWN per-launch token (`aterm-<pid>.token`, same-uid 0600 — exactly
//! the credential a same-uid `aterm-ctl --pid` client reads directly, so the
//! relay grants nothing the caller could not already take by dialing the sibling
//! itself). Owner-scope only; the original `@<sid>` selector is kept so the
//! sibling resolves the session among ITS OWN tabs. A stale entry pointing back
//! at the forwarder's own socket is refused (the self-dial guard), so a removed
//! session degrades to `ERR no such session`, never a loop.

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use aterm_session::{EdgeToken, LaunchNonce, Op, SessionId};
use aterm_uds::CtlStream;

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
    /// `ConfigWrite`, and `ClipboardWrite` have NO provisioned edge, so they are
    /// refused (`None`) — a child is provisioned only the three base ops, so the
    /// durable-config and clipboard-exfil authorities are never carried by an
    /// inherited edge (only the instance Owner holds them).
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

/// The process-wide proxy table: ONE per aterm process (the spawn path inserts a
/// child's capability; the control server reads it to forward). A singleton avoids
/// threading the handle through every `spawn_session`/`serve` caller; correctness-
/// wise a process has exactly one recursion fabric.
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
/// An `RwLock<Option<..>>` (not a `OnceLock`) so tests can set AND clear it.
static SELF_SOCK: RwLock<Option<(std::path::PathBuf, String)>> = RwLock::new(None);

/// Record this instance's bound control socket so session registration can
/// publish per-session graph entries ([`publish_session`]) and the sibling
/// forward can refuse to dial itself. Called once by the control server AFTER a
/// successful bind (never for a disabled socket).
///
/// The path is stored in CANONICAL form: the self-dial guard compares it
/// against `confine_proxy_sock`'s output, which is `canonicalize(dir)`-rooted.
/// Storing the raw path would defeat the guard on any host whose socket dir
/// has a symlinked ancestor (a symlinked `$HOME`, an `XDG_RUNTIME_DIR` under
/// `/var` → `/private/var`, …) — the exact fail-open a review adversary found:
/// a self-pointing graph entry would then relay a request back into this same
/// server without bound. The socket exists at record time (we just bound it),
/// so canonicalization only falls back on exotic filesystems — and then to a
/// dir-canonical + filename join, mirroring `confine_proxy_sock`'s own shape.
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

/// Test-only: serialize every test that touches the process-global recorded
/// self socket (tests run on parallel threads; two of them mutating
/// [`SELF_SOCK`] concurrently would flake). Acquire this FIRST, hold it for the
/// test's whole self-sock window, and `clear_self_sock` before dropping it.
#[cfg(test)]
pub(crate) fn self_sock_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Publish the discovery graph entry for a session THIS instance hosts, so a
/// SIBLING instance (and the flagless `aterm-ctl` client) can resolve `@<sid>` to
/// our socket. No-op until the control socket is bound ([`set_self_sock`]);
/// best-effort. Uses [`publish_graph_entry`], so an instance on an explicit
/// `--control-sock` ALSO lands its entry in the default rendezvous dir the
/// client reads. Called at the session-registration seam for every session.
pub(crate) fn publish_session(sid: &SessionId, nonce: &LaunchNonce) {
    let guard = SELF_SOCK.read().unwrap_or_else(|p| p.into_inner());
    if let Some((dir, sock)) = guard.as_ref() {
        // What goes on DISK is the spelling every reader can dial and dedupe:
        // the recorded path is canonical (the self-dial guard needs that), and
        // on Windows canonical means the verbatim `\\?\C:\…` form — which is
        // not what the bind-time publish above wrote for the root session, so
        // one instance's sessions reached `aterm-ctl` under two spellings and
        // it listed the instance twice (2026-09-22, 0.90.0). Un-verbatim it
        // here; the in-memory record keeps its canonical form untouched.
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
/// hosting instance re-checks its own store, so a stale entry can only produce
/// `ERR no such session`, never a wrong target.
pub(crate) fn unpublish_session(sid: &SessionId) {
    let guard = SELF_SOCK.read().unwrap_or_else(|p| p.into_inner());
    if let Some((dir, _)) = guard.as_ref() {
        retire_graph_entry(dir, sid);
    }
}

/// Read the per-launch AUTH token of the SIBLING instance whose socket is
/// `sock_path` (`<dir>/aterm-<pid>.sock` → `<dir>/aterm-<pid>.token`, and an
/// explicit socket → its own `<sock>.token`; 0600, same-uid). This is the
/// exact credential a same-uid `aterm-ctl --sock <path>` client reads for
/// itself, so presenting it on a forward grants nothing the caller could not
/// already obtain directly. `None` (fail closed) for an unreadable/empty token
/// or a path with no filename.
///
/// The candidates come from the shared rule
/// ([`aterm_types::control_socket::token_names_for_sock`]) and are tried in ITS
/// order: the sibling's own per-socket file first, and the legacy shared
/// `aterm.token` only when that one is ABSENT — which is what a sibling from a
/// build before the per-socket token wrote. A per-socket file that exists but
/// is empty ends the search (fail closed): it belongs to the instance being
/// dialed, and reaching past it for a directory-shared file would be reaching
/// for someone else's credential. Even then nothing is granted — a sibling
/// compares `AUTH` against the token it minted in memory, so a foreign value
/// is refused exactly as no token is.
#[must_use]
pub(crate) fn read_sibling_token(sock_path: &str) -> Option<String> {
    let p = Path::new(sock_path);
    let dir = p.parent()?;
    let name = p.file_name()?.to_string_lossy();
    for token_file in aterm_types::control_socket::token_names_for_sock(&name) {
        // The first candidate that EXISTS decides; only a missing (or
        // unreadable) file moves on to the legacy name.
        let Ok(raw) = std::fs::read_to_string(dir.join(token_file)) else {
            continue;
        };
        let t = raw.trim().to_string();
        return if t.is_empty() { None } else { Some(t) };
    }
    None
}

/// Write the discovery graph entry an inner aterm publishes at bind time so its
/// parent can reach it: `<sock_dir>/graph/<self-sid>` (0600) with three lines
/// `sock <abs-path>\nnonce <hex>\npid <n>\n`. The `pid` is THIS (the hosting)
/// process's — recorded so the flagless client's `instances`/`ls` can report a
/// pid even for an explicit-socket instance whose socket filename encodes none.
/// Edge tokens are NEVER written here — they live only in the child's 0600
/// edge-token file ([`write_edge_tokens`]), whose path rides `ATERM_EDGE_TOKENS`.
/// Best-effort: a write failure just means the parent cannot reach us by proxy
/// (direct per-instance reach still works).
///
/// This writes into ONE dir only; [`publish_graph_entry`] wraps it to ALSO
/// mirror into the well-known default rendezvous dir when the instance runs on
/// an explicit `--control-sock` outside it.
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

/// Test-only override for the mirror TARGET dir. Production leaves this `None`
/// and the mirror lands in the real `aterm_uds::control_socket_dir()`; a unit
/// test that exercises the publish/mirror path points this at a scratch TempDir
/// so it never creates/tightens-perms-on the USER'S REAL control dir. Serialized
/// with the self-sock state via [`self_sock_test_guard`] (the publish tests that
/// touch this hold that guard), so a concurrent [`rendezvous_mirror_dir`] reader
/// never observes a transient override.
#[cfg(test)]
static MIRROR_DIR_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Test-only: redirect the rendezvous mirror target to `dir` (or clear it with
/// `None`). The caller MUST hold [`self_sock_test_guard`] and clear it before
/// dropping the guard, so the override is never visible outside its own test.
#[cfg(test)]
pub(crate) fn set_mirror_dir_override(dir: Option<PathBuf>) {
    *MIRROR_DIR_OVERRIDE
        .write()
        .unwrap_or_else(|p| p.into_inner()) = dir;
}

/// The well-known DEFAULT control dir (`aterm_uds::control_socket_dir`) to ALSO
/// publish a graph entry into, or `None` when `sock_dir` already IS it (the
/// default per-instance case — no mirror needed) or the per-user base cannot be
/// resolved. The flagless `aterm-ctl` client only ever reads the default dir, so
/// an instance launched on an explicit `--control-sock` (whose entries would
/// otherwise land ONLY beside that socket) must mirror here to stay discoverable.
fn rendezvous_mirror_dir(sock_dir: &Path) -> Option<PathBuf> {
    // Under test, a set override stands in for the real default dir so no unit
    // test ever writes into the user's real control dir (production: no override).
    #[cfg(test)]
    {
        // Receiver named ON the acquisition line: the lock-order census
        // resolves identities from the `.read()` line's receiver, and the
        // rustfmt-split method chain left it receiver-less (the one standing
        // UNKNOWN-identity site).
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
/// ancestor — a `/var` → `/private/var` runtime dir — does not read as different),
/// else a lexical fallback. A false "different" only costs one harmless duplicate
/// write; a false "same" would just skip the (redundant) mirror.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Publish a session's discovery graph entry into the instance's OWN `sock_dir`
/// AND — when that dir is not the default rendezvous dir (i.e. this instance runs
/// on an explicit `--control-sock`) — into the default dir too, so the
/// flagless `aterm-ctl` client (which only ever reads the default dir) can still
/// self-locate and enumerate this instance. The entry carries the ABSOLUTE `sock
/// <path>`, so the client resolves the right socket wherever it actually lives.
/// Best-effort + idempotent: a default per-instance instance writes ONCE (the
/// mirror dir collapses to the same dir and is skipped), and a default dir that
/// is unavailable/unwritable simply degrades to the own-dir entry (no crash — a
/// headless explicit-socket instance still runs, just undiscoverable by flagless
/// clients until the default dir is writable).
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

/// Retire a session's discovery entry from BOTH its own dir and the mirrored
/// default rendezvous dir (the inverse of [`publish_graph_entry`]). Best-effort;
/// a leftover is harmless (the nonce guard fails a stale dial closed and the
/// client re-probes liveness), so this is hygiene, not correctness.
pub(crate) fn retire_graph_entry(sock_dir: &Path, sid: &SessionId) {
    remove_graph_entry(sock_dir, sid);
    if let Some(rv) = rendezvous_mirror_dir(sock_dir) {
        remove_graph_entry(&rv, sid);
    }
}

/// Open (create/truncate) a private sidecar file for writing. Unix: mode
/// `0600` — the shipping chain, verbatim. Windows: a plain create — there are
/// no POSIX mode bits; the file inherits the private dir's per-user ACL,
/// which is the (startup-disclosed) boundary there.
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
/// session's socket path is not left for a parent to dial. (A leftover is harmless
/// anyway — the nonce guard fails a stale dial closed — so this is hygiene.)
pub(crate) fn remove_graph_entry(sock_dir: &Path, sid: &SessionId) {
    let _ = std::fs::remove_file(graph_path(sock_dir, sid));
}

/// Sweep dead discovery entries: remove any `graph/<sid>` whose recorded socket no
/// longer has a live listener — a crashed session that never ran its graceful
/// `remove_graph_entry`. Mirrors `control_auth::sweep_stale_instances` for the
/// sibling per-instance files; best-effort (the nonce guard already fails a stale
/// dial closed, so this only keeps the dir bounded). Called at spawn.
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

/// Write the parent→child edge-token SECRETS to a 0600 file under
/// `<sock_dir>/edges/<child-sid>` (audit finding F1) and return its absolute path,
/// or `None` if the private dir / file cannot be created. The bearer tokens live
/// ONLY here (a 0600 file in the 0700 socket dir), never in inheritable env — so a
/// same-uid peer that cannot read 0600 files cannot obtain them. Three lines:
/// `read <hex>` / `write <hex>` / `signal <hex>`.
///
/// LIFECYCLE (F1, revised): the file PERSISTS for the parent session — it is NOT
/// consumed on the child's first read ([`read_edge_tokens`] is now repeatable).
/// The reason is the SAME-SHELL relaunch case: the child inherits the file PATH in
/// `ATERM_EDGE_TOKENS` pinned in its shell env, so a child aterm that exits and is
/// re-launched in the same shell must be able to re-read the same secrets to
/// re-install the parent edges; a consume-on-read deleted the file after the first
/// launch, breaking every subsequent relaunch (the outer's `@child` proxy answered
/// `ERR auth`). The secret window therefore widens from "write→first-read" to the
/// parent's session lifetime — which matches the EXISTING per-launch AUTH token
/// file (`aterm-<pid>.token`), also 0600 in the same 0700 same-uid dir for the
/// whole session, so the trust boundary (same-uid + 0600) is unchanged. The PARENT
/// owns the file: the [`ChildProxy`] that records its path removes it when the
/// child's session ends (see there for why a crash leftover needs no sweep).
/// Inheritance across a NEW aterm hop is still blocked — `ATERM_EDGE_TOKENS` stays
/// deny-listed, so only a same-shell relaunch (which re-inherits the pinned path)
/// re-reads it.
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
    let mut f = open_private(&path).ok()?;
    // THE OWNER'S LOCK, taken before the secret is written: the parent holds it
    // for the child's life ([`EdgeFile`]), so a later sweep can tell a live
    // parent's file from a dead one's ([`sweep_dead_edge_files`]).
    #[cfg(unix)]
    let locked = lock_owner(&f);
    #[cfg(not(unix))]
    let locked = true;
    if !locked || f.write_all(body.as_bytes()).is_err() {
        // The open created the file, and a partial write left some secret in it.
        // No `ChildProxy` will record a path this returns no name for, so nothing
        // else would ever remove it.
        drop(f);
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(EdgeFile { path, _lock: f })
}

/// One edge-token file this aterm wrote ([`write_edge_tokens`]) and the open
/// handle whose `flock` marks its owner alive: held by the [`ChildProxy`] for
/// the child's life, released when it drops (after the file is removed).
#[derive(Debug)]
pub(crate) struct EdgeFile {
    path: PathBuf,
    _lock: std::fs::File,
}

impl EdgeFile {
    /// The file's absolute path — what rides the child's env.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Take `f`'s exclusive owner lock without waiting (`flock(LOCK_EX|LOCK_NB)`
/// on its own open description): `false` if it cannot be taken.
#[cfg(unix)]
fn lock_owner(f: &std::fs::File) -> bool {
    use std::os::unix::io::AsRawFd as _;
    // SAFETY: `f` is an open file for the duration of the call; LOCK_NB never
    // waits.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// Remove the edge-token files in `dir` whose owning parent is gone (the
/// audit of 2026-09-25: nothing ever removed a crashed parent's file, one per
/// crash for ever). A file is its parent's while the parent's `flock` on it
/// holds ([`EdgeFile`]), so the owner test is the crash markers' own
/// (`crash_signal::markers::probe`): a file whose lock this sweep can take is
/// a dead parent's — its tokens authorize nothing again, the child's `(sid,
/// nonce)` never being reissued — and is removed. A file still EMPTY is left
/// alone: its writer may be between the create and the lock. A file the
/// probe cannot read either way is left too. A file of a build before the
/// lock (no lock ever held) reads as a dead owner's; a seamless update does
/// not carry the child proxies it would authorize against, so it is inert
/// too.
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
/// Same-uid + 0600 is the access gate (the path is non-secret; the file is not).
///
/// REPEATABLE (F1, revised): this read is non-destructive and may run any number
/// of times for the parent session's lifetime — a child re-launched in the SAME
/// shell re-reads the same file to re-install the parent edges. The parent owns the
/// file's removal ([`ChildProxy`]); the reader never deletes it.
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

/// What this aterm holds for ONE child it provisioned: the proxy-table key its
/// [`ProxyEntry`] was registered under and the 0600 edge-token file it wrote
/// ([`write_edge_tokens`]), when it could write one.
///
/// It RETIRES both when it drops. The session that spawned the child holds it, so
/// a closed tab deregisters its child and a long-lived aterm opening and closing
/// tabs does not grow [`proxies`] for the process lifetime (audit finding S1,
/// `aterm_spec::derive::proxy_registry_model`); and a spawn that fails after
/// provisioning drops it on the way out, so the same retirement runs with no
/// second copy of the teardown to keep in step. The file removed is the exact
/// path written — never one re-derived from a socket dir that may resolve
/// differently by teardown time.
///
/// There is deliberately NO liveness-based sweep of the `edges/` dir: a
/// freshly-provisioned child has no discovery entry UNTIL it launches (often much
/// later, or never), so "no live graph entry" cannot distinguish a still-needed
/// fresh file from an orphan — a sweep on that signal would clobber the very file
/// a not-yet-launched (or about-to-relaunch) child must read. A file orphaned by a
/// CRASHED parent is cryptographically inert: its tokens authorize only against
/// the dead child's exact `(sid, nonce)`, both random and never reissued, so a
/// leftover can never authorize anything again.
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

/// The HOSTING pid a session's discovery entry records (`pid <n>`), or `None`
/// when there is no entry, it is unreadable, or it predates the `pid` line.
///
/// The one fact [`crate::identity_claim`] needs to answer "is some OTHER live
/// instance already serving this id?" without dialing a same-uid-writable path.
/// A pid alone grants nothing and reveals nothing, which is why this reads the
/// line rather than the `sock` one.
pub(crate) fn graph_entry_host_pid(sock_dir: &Path, sid: &SessionId) -> Option<u32> {
    let body = std::fs::read_to_string(graph_path(sock_dir, sid)).ok()?;
    aterm_types::control_socket::graph_entry_pid(&body)
}

/// Read a child's discovery entry: `(sock_path, nonce)` or `None` if absent /
/// malformed. PURE parse split out for testing.
pub(crate) fn read_graph_entry(sock_dir: &Path, sid: &SessionId) -> Option<(String, LaunchNonce)> {
    let body = std::fs::read_to_string(graph_path(sock_dir, sid)).ok()?;
    parse_graph_entry(&body)
}

/// Parse a graph-entry body (`sock <path>\nnonce <hex>\n`). The `sock` line is
/// the SHARED on-disk format (`aterm_types::control_socket::graph_entry_sock`,
/// also read by `aterm-ctl`'s self-location); the nonce line is server-only.
fn parse_graph_entry(body: &str) -> Option<(String, LaunchNonce)> {
    let sock = aterm_types::control_socket::graph_entry_sock(body)?;
    let nonce = body
        .lines()
        .find_map(|l| l.strip_prefix("nonce "))
        .and_then(|h| LaunchNonce::from_hex(h.trim()))?;
    Some((sock, nonce))
}

/// Build the first line to present on the child socket: `TOKEN <edge-hex> <verb>`,
/// where `verb` is the caller's already-rewritten verb line (the direct child's
/// own selector inlined to `@.`). The shipped path forwards DIRECT children only
/// (one hop) — the child is never in its own proxy table, so no cycle can form.
#[must_use]
pub(crate) fn forward_first_line(edge_hex: &str, verb: &str) -> String {
    format!("TOKEN {edge_hex} {verb}\n")
}

/// The inverse of [`forward_first_line`]: `(edge_hex, verb)`, or `None` for a
/// line that is not a `TOKEN <hex> <verb>` handshake.
fn split_forward_line(first_line: &str) -> Option<(&str, &str)> {
    let line = first_line.strip_suffix('\n').unwrap_or(first_line);
    let (token, verb) = line.strip_prefix("TOKEN ")?.split_once(' ')?;
    (!token.is_empty() && !verb.is_empty()).then_some((token, verb))
}

/// How long a forward waits for the child to prove it is SERVING the
/// connection (its answer to the connect probe) before the caller is told the
/// child accepted nothing. The same bound `aterm ctl` gives its own connect
/// phase: a child whose listener is wedged — connections queued, never
/// accepted — must not hold the caller (and this lane) for a whole verb
/// deadline.
pub(crate) const FORWARD_ACCEPT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Connect to `child_sock`, present `first_line`, and RELAY bytes transparently in
/// both directions until either side closes. Format-agnostic: carries any verb's
/// framing (status lines, `OK <n>` bodies, subscribe push frames, binary). The
/// `client_prebuffered` bytes (anything the server's `BufReader` already read past
/// the request line) are forwarded to the child FIRST so nothing is lost.
///
/// Returns `Ok(())` on a clean close, or an `io::Error` if the dial / handshake
/// failed before any relay (so the caller can answer `ERR`) — including a child
/// that did not answer the connect probe within [`FORWARD_ACCEPT_DEADLINE`]
/// (`ErrorKind::TimedOut`).
pub(crate) fn connect_and_relay(
    child_sock: &str,
    first_line: &str,
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

/// Whether a connect-probe answer refuses the CONNECTION rather than answering
/// the probe: the token was refused (`ERR auth`, after which the server
/// closes), or no lane could take it (`ERR control server busy; retry`).
pub(crate) fn is_connection_refusal(line: &str) -> bool {
    line == "ERR auth" || line.starts_with("ERR control server busy")
}

/// THE CONNECT PROBE of a forward: authenticate with `token` and ask `version`
/// — answered by every aterm generation, for any scope, with one line and no
/// main-thread hop — then read that one line within `within`. Only a child that
/// answers is sent the caller's verb, so a child whose listener queues
/// connections without accepting them costs the caller seconds, not the verb's
/// deadline, and receives nothing. The reply is consumed here, never relayed.
///
/// The verb then travels as the connection's next request line, which the
/// child serves exactly as it would have served it folded into the handshake.
fn prove_child_serving(
    child: &CtlStream,
    token: &str,
    within: std::time::Duration,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let mut hello = String::with_capacity(token.len() + 16);
    hello.push_str("TOKEN ");
    hello.push_str(token);
    hello.push_str(" version\n");
    (&*child).write_all(hello.as_bytes())?;
    (&*child).flush()?;
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
    let line = line.trim_end_matches('\r');
    // A refusal ends the connection: the verb is never sent.
    if is_connection_refusal(line) {
        return Err(Error::other(format!("the child answered {line:?}")));
    }
    child.set_read_timeout(None)
}

/// [`connect_and_relay`] with its relay stage as a parameter: the REPLY-FIDELITY
/// contract (`reply_fidelity_model`) lives entirely in this function, so it is
/// driven by the Tier-1 bind with the real relay and with a relay that fails
/// after delivery (`reply_fidelity_conformance`) — a `try_clone` under fd
/// exhaustion cannot be staged in a shared test process.
fn deliver_then_relay(
    child_sock: &str,
    first_line: &str,
    client: &CtlStream,
    client_prebuffered: &[u8],
    accept_within: std::time::Duration,
    relay: impl FnOnce(&CtlStream, &CtlStream) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let child = CtlStream::connect(child_sock)?;
    // Prove the child is serving before it is handed the verb, then send the
    // rewritten verb as the next request line. (A line that is not a TOKEN
    // handshake is presented as it is.)
    match split_forward_line(first_line) {
        Some((token, verb)) => {
            prove_child_serving(&child, token, accept_within)?;
            (&child).write_all(verb.as_bytes())?;
            (&child).write_all(b"\n")?;
        }
        None => (&child).write_all(first_line.as_bytes())?,
    }
    if !client_prebuffered.is_empty() {
        (&child).write_all(client_prebuffered)?;
    }
    (&child).flush()?;
    // Past this point the verb HAS been delivered to the child. A relay-stage
    // failure (e.g. a `try_clone` under fd exhaustion) tears the connection down
    // but must NOT be reported as "forward failed" — that would be a false
    // negative for an op that already reached the child. Only a connect/handshake
    // error (the `?`s above, before any byte was delivered) surfaces as `Err` so
    // the caller can honestly answer `ERR forward`.
    let _ = relay(client, &child);
    Ok(())
}

/// Pump bytes both ways between two connected streams, preserving graceful
/// half-close order. EOF after a child response becomes `Shutdown::Write` on
/// the original client only after every preceding byte was copied + flushed;
/// client EOF then becomes `Shutdown::Write` on the child. This matters for
/// guarded artifact replies: the original client's explicit post-response ACK
/// must travel client → child after the complete child → client response, rather
/// than a first EOF tearing down the opposite direction.
///
/// A real copy error still shuts both sockets down to unblock the paired pump.
fn relay_bidirectional(client: &CtlStream, child: &CtlStream) -> std::io::Result<()> {
    let mut c2s_r = client.try_clone()?;
    let mut c2s_w = child.try_clone()?;
    let mut s2c_r = child.try_clone()?;
    let mut s2c_w = client.try_clone()?;
    let w_client = client.try_clone()?;
    let w_child = child.try_clone()?;
    // child -> client on a worker; client -> child here. The worker says when
    // it is done (its sender drops even if it unwinds), so the hangup watch
    // below costs no latency on a relay that ends normally.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        match copy_until_eof(&mut s2c_r, &mut s2c_w) {
            Ok(()) => {
                let _ = w_client.shutdown(std::net::Shutdown::Write);
            }
            Err(_) => {
                let _ = w_client.shutdown(std::net::Shutdown::Both);
                let _ = w_child.shutdown(std::net::Shutdown::Both);
            }
        }
        let _ = done_tx.send(());
    });
    match copy_until_eof(&mut c2s_r, &mut c2s_w) {
        // The client's EOF: a half-close still wants the answer, but a client
        // that is GONE (closed both halves) has nobody to answer — tear both
        // sides down so the child's blocking verb and this lane end now.
        Ok(()) if !aterm_uds::hangup::peer_closed(client) => {
            let _ = child.shutdown(std::net::Shutdown::Write);
        }
        Ok(()) | Err(_) => {
            let _ = client.shutdown(std::net::Shutdown::Both);
            let _ = child.shutdown(std::net::Shutdown::Both);
        }
    }
    // A client that half-closed and THEN died is seen by nobody above (this
    // thread no longer reads it, the worker is parked on the child): watch for
    // that hangup while the answer is still coming, so a dead client's lane is
    // given back within a poll instead of when the child's verb times out.
    while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
        done_rx.recv_timeout(crate::control::HANGUP_POLL)
    {
        if aterm_uds::hangup::peer_closed(client) {
            let _ = client.shutdown(std::net::Shutdown::Both);
            let _ = child.shutdown(std::net::Shutdown::Both);
            break;
        }
    }
    let _ = worker.join();
    let _ = client.shutdown(std::net::Shutdown::Both);
    let _ = child.shutdown(std::net::Shutdown::Both);
    Ok(())
}

/// Copy `reader` → `writer` in 32 KiB chunks until EOF or error.
fn copy_until_eof<R: Read, W: Write>(reader: &mut R, writer: &mut W) -> std::io::Result<()> {
    let mut buf = [0u8; 32 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        writer.write_all(&buf[..n])?;
        writer.flush()?;
    }
}

/// Drain whatever a server-side `BufReader` already buffered past the request line,
/// so [`connect_and_relay`] can forward it to the child before the raw relay. A
/// freshly-handshaked connection usually has none.
///
/// Uses [`BufReader::buffer`] — the bytes ALREADY in the internal buffer — and
/// NEVER `fill_buf()`: `fill_buf` performs a blocking read when the buffer is
/// empty, which is the COMMON case for a one-line forward request (the client
/// sent `TOKEN <hex> @<sid> <verb>\n` and is now blocked awaiting the reply). A
/// `fill_buf` there would deadlock — the client never sends more, so the drain
/// would park forever before the relay even starts. `buffer()` returns the
/// pipelined leftovers when present and an empty slice otherwise, no syscall.
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
    use std::io::BufReader;

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
    /// session entry agree on how ONE socket is spelled.
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

    /// FINDING #2 (custom-socket discovery split-brain): an instance bound to an
    /// EXPLICIT `--control-sock` — whose socket lives OUTSIDE the default
    /// rendezvous dir — mirrors its session graph entry INTO that default dir (the
    /// only dir the flagless `aterm-ctl` client reads). The client must recover the
    /// ABSOLUTE explicit socket path (wherever it lives) + nonce from that entry, so
    /// a flagless in-session call reaches the instance hosting its terminal even
    /// though the socket is not in the default dir. Simulated here by the entry the
    /// server writes into the default dir; parsed with the SAME shared helper
    /// (`control_socket::graph_entry_sock`) the client's self-location uses.
    #[test]
    fn explicit_socket_instance_resolvable_from_default_dir_entry() {
        // `default_dir` stands in for `aterm_uds::control_socket_dir()`.
        let default_dir =
            std::env::temp_dir().join(format!("aterm-defaultdir-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&default_dir);
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        // The instance's ACTUAL socket lives in a wholly separate, non-default dir.
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

    /// The mirror targets the default rendezvous dir ONLY for an instance whose own
    /// socket dir is NOT the default (an explicit-socket instance); a default
    /// per-instance instance already lives there and writes ONCE (no duplicate).
    /// Reads the environment-derived default dir but never MUTATES the environment.
    /// Holds [`self_sock_test_guard`] so a concurrent publish test's transient
    /// `MIRROR_DIR_OVERRIDE` can never shadow the real default dir it asserts on.
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

    #[test]
    fn forward_first_line_presents_token_and_verb() {
        assert_eq!(
            forward_first_line("abcd", "@. screen"),
            "TOKEN abcd @. screen\n"
        );
        assert_eq!(
            split_forward_line(&forward_first_line("abcd", "@. screen")),
            Some(("abcd", "@. screen"))
        );
        assert_eq!(split_forward_line("AUTH abcd\n"), None);
        assert_eq!(split_forward_line("TOKEN abcd\n"), None);
    }

    /// THE WEDGED CHILD. A child whose listener queues connections without
    /// accepting them (the 2026-09-25 incident's state) is reported within the
    /// connect-probe bound as having accepted nothing — an error BEFORE any
    /// relay, so the caller answers it — and the verb never reaches it. The
    /// served child in the test above is the negative control.
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
            &forward_first_line("tok-hex", "@. text"),
            &client_relay,
            &[],
            std::time::Duration::from_millis(400),
            |_, _| {
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
            started.elapsed() < std::time::Duration::from_secs(3),
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
            &forward_first_line("stale", "@. key enter"),
            &client_relay,
            &[],
            std::time::Duration::from_secs(5),
            |_, _| Ok(()),
        );
        assert!(result.is_err(), "a refused probe is a failed forward");
        assert_eq!(child.join().unwrap(), "", "the verb was never sent");
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
        let first_line = forward_first_line("tok-hex", "screen");
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

        // Close BOTH client handles (the raw stream AND `app_rdr`'s cloned fd) so
        // the relay's client→child reader hits EOF and the relay thread returns.
        let _ = client_app.shutdown(std::net::Shutdown::Both);
        drop(app_rdr);
        drop(client_app);
        let _ = relay.join();
        let _ = child.join();
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
        original
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();

        let relay =
            std::thread::spawn(move || relay_bidirectional(&relay_client, &relay_child).unwrap());
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
        service.join().unwrap();
        assert!(
            !alive.load(std::sync::atomic::Ordering::Acquire),
            "the original client's ACK crosses the proxy before guard release"
        );
        let _ = original.shutdown(std::net::Shutdown::Both);
        drop(reader);
        drop(original);
        relay.join().unwrap();
    }

    /// Which of `relay_bidirectional`'s two error arms a teardown case drives.
    #[cfg(unix)]
    #[derive(Clone, Copy, Debug)]
    enum RelayFault {
        /// The child-bound socket loses its send half: the MAIN thread's
        /// client-to-child write fails the moment the original client sends a
        /// request, while the worker is parked reading a child that never speaks.
        ChildBound,
        /// The client-bound socket loses its send half: the WORKER's
        /// child-to-client write fails the moment the child answers, while the
        /// main thread is parked reading an original client that never speaks.
        ClientBound,
    }

    /// Tear the real relay down under `fault` while both peers stay open and
    /// otherwise silent, and project what it left onto `RelayTeardown`'s
    /// variables. `done` is teardown observed by the peer whose bytes the
    /// faulted arm was relaying — its read sees EOF once the relay shuts that
    /// socket (the other peer reads EOF from the injected fault alone, so it
    /// could not tell) — and each read half is observed on a clone of that
    /// local socket taken beforehand (a non-blocking read answers EOF when shut,
    /// `WouldBlock` when a pump could still park on it). The second value is
    /// whether `relay_bidirectional` returned: its worker joined.
    #[cfg(unix)]
    fn relay_teardown_under(
        fault: RelayFault,
    ) -> (std::collections::BTreeMap<&'static str, i64>, bool) {
        use std::time::Duration;
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
            let result = relay_bidirectional(&relay_client, &relay_child);
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
        speaker
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("deadline");
        let mut byte = [0u8; 1];
        let done = matches!(speaker.read(&mut byte), Ok(0));
        let returned = returned_rx.recv_timeout(Duration::from_secs(3)).is_ok();
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
        // Release anything still parked before judging, so a failure cannot hang.
        drop(child_peer);
        drop(original);
        let _ = relay.join();
        (observed, returned)
    }

    /// CONFORMANCE (Tier-1) of the REAL relay to
    /// `aterm_spec::derive::relay_teardown_model` (`RelayTeardown`, audit M2):
    /// when the cross-process relay tears down, BOTH read halves of BOTH of its
    /// local sockets are shut, so a pump parked on a CLONE of either one gets EOF
    /// and the worker thread joins — no parked thread, no held fd.
    ///
    /// `relay_bidirectional` has two error arms, one per pump, and each is
    /// driven by the fault that exercises it while both peers stay open and
    /// silent ([`RelayFault`]): a failed write toward the child (the main
    /// thread's arm) and a failed write toward the original client (the
    /// worker's arm). For each, the projection ([`relay_teardown_under`]) must
    /// be the model's `Teardown` step, and the relay must have returned.
    ///
    /// NEGATIVE CONTROL: `Buggy = 1` (the original `shutdown(Write)`-only) must
    /// reject every real step. Turn EITHER arm's `shutdown(Both)` calls into
    /// `shutdown(Write)` and its case leaves both read halves open with a pump
    /// still parked — the step only the mutant admits.
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
        // The CORE of this bug fix: two reads of the same file BOTH succeed (the
        // same-shell relaunch re-reads it; consume-on-read previously broke this).
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

    /// A DEAD PARENT'S EDGE FILE IS SWEPT, A LIVE ONE'S NEVER (the audit of
    /// 2026-09-25: `edges/` was never swept, one file per crashed parent for
    /// ever — twelve on the owner's Mac since August). A parent holds its
    /// file's owner lock for the child's life; at the next provisioning a file
    /// whose lock is free (its parent gone — here, its handle dropped without
    /// the removal a clean retire does, as a crash leaves it) is removed.
    /// NEGATIVE CONTROLS: the file of a live parent (its `EdgeFile` held) and
    /// an EMPTY file (a writer between its create and its lock) stay.
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
        // The lock released, the file left: a crashed parent. Released by
        // LOCK_UN before the close, because this process stands in for a parent
        // that is GONE: a lock released by the close alone lives on in any
        // child another test is forking at that instant, until it execs, and
        // the sweep below would read the parent as live (the fd-copy sweep of
        // 2026-09-27; `control_auth`'s instance lease test states the same).
        crashed
            ._lock
            .unlock()
            .expect("release the crashed parent's lock");
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
        assert!(!crashed_path.exists(), "the dead parent's file is swept");
        assert!(live.path().exists(), "a live parent's file is never swept");
        assert!(empty.exists(), "an empty file may be mid-write: left");
        assert!(next.path().exists());
        drop((live, next));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SIBLING DISCOVERY lifecycle: before the control socket binds,
    /// `publish_session` is a safe no-op; after `set_self_sock`, it writes the
    /// session's graph entry pointing at OUR instance socket, and
    /// `unpublish_session` retires it on close. `read_sibling_token` pairs the
    /// per-instance socket with its 0600 token file and fails closed on
    /// absent/empty tokens.
    #[test]
    fn publish_session_lifecycle_follows_bind_and_close() {
        let _guard = self_sock_test_guard();
        let dir = std::env::temp_dir().join(format!("aterm-pub-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // Redirect the rendezvous MIRROR into a scratch dir so publishing never
        // creates/tightens-perms-on the user's REAL control dir. `dir` (our own
        // socket dir) is NOT the default dir, so `publish_session` would otherwise
        // mirror the entry into `aterm_uds::control_socket_dir()` — the real
        // ~/Library/Application Support/aterm/graph. The override (held under the
        // self-sock guard, cleared before it drops) confines that mirror here.
        let mirror = std::env::temp_dir().join(format!("aterm-pub-mirror-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&mirror);
        set_mirror_dir_override(Some(mirror.clone()));
        // The real default dir must be UNTOUCHED by this test — capture it so we
        // can assert our (randomly-generated) sid never lands there.
        let real_default = aterm_uds::control_socket_dir();
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();

        // Pre-bind: publishing is a no-op (no panic, no entry).
        clear_self_sock();
        publish_session(&sid, &nonce);
        assert!(read_graph_entry(&dir, &sid).is_none(), "no entry pre-bind");

        // Post-bind: the entry appears, pointing at OUR socket, with the nonce.
        // `set_self_sock` stores the CANONICAL path (the self-dial guard compares
        // against `confine_proxy_sock`'s canonical output), so the published
        // entry carries the canonical form even when the raw path was recorded
        // through a symlinked ancestor (macOS temp: /var → /private/var).
        let sock = dir.join("aterm-88001.sock").to_string_lossy().into_owned();
        // …in the PUBLISHED spelling: Windows' canonical form is the verbatim
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

        // The mirror landed in the SCRATCH dir, NOT the real default dir.
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

        // Close: the entry is retired from BOTH our dir and the (scratch) mirror.
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

    /// REGRESSION: `drain_buffered` must NOT block when the BufReader has no
    /// buffered bytes and the peer is silent — the common one-line-forward case.
    /// (The bug was `fill_buf()`, which blocks on an empty buffer and hung every
    /// forward before the relay started.) If it regresses, this test HANGS — the
    /// correct, loud failure mode under a test timeout.
    #[test]
    fn drain_buffered_never_blocks_on_empty_buffer() {
        let (a, _b) = CtlStream::pair().expect("pair"); // _b stays open + silent
        let mut r = std::io::BufReader::new(a);
        assert!(
            drain_buffered(&mut r).is_empty(),
            "empty buffer drains to nothing, no block"
        );
    }

    /// `drain_buffered` returns exactly the bytes PIPELINED past the request line
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
}
