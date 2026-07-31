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
//! ## Scope: one hop
//!
//! The shipped path forwards DIRECT children only — the child's own selector is
//! inlined to `@.` so it runs the verb on itself, and a child is never in its own
//! proxy table, so no cycle can form. Transitive `@<grandchild>` forwarding (which
//! would need a `via=<n>` hop guard) is NOT implemented; a grandchild selector
//! simply does not resolve here and falls through to a local `ERR no such session`.
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
pub struct ProxyEntry {
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
    pub fn token_for(&self, op: Op) -> Option<&EdgeToken> {
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
pub type ProxyTable = Arc<RwLock<HashMap<SessionId, ProxyEntry>>>;

/// A fresh, empty proxy table.
#[must_use]
pub fn new_proxy_table() -> ProxyTable {
    Arc::new(RwLock::new(HashMap::new()))
}

/// The process-wide proxy table: ONE per aterm process (the spawn path inserts a
/// child's capability; the control server reads it to forward). A singleton avoids
/// threading the handle through every `spawn_session`/`serve` caller; correctness-
/// wise a process has exactly one recursion fabric.
static PROXIES: std::sync::OnceLock<ProxyTable> = std::sync::OnceLock::new();

/// The process-wide [`ProxyTable`] (lazily initialized, cloned Arc).
#[must_use]
pub fn proxies() -> ProxyTable {
    PROXIES.get_or_init(new_proxy_table).clone()
}

/// Record the capability this aterm holds over a child it just spawned.
pub fn register_child(child: SessionId, entry: ProxyEntry) {
    proxies()
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .insert(child, entry);
}

/// Look up the capability for a child by session id (cloned out).
#[must_use]
pub fn lookup_child(sid: &SessionId) -> Option<ProxyEntry> {
    proxies()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(sid)
        .cloned()
}

/// Drop the capability for a child (its session closed) so the process-wide table
/// does not grow for the process lifetime as tabs open and close.
pub fn deregister_child(child: &SessionId) {
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
pub fn set_self_sock(sock_dir: &Path, sock_path: &str) {
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
pub fn self_sock_path() -> Option<String> {
    SELF_SOCK
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|(_, p)| p.clone())
}

/// Test-only: clear the recorded self socket so tests that set it cannot leak
/// state into other tests in the same process.
#[cfg(test)]
pub fn clear_self_sock() {
    *SELF_SOCK.write().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Test-only: serialize every test that touches the process-global recorded
/// self socket (tests run on parallel threads; two of them mutating
/// [`SELF_SOCK`] concurrently would flake). Acquire this FIRST, hold it for the
/// test's whole self-sock window, and `clear_self_sock` before dropping it.
#[cfg(test)]
pub fn self_sock_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Publish the discovery graph entry for a session THIS instance hosts, so a
/// SIBLING instance (and the flagless `aterm-ctl` client) can resolve `@<sid>` to
/// our socket. No-op until the control socket is bound ([`set_self_sock`]);
/// best-effort. Uses [`publish_graph_entry`], so an instance on an explicit
/// `$ATERM_CONTROL_SOCK` ALSO lands its entry in the default rendezvous dir the
/// client reads. Called at the session-registration seam for every session.
pub fn publish_session(sid: &SessionId, nonce: &LaunchNonce) {
    let guard = SELF_SOCK.read().unwrap_or_else(|p| p.into_inner());
    if let Some((dir, sock)) = guard.as_ref() {
        publish_graph_entry(dir, sid, sock, nonce);
    }
}

/// Remove a closed session's discovery entry (best-effort) so siblings stop
/// resolving it. No-op when no socket is bound. A leftover (crash, missed
/// close) is harmless: the sibling forward re-checks socket liveness and the
/// hosting instance re-checks its own store, so a stale entry can only produce
/// `ERR no such session`, never a wrong target.
pub fn unpublish_session(sid: &SessionId) {
    let guard = SELF_SOCK.read().unwrap_or_else(|p| p.into_inner());
    if let Some((dir, _)) = guard.as_ref() {
        retire_graph_entry(dir, sid);
    }
}

/// Read the per-launch AUTH token of the SIBLING instance whose socket is
/// `sock_path` (`<dir>/aterm-<pid>.sock` → `<dir>/aterm-<pid>.token`, 0600,
/// same-uid). This is the exact credential a same-uid `aterm-ctl --pid <pid>`
/// client reads for itself, so presenting it on a forward grants nothing the
/// caller could not already obtain directly. `None` (fail closed) for an
/// unreadable/empty token or a path with no filename.
#[must_use]
pub fn read_sibling_token(sock_path: &str) -> Option<String> {
    let p = Path::new(sock_path);
    let dir = p.parent()?;
    let name = p.file_name()?.to_string_lossy();
    let token_file = aterm_types::control_socket::token_name_for_sock(&name);
    let raw = std::fs::read_to_string(dir.join(token_file)).ok()?;
    let t = raw.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// Write the discovery graph entry an inner aterm publishes at bind time so its
/// parent can reach it: `<sock_dir>/graph/<self-sid>` (0600) with three lines
/// `sock <abs-path>\nnonce <hex>\npid <n>\n`. The `pid` is THIS (the hosting)
/// process's — recorded so the flagless client's `instances`/`ls` can report a
/// pid even for an explicit-socket instance whose socket filename encodes none.
/// Edge tokens are NEVER written here — they travel only via the injected env.
/// Best-effort: a write failure just means the parent cannot reach us by proxy
/// (direct per-instance reach still works).
///
/// This writes into ONE dir only; [`publish_graph_entry`] wraps it to ALSO
/// mirror into the well-known default rendezvous dir when the instance runs on
/// an explicit `$ATERM_CONTROL_SOCK` outside it.
pub fn write_graph_entry(sock_dir: &Path, sid: &SessionId, sock_path: &str, nonce: &LaunchNonce) {
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
pub fn set_mirror_dir_override(dir: Option<PathBuf>) {
    *MIRROR_DIR_OVERRIDE
        .write()
        .unwrap_or_else(|p| p.into_inner()) = dir;
}

/// The well-known DEFAULT control dir (`aterm_uds::control_socket_dir`) to ALSO
/// publish a graph entry into, or `None` when `sock_dir` already IS it (the
/// default per-instance case — no mirror needed) or the per-user base cannot be
/// resolved. The flagless `aterm-ctl` client only ever reads the default dir, so
/// an instance launched on an explicit `$ATERM_CONTROL_SOCK` (whose entries would
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
/// on an explicit `$ATERM_CONTROL_SOCK`) — into the default dir too, so the
/// flagless `aterm-ctl` client (which only ever reads the default dir) can still
/// self-locate and enumerate this instance. The entry carries the ABSOLUTE `sock
/// <path>`, so the client resolves the right socket wherever it actually lives.
/// Best-effort + idempotent: a default per-instance instance writes ONCE (the
/// mirror dir collapses to the same dir and is skipped), and a default dir that
/// is unavailable/unwritable simply degrades to the own-dir entry (no crash — a
/// headless explicit-socket instance still runs, just undiscoverable by flagless
/// clients until the default dir is writable).
pub fn publish_graph_entry(sock_dir: &Path, sid: &SessionId, sock_path: &str, nonce: &LaunchNonce) {
    write_graph_entry(sock_dir, sid, sock_path, nonce);
    if let Some(rv) = rendezvous_mirror_dir(sock_dir) {
        write_graph_entry(&rv, sid, sock_path, nonce);
    }
}

/// Retire a session's discovery entry from BOTH its own dir and the mirrored
/// default rendezvous dir (the inverse of [`publish_graph_entry`]). Best-effort;
/// a leftover is harmless (the nonce guard fails a stale dial closed and the
/// client re-probes liveness), so this is hygiene, not correctness.
pub fn retire_graph_entry(sock_dir: &Path, sid: &SessionId) {
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
pub fn remove_graph_entry(sock_dir: &Path, sid: &SessionId) {
    let _ = std::fs::remove_file(graph_path(sock_dir, sid));
}

/// Sweep dead discovery entries: remove any `graph/<sid>` whose recorded socket no
/// longer has a live listener — a crashed session that never ran its graceful
/// `remove_graph_entry`. Mirrors `control_auth::sweep_stale_instances` for the
/// sibling per-instance files; best-effort (the nonce guard already fails a stale
/// dial closed, so this only keeps the dir bounded). Called at spawn.
pub fn sweep_stale_graph(sock_dir: &Path) {
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
/// owns the file and removes it on session/child teardown ([`remove_edge_tokens`]);
/// crash leftovers are swept at the next spawn ([`sweep_stale_edges`]). Inheritance
/// across a NEW aterm hop is still blocked — `ATERM_EDGE_TOKENS` stays deny-listed,
/// so only a same-shell relaunch (which re-inherits the pinned path) re-reads it.
pub fn write_edge_tokens(
    sock_dir: &Path,
    child_sid: &SessionId,
    read_hex: &str,
    write_hex: &str,
    signal_hex: &str,
) -> Option<String> {
    let dir = sock_dir.join("edges");
    if crate::control_auth::ensure_private_dir(&dir).is_err() {
        return None;
    }
    let path = dir.join(child_sid.as_str());
    let body = format!("read {read_hex}\nwrite {write_hex}\nsignal {signal_hex}\n");
    let mut f = open_private(&path).ok()?;
    f.write_all(body.as_bytes()).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// Read the three edge-token hexes `(read, write, signal)` from the 0600 file at
/// `path` (written by [`write_edge_tokens`]), or `None` if absent / malformed.
/// Same-uid + 0600 is the access gate (the path is non-secret; the file is not).
///
/// REPEATABLE (F1, revised): this read is non-destructive and may run any number
/// of times for the parent session's lifetime — a child re-launched in the SAME
/// shell re-reads the same file to re-install the parent edges. The parent owns the
/// file's removal ([`remove_edge_tokens`] on teardown, [`sweep_stale_edges`] for
/// crash leftovers); the reader never deletes it.
pub fn read_edge_tokens(path: &str) -> Option<(String, String, String)> {
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

/// The edge-token filename for a child session id, under `<sock_dir>/edges/`.
fn edge_path(sock_dir: &Path, child_sid: &SessionId) -> std::path::PathBuf {
    sock_dir.join("edges").join(child_sid.as_str())
}

/// Remove the parent→child edge-token file the PARENT wrote ([`write_edge_tokens`])
/// once the spawned child session is torn down — the parent owns the file (it lives
/// in the parent's own 0700 socket dir) and is responsible for its removal, since
/// the file now PERSISTS for the session rather than being consumed on the child's
/// first read (so a same-shell child relaunch can re-read it). Best-effort, run on
/// graceful session/child teardown ([`crate::main`]'s `Session::drop`).
///
/// There is deliberately NO liveness-based sweep: a freshly-provisioned child has no
/// discovery entry UNTIL it launches (often much later, or never), so "no live graph
/// entry" cannot distinguish a still-needed fresh file from an orphan — a sweep on
/// that signal would clobber the very file a not-yet-launched (or about-to-relaunch)
/// child must read. A file orphaned by a CRASHED parent is cryptographically inert:
/// its tokens authorize only against the dead child's exact `(sid, nonce)`, both
/// random and never reissued, so a leftover can never authorize anything again.
pub fn remove_edge_tokens(sock_dir: &Path, child_sid: &SessionId) {
    let _ = std::fs::remove_file(edge_path(sock_dir, child_sid));
}

/// Read a child's discovery entry: `(sock_path, nonce)` or `None` if absent /
/// malformed. PURE parse split out for testing.
pub fn read_graph_entry(sock_dir: &Path, sid: &SessionId) -> Option<(String, LaunchNonce)> {
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
pub fn forward_first_line(edge_hex: &str, verb: &str) -> String {
    format!("TOKEN {edge_hex} {verb}\n")
}

/// Connect to `child_sock`, present `first_line`, and RELAY bytes transparently in
/// both directions until either side closes. Format-agnostic: carries any verb's
/// framing (status lines, `OK <n>` bodies, subscribe push frames, binary). The
/// `client_prebuffered` bytes (anything the server's `BufReader` already read past
/// the request line) are forwarded to the child FIRST so nothing is lost.
///
/// Returns `Ok(())` on a clean close, or an `io::Error` if the dial / handshake
/// failed before any relay (so the caller can answer `ERR`).
pub fn connect_and_relay(
    child_sock: &str,
    first_line: &str,
    client: &CtlStream,
    client_prebuffered: &[u8],
) -> std::io::Result<()> {
    let child = CtlStream::connect(child_sock)?;
    // Present the handshake + folded, rewritten verb.
    (&child).write_all(first_line.as_bytes())?;
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
    let _ = relay_bidirectional(client, &child);
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
    // child -> client on a worker; client -> child here.
    let worker = std::thread::spawn(move || match copy_until_eof(&mut s2c_r, &mut s2c_w) {
        Ok(()) => {
            let _ = w_client.shutdown(std::net::Shutdown::Write);
        }
        Err(_) => {
            let _ = w_client.shutdown(std::net::Shutdown::Both);
            let _ = w_child.shutdown(std::net::Shutdown::Both);
        }
    });
    match copy_until_eof(&mut c2s_r, &mut c2s_w) {
        Ok(()) => {
            let _ = child.shutdown(std::net::Shutdown::Write);
        }
        Err(_) => {
            let _ = client.shutdown(std::net::Shutdown::Both);
            let _ = child.shutdown(std::net::Shutdown::Both);
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
pub fn drain_buffered<R: Read>(reader: &mut std::io::BufReader<R>) -> Vec<u8> {
    let buffered = reader.buffer().to_vec();
    let n = buffered.len();
    reader.consume(n);
    buffered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

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
    /// EXPLICIT `$ATERM_CONTROL_SOCK` — whose socket lives OUTSIDE the default
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

        // The fake child: read the TOKEN line, then reply with a framed body that
        // includes a raw 0xff byte, then echo one more line, then close.
        let child = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut rdr = BufReader::new(conn.try_clone().unwrap());
            let mut first = String::new();
            rdr.read_line(&mut first).unwrap();
            assert!(
                first.starts_with("TOKEN tok-hex screen"),
                "handshake: {first:?}"
            );
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

    /// F1 (revised): edge-token secrets round-trip through the 0600 file, the file
    /// is owner-only (0600), and the read is REPEATABLE — it PERSISTS for the
    /// session so a child re-launched in the same shell can re-read it. The PARENT
    /// removes it on teardown via `remove_edge_tokens` (keyed by child sid).
    #[test]
    fn edge_tokens_file_is_0600_and_read_is_repeatable() {
        let dir = std::env::temp_dir().join(format!("aterm-edges-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sid = SessionId::generate();
        let (r, w, s) = ("aa".repeat(32), "bb".repeat(32), "cc".repeat(32));
        let path = write_edge_tokens(&dir, &sid, &r, &w, &s).expect("write");
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
        // The parent owns removal, keyed by child sid; after it the file is gone.
        remove_edge_tokens(&dir, &sid);
        assert!(
            read_edge_tokens(&path).is_none(),
            "removed by owning parent"
        );
        // A different child's sid is a no-op (removes only its own file).
        remove_edge_tokens(&dir, &SessionId::generate());
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
        let canon_sock = std::fs::canonicalize(&dir)
            .unwrap()
            .join("aterm-88001.sock")
            .to_string_lossy()
            .into_owned();
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
