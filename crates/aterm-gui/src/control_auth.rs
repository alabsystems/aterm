// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Default-on access control for the introspection CONTROL SOCKET.
//!
//! The control socket grants FULL power over the live terminal (drive the
//! shell, deliver signals, snapshot pixels). Historically it bound a
//! world-writable `/tmp/aterm.sock` with no authentication, so ANY local
//! process — or a different local user — could drive the terminal. This module
//! closes that hole with three layers, all default-on and all transparent to a
//! same-user client:
//!
//! 1. **Per-user private directory.** The socket lives in a `0700` directory
//!    only the owning user can traverse (`$XDG_RUNTIME_DIR`, else
//!    `~/Library/Application Support/aterm`), and the socket file itself is
//!    `chmod 0600` after bind. A different user cannot even reach the socket.
//! 2. **Peer credential check.** After `accept(2)` the server reads the
//!    connecting peer's uid via `getpeereid(2)` and refuses any peer whose uid
//!    is not our own `geteuid()`. Defence in depth in case the directory perms
//!    are ever loosened (shared `$XDG_RUNTIME_DIR`, ACLs, ...).
//! 3. **Capability token.** On startup we generate 32 random bytes and write
//!    their hex to this instance's token file (`0600`). Every connection must
//!    present `AUTH <hex>` (or a `TOKEN <hex> <verb...>` prefix) as its first
//!    line; the server compares it to the stored token in constant time. A
//!    same-uid process that cannot read the `0600` token file (a sandboxed
//!    peer, a confused-deputy) is refused even though its uid matches.
//!
//! Instances do not collide: each binds its own `aterm-<pid>.sock` with a
//! matching `aterm-<pid>.token`, and a `aterm.sock` symlink is atomically
//! repointed at the newest instance so a single-instance `aterm-ctl` needs no
//! flags. Naming/staleness decisions are engine-side
//! ([`aterm_types::control_socket`]); this module does the filesystem work.
//!
//! "No nagging, keep power": there is NO prompt and NO new required flag. The
//! `aterm-ctl` client resolves the same directory, reads the token, and sends
//! the `AUTH` line automatically, so normal same-user usage is unchanged. A
//! same-uid client with the right token gets ALL verbs with zero friction;
//! everyone else is refused before the first verb runs.
//!
//! Platform split: the portable decisions live here; the filesystem/peer
//! primitives are per-platform sibling modules (`control_auth_unix.rs` — the
//! shipping POSIX code, moved verbatim — and `control_auth_win.rs`, whose
//! honestly-reduced posture — no peer-uid gate, but a `%LOCALAPPDATA%` directory
//! whose owner is VERIFIED and DACL hardened to owner-only
//! (`verify_owner_and_harden`), token still mandatory — is documented there and
//! disclosed at startup).

use std::path::{Path, PathBuf};

use aterm_types::control_socket::{self, SocketDirective};
use aterm_uds::CtlStream;

#[cfg(unix)]
#[path = "control_auth_unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "control_auth_win.rs"]
mod imp;

#[cfg(unix)]
pub use imp::our_uid;
pub use imp::{
    ensure_private_dir, lock_socket_file, peer_check, provision_token, publish_latest_link,
};
// Test-only import: `peer_uid`'s production caller (`peer_check`) lives inside
// the unix `imp` module; only the socketpair unit test below calls it directly.
#[cfg(all(unix, test))]
use imp::peer_uid;
// Test-only import: `random_token_hex`'s production callers live inside the
// per-platform `imp` modules (`provision_token`); only the unit tests below
// draw a token directly.
#[cfg(test)]
use imp::random_token_hex;

/// Token filename beside a socket that is not per-instance (an explicit
/// `$ATERM_CONTROL_SOCK` path).
pub const TOKEN_FILE: &str = control_socket::SIBLING_TOKEN_FILE;

/// Filename of the `latest` symlink in the per-user directory, pointing at
/// the newest instance's `aterm-<pid>.sock`.
pub const SOCK_FILE: &str = control_socket::LATEST_SOCK_FILE;

/// Subdirectory of the socket directory that confines `image`-verb PNG writes.
pub const IMAGES_DIR: &str = "images";

/// VIDEO introspection recordings subdir (frame sequences + index.json).
pub const VIDEO_DIR: &str = "video";

/// Filename of the PERSISTENT network-drive token, stored beside the operator's TLS
/// key (a persistent, operator-owned dir). Unlike the per-launch control token, this
/// SURVIVES a remote restart, so a saved `dial` credential is not invalidated every
/// time the remote process restarts (R37).
pub const NETWORK_DRIVE_TOKEN_FILE: &str = "aterm-network-drive.token";

/// Load (or generate-once into a `0600` file) the PERSISTENT network-drive token in
/// `dir` — the channel-binding secret a `dial` peer presents. Because it persists
/// across restarts, an operator provisions it ONCE (`dial-token <name> <hex>`) and
/// it keeps working, instead of re-copying the remote's per-launch control token
/// after every restart. Returns the 64-char hex, or `None` if entropy/write fails
/// (the caller then falls back to the per-launch token — network drive still works,
/// it just needs re-provisioning per restart, the pre-R37 behavior).
///
/// Returns `(hex, freshly_minted)` — `freshly_minted` is `true` ONLY on the run that
/// creates the file, so the caller reveals the raw secret to the operator's log once
/// (at provisioning) instead of on every boot (CWE-532).
#[must_use]
pub fn load_or_create_network_drive_token(dir: &Path) -> Option<(String, bool)> {
    let path = dir.join(NETWORK_DRIVE_TOKEN_FILE);
    // Reuse an existing valid token (the persistence that makes a saved credential
    // survive a restart). HARDENED read, matching the cert/key + control-token
    // readers: `O_NONBLOCK` so a same-uid writerless FIFO planted at this path cannot
    // park the control-socket serve thread at `open()`, `O_NOFOLLOW`, regular-file
    // only, capped — the bare `std::fs::read` this replaced had none of those. A
    // corrupt/short/unreadable file is regenerated.
    if let Some(hex) =
        read_drive_token_file(&path).filter(|h| aterm_session::EdgeToken::from_hex(h).is_some())
    {
        return Some((hex, false)); // loaded an existing token — NOT freshly minted
    }
    // First run (or corrupt file): mint a fresh token and persist it 0600.
    let hex = imp::random_token_hex()?;
    crate::snapshot_path::write_private(&path, hex.as_bytes()).ok()?;
    Some((hex, true)) // freshly minted — safe to reveal to the operator once
}

/// Hardened read of the persistent drive-token file: `O_NONBLOCK | O_NOFOLLOW`,
/// regular-file only, capped. `None` on any failure (the caller regenerates).
fn read_drive_token_file(path: &Path) -> Option<String> {
    use std::io::Read;
    #[cfg(unix)]
    let f = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)
            .ok()?
    };
    #[cfg(not(unix))]
    let f = std::fs::File::open(path).ok()?;
    if !f.metadata().ok()?.file_type().is_file() {
        return None; // FIFO / device / directory / symlink target — refuse
    }
    let mut bytes = Vec::new();
    f.take(4096).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).trim().to_string())
}

/// Create + canonicalize a fresh SERVER-NAMED recording directory under the
/// socket dir's `video/` subdir. ZERO client-controlled path components — the
/// stamp name is generated here — so this is strictly stronger than the image
/// confinement posture (there is no request string to normalize at all). Every
/// file inside is then written via the same dir-fd `write_private_at` contract
/// the image writers use. Returns the canonical recording dir.
#[must_use]
pub fn confine_video_dir(sock_dir: &Path) -> Option<PathBuf> {
    let root = sock_dir.join(VIDEO_DIR);
    ensure_private_dir(&root).ok()?;
    let canon = std::fs::canonicalize(&root).ok()?;
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    for n in 0..1000u32 {
        let dir = canon.join(format!("rec-{base}-{n:03}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                let _ = ensure_private_dir(&dir); // clamp perms (0700 posture)
                prune_video_dirs(&canon, &dir);
                return std::fs::canonicalize(&dir).ok();
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Recordings kept on disk, INCLUDING the one just created. Each recording can
/// hold up to a full frame budget in PNGs (hundreds of MiB at `full`), so an
/// agent that records in a loop would otherwise grow `video/` without bound;
/// the server prunes on every new recording, oldest first.
const VIDEO_KEEP: usize = 8;

/// Delete recordings beyond [`VIDEO_KEEP`], never touching `fresh` (the dir just
/// created for the in-flight recording). The server-named `rec-<epoch>-<nnn>`
/// stamps sort oldest-first lexicographically, so name order IS age order.
/// Best-effort: an undeletable entry is skipped — retention must never fail a
/// recording.
///
/// Only prunes COMPLETED recordings (those that own an `index.json`, the completion
/// marker the encoder writes LAST). An in-flight recording's async encode worker
/// writes PNGs + the index into its dir over seconds-to-minutes, but its dir is NOT
/// `fresh` for a LATER `video` call — so without the completion gate, a second
/// recording's prune could `remove_dir_all` a dir the first recording's worker is
/// still writing into, destroying a recording mid-encode.
fn prune_video_dirs(root: &Path, fresh: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut recs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p != fresh
                && p.is_dir()
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("rec-"))
                // COMPLETED only: an in-flight encode has no index.json yet, so it is
                // never eligible for pruning while its worker is still writing.
                && p.join("index.json").is_file()
        })
        .collect();
    recs.sort();
    // `fresh` occupies one keep slot; the newest VIDEO_KEEP-1 older ones stay.
    let excess = recs.len().saturating_sub(VIDEO_KEEP - 1);
    for old in recs.into_iter().take(excess) {
        let _ = std::fs::remove_dir_all(&old);
    }
}

/// Resolve the per-user directory that holds the control socket, token, and
/// image-confinement subdir, creating it if missing (`0700` on Unix; on
/// Windows the default per-user ACL is the boundary — see
/// `control_auth_win.rs`).
///
/// Order of preference (matched exactly by the `aterm-ctl` client):
/// 1. `$XDG_RUNTIME_DIR/aterm` when `XDG_RUNTIME_DIR` is set (already a
///    per-user `0700` dir on systems that provide it).
/// 2. `~/Library/Application Support/aterm` on macOS (the conventional
///    per-user app-support location), created `0700`.
/// 3. Windows: `%TEMP%\aterm` (`%TMP%`/`%TEMP%`, else `%LOCALAPPDATA%\Temp\aterm`)
///    — deliberately OUTSIDE the OneDrive-managed `%APPDATA%` subtree, where afunix
///    `connect` cannot reach the socket's reparse point (WSAEINVAL). See
///    [`aterm_uds::control_socket_dir`]. Short enough for the 108-byte `sun_path`.
///
/// The directory DECISION is shared with the client (`aterm-ctl`) via
/// [`aterm_uds::control_socket_dir`] so server and client can never drift; the
/// server additionally hardens the dir (owner-check + DACL) below.
///
/// Returns `None` only when the per-user base cannot be resolved from the
/// environment, which should not happen for an interactive session.
#[must_use]
pub fn socket_dir() -> Option<PathBuf> {
    let dir = aterm_uds::control_socket_dir()?;
    ensure_private_dir(&dir).ok()?;
    Some(dir)
}

/// Everything the server needs to provision one instance's control socket.
#[derive(Clone)]
pub struct SocketPlan {
    /// Path to bind the listening socket at.
    pub sock_path: String,
    /// Path of this instance's capability-token file.
    pub token_path: PathBuf,
    /// The `latest` convenience symlink to maintain (`None` for an explicit
    /// `$ATERM_CONTROL_SOCK` override, which owns its path outright).
    pub latest_link: Option<PathBuf>,
}

/// How the control socket should be provisioned this launch.
pub enum SocketResolution {
    /// Bind per this plan.
    Enabled(SocketPlan),
    /// Explicitly disabled via the environment; do not bind.
    Disabled,
    /// No per-user directory and no override resolvable; do not bind.
    NoDir,
    /// `$ATERM_CONTROL_SOCK` names a path too long for `sun_path`; do not bind.
    /// Both `bind` and `connect` on such a path fail `EINVAL` — and the fail-safe
    /// liveness probe must treat an unexpected connect error as "maybe live", so
    /// without this variant the launch log claimed "already has a live listener"
    /// for a socket that can never exist (a real 25-minute debugging trap). The
    /// warning names the actual limit instead.
    PathTooLong { path: String, limit: usize },
}

/// The longest socket path `sockaddr_un` can carry on this platform, EXCLUDING
/// the NUL terminator: `sun_path` is 104 bytes on the BSDs/macOS and 108 on
/// Linux/Windows (`UNIX_PATH_MAX`), one of which the kernel spends on the NUL.
pub const MAX_SUN_PATH: usize = if cfg!(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
)) {
    103
} else {
    107
};

/// Whether `path` fits `sockaddr_un` on this platform (see [`MAX_SUN_PATH`]).
/// Byte length, not char count — `sun_path` is a byte array.
#[must_use]
pub fn sun_path_ok(path: &str) -> bool {
    path.len() <= MAX_SUN_PATH
}

/// The socket plan decision. `$ATERM_CONTROL_SOCK` may name an explicit path,
/// or disable the socket entirely with `0`/`off` (as does
/// `$ATERM_NO_CONTROL_SOCK=1`); unset/empty means the per-instance default
/// `aterm-<pid>.sock` inside [`socket_dir`], published via the `aterm.sock`
/// symlink. The decision itself is engine-side
/// ([`control_socket::socket_directive`]); this just reads the environment.
#[must_use]
pub fn resolve_socket_plan() -> SocketResolution {
    let explicit = std::env::var_os("ATERM_CONTROL_SOCK").map(|v| v.to_string_lossy().into_owned());
    let kill = std::env::var_os("ATERM_NO_CONTROL_SOCK").map(|v| v.to_string_lossy().into_owned());
    match control_socket::socket_directive(explicit.as_deref(), kill.as_deref()) {
        SocketDirective::Disabled => SocketResolution::Disabled,
        SocketDirective::Explicit(p) => {
            // An oversized path can never bind (EINVAL) — and its liveness probe
            // would misread the same EINVAL as "maybe live". Name the real
            // problem instead of running socketless behind a misleading warning.
            if !sun_path_ok(&p) {
                return SocketResolution::PathTooLong {
                    path: p,
                    limit: MAX_SUN_PATH,
                };
            }
            let token_path = dir_of_socket(&p).join(TOKEN_FILE);
            SocketResolution::Enabled(SocketPlan {
                sock_path: p,
                token_path,
                latest_link: None,
            })
        }
        SocketDirective::PerInstance => match socket_dir() {
            Some(dir) => {
                let pid = std::process::id();
                SocketResolution::Enabled(SocketPlan {
                    sock_path: dir
                        .join(control_socket::instance_sock_name(pid))
                        .to_string_lossy()
                        .into_owned(),
                    token_path: dir.join(control_socket::instance_token_name(pid)),
                    latest_link: Some(dir.join(SOCK_FILE)),
                })
            }
            None => SocketResolution::NoDir,
        },
    }
}

/// Remove per-instance sockets/tokens left behind by instances whose pid is
/// no longer alive (a crashed session cannot clean up after itself). Live
/// instances — including ourselves — and the fixed filenames are untouched.
pub fn sweep_stale_instances(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    for stale in control_socket::stale_instance_files(&refs, &imp::pid_alive) {
        let _ = std::fs::remove_file(dir.join(stale));
    }
}

/// Graceful-exit cleanup: remove this instance's socket + token, and the
/// `latest` alias ONLY while it still points at our socket (a newer
/// instance may have repointed it). Crash exits are covered by
/// [`sweep_stale_instances`] at the next spawn.
pub fn cleanup_socket(plan: &SocketPlan) {
    let _ = std::fs::remove_file(&plan.sock_path);
    let _ = std::fs::remove_file(&plan.token_path);
    if let Some(link) = &plan.latest_link {
        let our_pid = Path::new(&plan.sock_path)
            .file_name()
            .and_then(|f| control_socket::instance_pid(&f.to_string_lossy()));
        let target = aterm_uds::latest::target_name(link);
        if let (Some(pid), Some(target)) = (our_pid, target)
            && control_socket::symlink_targets_pid(&target.to_string_lossy(), pid)
        {
            let _ = std::fs::remove_file(link);
        }
    }
}

/// The directory a given socket `path` lives in — used to locate the sibling
/// token file and `images/` subdir for an explicit `$ATERM_CONTROL_SOCK`.
#[must_use]
pub fn dir_of_socket(path: &str) -> PathBuf {
    Path::new(path)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Read the capability token from `<dir>/aterm.token`, trimming whitespace.
/// The symmetric counterpart of [`provision_token`]: the `aterm-ctl` client
/// reads the token equivalently (resolving the per-instance token through the
/// `latest` symlink), and the server uses it in tests and as a self-check.
/// Returns `None` if unreadable (wrong user, missing).
#[must_use]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "symmetric API; client reads token equivalently")
)]
pub fn read_token(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join(TOKEN_FILE)).ok()?;
    let t = raw.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// Whether a control socket at `path` has a LIVE listener: a successful `connect`
/// proves someone is bound. FAIL-SAFE — only an explicit connection-refused /
/// not-found means "stale, safe to remove"; any other error is treated as live so
/// we never unlink a maybe-live socket. (The Windows `CtlStream::connect`
/// explicitly normalizes a missing path to `NotFound` so this fail-safe holds
/// there too.)
#[must_use]
pub fn socket_is_live(path: &str) -> bool {
    use std::io::ErrorKind;
    match CtlStream::connect(path) {
        Ok(_) => true,
        Err(e) => !matches!(e.kind(), ErrorKind::ConnectionRefused | ErrorKind::NotFound),
    }
}

/// [`socket_is_live`] for the PER-INSTANCE default plan (`aterm-<ourpid>.sock`).
///
/// The fail-safe above INVERTS for this path: nobody else can legitimately own a
/// path named after OUR pid, so the only thing an unexpected connect error
/// (`EPERM` under a hardened runtime, `ETIMEDOUT`, `ECONNRESET` from a half-dead
/// leftover of a crashed same-pid predecessor) can indicate is stale junk — and
/// treating it as "live" means running SOCKETLESS for the process lifetime with
/// no retry (the 2026-07-05 dark-introspection incident's prime suspect). Only a
/// SUCCESSFUL connect — a listener actually answering — refuses the bind here.
/// Explicit shared `$ATERM_CONTROL_SOCK` paths keep the strict
/// [`socket_is_live`] fail-safe (never hijack a maybe-live parent).
#[must_use]
pub fn socket_is_live_per_instance(path: &str) -> bool {
    CtlStream::connect(path).is_ok()
}

/// What to do with an existing socket path before binding (Item 5).
#[derive(Debug, PartialEq, Eq)]
pub enum BindAction {
    /// No live listener — remove any stale file and bind.
    RemoveAndBind,
    /// A live listener already owns this path — do NOT touch it; run socket-less.
    RefuseLiveSocket,
}

/// Decide whether it is safe to (unlink and) bind a socket path, given whether a
/// live listener is already there. REFUSE a live socket so a nested aterm that
/// somehow still sees an explicit `$ATERM_CONTROL_SOCK` can never unlink+steal its
/// parent's listener (the liveness BELT to the env deny-list's SUSPENDERS — GAP-5
/// of the recursion topology). `is_live` is injected for unit-testability.
#[must_use]
pub fn decide_bind(is_live: bool) -> BindAction {
    if is_live {
        BindAction::RefuseLiveSocket
    } else {
        BindAction::RemoveAndBind
    }
}

/// Constant-time equality of two byte slices.
///
/// Always inspects every byte of the LONGER input (so length differences do
/// not leak via early return), accumulating differences into one flag and
/// folding the length comparison into the same flag. No `&&`/`||` short-circuit
/// and no early `return` on first mismatch.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let max = a.len().max(b.len());
    // Length mismatch sets the flag regardless of WHICH bits of the delta
    // differ (the old `as u8 | (>>8) as u8` fold only covered bits 0..16, so a
    // delta with all set bits >= 65536 was dropped). `u8::from(..) * 0xff` is
    // 0x00 when equal, 0xff when not — no overflow in debug or release.
    let mut diff: u8 = u8::from(a.len() != b.len()) * 0xff;
    for i in 0..max {
        // Reading past the end of the shorter slice would be UB; index into the
        // longer via wrapping to 0 with a sentinel that always differs when one
        // side is exhausted. `diff` already carries the length mismatch, so the
        // result is correct regardless of the sentinel value chosen.
        let av = *a.get(i).unwrap_or(&0);
        let bv = *b.get(i).unwrap_or(&0xff);
        diff |= av ^ bv;
    }
    diff == 0
}

/// Outcome of parsing/validating a connection's first line against the token.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Authenticated. If the first line carried an inline `TOKEN <hex> <verb>`
    /// the remaining verb text is returned so the caller dispatches it now;
    /// a bare `AUTH <hex>` yields `None` (the next line is the first verb).
    Ok(Option<String>),
    /// Authentication failed (bad/missing token line, wrong token).
    Denied,
}

/// Validate a connection's first line against the expected token.
///
/// Accepts two equivalent forms so a client can authenticate without an extra
/// round-trip:
/// * `AUTH <hex>`            — dedicated auth line; the verb follows on line 2.
/// * `TOKEN <hex> <verb...>` — token + first verb folded into one line.
///
/// The comparison is constant-time. Anything else (no token line, wrong token,
/// malformed) is [`AuthOutcome::Denied`].
#[must_use]
pub fn check_auth_line(line: &str, expected: &str) -> AuthOutcome {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let (head, rest) = match line.split_once(' ') {
        Some((h, r)) => (h, r),
        None => (line, ""),
    };
    match head {
        "AUTH" => {
            if constant_time_eq(rest.trim_end().as_bytes(), expected.as_bytes()) {
                AuthOutcome::Ok(None)
            } else {
                AuthOutcome::Denied
            }
        }
        "TOKEN" => {
            // `TOKEN <hex> <verb...>`: split off the hex, keep the verb tail.
            let (hex, verb) = match rest.split_once(' ') {
                Some((h, v)) => (h, v),
                None => (rest, ""),
            };
            if constant_time_eq(hex.trim_end().as_bytes(), expected.as_bytes()) {
                AuthOutcome::Ok(Some(verb.to_string()))
            } else {
                AuthOutcome::Denied
            }
        }
        _ => AuthOutcome::Denied,
    }
}

/// A caller-supplied `image` path confined to the `images/` subdir, as a
/// canonical directory plus a SINGLE final filename component.
///
/// TOCTOU-1: the confinement decision is made on the control thread but the
/// WRITE happens on the main thread, so we must not let the writer re-resolve a
/// multi-segment path string (an intermediate dir could be symlink-swapped in
/// the gap). Returning the canonical `images/` dir and a bare filename — with
/// NESTED target dirs forbidden — lets the writer open the directory
/// `O_DIRECTORY|O_NOFOLLOW` once and `openat` the final component, so there is no
/// intermediate path component left to swap.
#[derive(Clone, Debug)]
pub struct ConfinedImage {
    /// The canonical `images/` directory (the only directory ever opened).
    pub dir: PathBuf,
    /// The single, validated filename to create inside `dir` (no separators).
    pub file_name: std::ffi::OsString,
}

impl ConfinedImage {
    /// The full path, for logging / `OK <w> <h> <path>` replies only — NOT for
    /// re-opening (the writer must use [`Self::dir`] + [`Self::file_name`]).
    #[must_use]
    pub fn display_path(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }
}

/// Confine a caller-supplied `image` path to the `images/` subdir of the
/// socket directory.
///
/// The subdir is created `0700`. A relative or bare-filename request is
/// resolved INTO the subdir; an absolute request must already live inside it.
/// NESTED target directories are FORBIDDEN — the file must be a direct child of
/// `images/` — so the only directory component is the canonical subdir itself
/// (closing the intermediate-dir symlink-swap window, TOCTOU-1). Returns the
/// canonical dir + validated filename, or `None` (→ `ERR path`) when the request
/// would escape or names a nested path.
///
/// SPEC: this is the real `Confine` action of the external `PathConfine.tla` model
/// (TRUST_NATIVE_TLA Phase 2, control-socket CONFINEMENT family). The spec's
/// `WriteWithinSubdir` / `EscapeRejected` invariants — a committed write only ever
/// lands INSIDE the root, and a request whose final component resolves OUTSIDE is
/// rejected with no write — are exactly the confused-deputy symlink escape this
/// canonicalize-the-RESOLVED-location check fixes (the old guard checked only the
/// parent prefix, then the writer FOLLOWED a symlinked last segment). A `Some(_)`
/// here maps to the spec's `committed=TRUE, target="inside"`; a `None` (escape) maps
/// to `committed=FALSE, target="none"`. Tier-1 conformance drives this against a real
/// planted symlink (`tests/conformance_pathconfine.rs`).
// Anchor gated on `test` ALONE (not a feature): the relocated `spec_xref_closure`
// gate lives in THIS crate's own test build, so `cfg(test)` already makes this
// anchor visible to it — no cross-crate `spec-anchors` feature is needed here.
// PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): `Confine` projects the real
// confine outcome onto the spec's `<<linkOutside, decided, committed, target>>` — the
// projection the `path_confine_conformance` Tier-1 test drives (`Some` → committed
// inside, `None` → rejected). L2 requires the projection NAME be present (Trust does
// not execute it); `aterm_gui::control_auth::project_confine` is that witness.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "path_confine",
        action = "Confine",
        project = "aterm_gui::control_auth::project_confine"
    )
)]
#[must_use]
pub fn confine_image_path(sock_dir: &Path, requested: &str) -> Option<ConfinedImage> {
    let images = sock_dir.join(IMAGES_DIR);
    ensure_private_dir(&images).ok()?;
    let canon_images = std::fs::canonicalize(&images).ok()?;

    let req = Path::new(requested);
    // Map the request to a candidate path inside (or claimed-inside) the subdir.
    // A bare name or relative path is taken relative to the images subdir, never
    // the process cwd, so `aterm-ctl image shot.png` just works.
    let raw_candidate = if req.is_absolute() {
        req.to_path_buf()
    } else {
        canon_images.join(req)
    };

    // LEXICAL normalization: collapse `.`/`..` purely on the path string,
    // refusing any `..` that would climb above the ROOT. This kills `..`-escape
    // tricks WITHOUT depending on whether the (possibly non-existent) target is
    // on disk — closing the hole where a non-existent escape parent (e.g.
    // `../../etc/passwd`) would slip past a canonicalize() that errors.
    let lexical = lexically_normalize(&raw_candidate)?;

    // FORBID NESTED TARGET DIRS (TOCTOU-1): the file's parent, canonicalized,
    // must be EXACTLY the canonical images subdir — not merely inside it. This
    // means there is a single directory component (`images/`) and one filename,
    // so the writer never re-resolves a multi-segment string whose intermediate
    // dir could be symlink-swapped between this check and the open.
    let file_name = lexical.file_name()?;
    // A filename with a path separator or `..`/`.` is not a single component.
    if Path::new(file_name).components().count() != 1 {
        return None;
    }
    let parent = lexical.parent()?;
    let canon_parent = std::fs::canonicalize(parent).ok()?;
    if canon_parent != canon_images {
        return None;
    }
    // Reject a SYMLINK at the final component up front (defence in depth): the
    // writer also uses `O_NOFOLLOW`, but rejecting here gives a clean `ERR path`
    // for the common case and avoids even attempting the open.
    let resolved = canon_images.join(file_name);
    if let Ok(md) = std::fs::symlink_metadata(&resolved)
        && md.file_type().is_symlink()
    {
        return None;
    }
    Some(ConfinedImage {
        dir: canon_images,
        file_name: file_name.to_os_string(),
    })
}

/// Confine a discovery-graph socket path to the trusted socket directory before the
/// proxy forward dials it and presents a parent-minted edge token. The graph entry
/// (`<sock_dir>/graph/<sid>`) is same-uid writable AND carries the launch nonce in
/// plain text, so the nonce guard alone does NOT stop a hostile same-uid process from
/// overwriting a child's entry with `sock <attacker-path>` (copying the readable
/// nonce) to make the parent connect to an attacker socket and hand over the
/// capability token. Mirror [`confine_image_path`]'s posture: the path must name a
/// single real file DIRECTLY in `sock_dir` (canonical parent == canonical `sock_dir`,
/// exactly one component, no `..`) and must NOT be a symlink — so a forward can only
/// ever dial a real socket inside our own runtime dir, never a redirected one.
/// Returns the confined (canonical-dir-rooted) path, or `None` to fail the forward
/// closed. The legit publisher writes exactly `<sock_dir>/aterm-<pid>.sock`.
#[must_use]
pub fn confine_proxy_sock(sock_dir: &Path, requested: &str) -> Option<String> {
    let canon_dir = std::fs::canonicalize(sock_dir).ok()?;
    let req = Path::new(requested);
    if !req.is_absolute() {
        return None; // graph entries publish ABSOLUTE instance-socket paths
    }
    // Lexically kill `.`/`..` first (so a non-existent escape can't slip past a
    // canonicalize that errors), then require a single filename whose canonical
    // parent is EXACTLY our socket dir — never a nested or redirected directory.
    let lexical = lexically_normalize(req)?;
    let file_name = lexical.file_name()?;
    if Path::new(file_name).components().count() != 1 {
        return None;
    }
    let canon_parent = std::fs::canonicalize(lexical.parent()?).ok()?;
    if canon_parent != canon_dir {
        return None;
    }
    // Reject a SYMLINK at the final component: `connect()` follows symlinks, so a
    // same-uid attacker could otherwise plant `<sock_dir>/aterm-x.sock -> /attacker`.
    let resolved = canon_dir.join(file_name);
    if let Ok(md) = std::fs::symlink_metadata(&resolved)
        && md.file_type().is_symlink()
    {
        return None;
    }
    Some(resolved.to_string_lossy().into_owned())
}

/// Lexically resolve `.`/`..`/`//` in an ABSOLUTE path WITHOUT touching the
/// filesystem. Returns `None` if a `..` would escape above the root (so a path
/// can never resolve above `/`). Symlinks are intentionally NOT followed here —
/// that is the canonicalize re-check's job; this pass just kills `..` tricks.
fn lexically_normalize(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                match out.last() {
                    // Pop a normal segment; refuse to climb above the root.
                    Some(Component::Normal(_)) => {
                        out.pop();
                    }
                    Some(Component::RootDir) | None => return None,
                    _ => out.push(comp),
                }
            }
            other => out.push(other),
        }
    }
    Some(out.iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ITEM 5: `socket_is_live` is true for a bound listener and false for an
    /// absent OR stale (file present, nobody listening) socket — so the bind gate
    /// removes stale files but never unlinks a live listener.
    #[test]
    fn socket_is_live_true_for_bound_false_for_stale() {
        let dir = std::env::temp_dir().join(format!("aterm-live-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.sock");
        let ps = path.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&path);
        assert!(!socket_is_live(&ps), "absent socket is not live");
        let listener = aterm_uds::CtlListener::bind(&path).expect("bind");
        assert!(socket_is_live(&ps), "bound socket is live");
        drop(listener); // file remains, nobody listens -> becomes stale
        // Socket teardown is ASYNCHRONOUS: under heavy parallel load there is a brief
        // window after the listener fd closes where a connect to the now-orphaned path
        // returns something OTHER than ECONNREFUSED/NotFound, which `socket_is_live`
        // (deliberately fail-SAFE: any ambiguous error => "live", so we never unlink a
        // maybe-live socket) reports as live. Poll until the kernel has fully torn the
        // listener down — it must report stale within a bounded window; never doing so
        // is a real regression, not the teardown race.
        let mut went_stale = false;
        for _ in 0..200 {
            if !socket_is_live(&ps) {
                went_stale = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            went_stale,
            "stale socket file (no listener) must become not-live"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// The bind decision refuses a live socket (never hijack) and removes-then-binds
    /// a stale/absent one.
    #[test]
    fn decide_bind_refuses_live_keeps_stale() {
        assert_eq!(decide_bind(true), BindAction::RefuseLiveSocket);
        assert_eq!(decide_bind(false), BindAction::RemoveAndBind);
    }

    #[test]
    fn constant_time_eq_matches() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"", b""));
        let tok = "deadbeef".repeat(8);
        assert!(constant_time_eq(tok.as_bytes(), tok.as_bytes()));
    }

    #[test]
    fn constant_time_eq_rejects_differences() {
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"abc", b""));
        assert!(!constant_time_eq(b"", b"abc"));
    }

    #[test]
    fn constant_time_eq_detects_length_delta_above_16_bits() {
        // Regression: the length fold once only covered the low 16 bits of the
        // usize delta, so all-zero slices whose lengths differ by exactly
        // 0x10000 (65536) compared EQUAL — the common prefix matched and the
        // dropped delta bit hid the rest. Must report NOT equal.
        let short = vec![0u8; 64];
        let long = vec![0u8; 64 + 65536];
        assert!(!constant_time_eq(&short, &long));
        assert!(!constant_time_eq(&long, &short));
    }

    /// An explicit socket path over the platform `sun_path` capacity must be
    /// refused BY NAME (`PathTooLong`), never funneled into the bind/probe path —
    /// bind AND connect both fail `EINVAL` there, and the fail-safe liveness
    /// probe reads that as "maybe live", producing the misleading "already has a
    /// live listener" launch warning for a socket that can never exist.
    #[test]
    fn sun_path_limit_is_enforced_byte_wise() {
        assert!(sun_path_ok(&"a".repeat(MAX_SUN_PATH)), "at the limit binds");
        assert!(
            !sun_path_ok(&"a".repeat(MAX_SUN_PATH + 1)),
            "one over is refused"
        );
        // Byte length, not char count: a multibyte path near the limit.
        let mb = format!("{}é", "a".repeat(MAX_SUN_PATH - 1)); // é = 2 bytes ⇒ limit+1
        assert!(!sun_path_ok(&mb), "multibyte overflow is caught byte-wise");
        // Platform sanity: the constant matches the OS family.
        #[cfg(target_os = "macos")]
        assert_eq!(MAX_SUN_PATH, 103);
        #[cfg(target_os = "linux")]
        assert_eq!(MAX_SUN_PATH, 107);
    }

    #[test]
    fn random_token_is_64_hex_chars() {
        let t = random_token_hex().expect("entropy available");
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        // Two draws must differ (astronomically unlikely to collide).
        let t2 = random_token_hex().expect("entropy available");
        assert_ne!(t, t2);
    }

    #[test]
    fn auth_line_accepts_correct_token() {
        let tok = "a".repeat(64);
        assert_eq!(
            check_auth_line(&format!("AUTH {tok}"), &tok),
            AuthOutcome::Ok(None)
        );
        // CRLF tolerance: a trailing CR must not break the compare.
        assert_eq!(
            check_auth_line(&format!("AUTH {tok}\r"), &tok),
            AuthOutcome::Ok(None)
        );
    }

    #[test]
    fn auth_line_rejects_wrong_token() {
        let tok = "a".repeat(64);
        let bad = "b".repeat(64);
        assert_eq!(
            check_auth_line(&format!("AUTH {bad}"), &tok),
            AuthOutcome::Denied
        );
        assert_eq!(check_auth_line("AUTH", &tok), AuthOutcome::Denied);
        assert_eq!(check_auth_line("text", &tok), AuthOutcome::Denied);
        assert_eq!(check_auth_line("", &tok), AuthOutcome::Denied);
    }

    #[test]
    fn token_prefix_form_carries_verb() {
        let tok = "c".repeat(64);
        assert_eq!(
            check_auth_line(&format!("TOKEN {tok} text"), &tok),
            AuthOutcome::Ok(Some("text".to_string()))
        );
        assert_eq!(
            check_auth_line(&format!("TOKEN {tok} send echo hi"), &tok),
            AuthOutcome::Ok(Some("send echo hi".to_string()))
        );
        // Bare token with no verb still authenticates (empty verb tail).
        assert_eq!(
            check_auth_line(&format!("TOKEN {tok}"), &tok),
            AuthOutcome::Ok(Some(String::new()))
        );
        // Wrong token in TOKEN form is denied.
        let bad = "d".repeat(64);
        assert_eq!(
            check_auth_line(&format!("TOKEN {bad} text"), &tok),
            AuthOutcome::Denied
        );
    }

    #[test]
    #[cfg(unix)]
    fn ensure_private_dir_refuses_group_or_other_writable() {
        use std::os::unix::fs::PermissionsExt;
        // SEC-3: a pre-existing dir that is group/other-writable is REFUSED, not
        // silently provisioned into — even after we force the mode, a foreign
        // owner / loose bits indicate an unsafe directory.
        let dir = std::env::temp_dir().join(format!("aterm-dir-gw-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        // Loosen it behind ensure_private_dir's back, then call again: it forces
        // 0700 and the re-stat passes (we own it). Verify the success path.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o770)).unwrap();
        // ensure_private_dir forces 0700, so a subsequent call succeeds (owner is
        // us, bits tightened). This proves the gate accepts our own dir.
        ensure_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "ensure_private_dir must force 0700");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn provision_token_refuses_symlinked_path() {
        // SEC-3: provision_token must not write THROUGH a symlink planted at the
        // token path (O_EXCL|O_NOFOLLOW + unlink-first). The victim is untouched.
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("aterm-tok-sym-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        let tokpath = dir.join("aterm.token");
        symlink(&victim, &tokpath).unwrap();
        // unlink-first removes the symlink, then O_EXCL|O_NOFOLLOW creates a real
        // file — so the token lands in a fresh regular file, NOT the victim.
        let _ = provision_token(&tokpath);
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"original",
            "the symlink target must not be written through",
        );
        // And the token path is now a regular file (the unlink-first replaced the
        // symlink), readable as a 0600 token.
        let md = std::fs::symlink_metadata(&tokpath).unwrap();
        assert!(
            md.file_type().is_file(),
            "token path must be a regular file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn provision_and_read_token_roundtrip() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aterm-auth-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let written = provision_token(&dir.join(TOKEN_FILE)).expect("token written");
        let read = read_token(&dir).expect("token readable");
        assert_eq!(written, read);
        // Token file is 0600.
        let mode = std::fs::metadata(dir.join(TOKEN_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        // Dir is 0700.
        let dmode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Windows twin of the Unix roundtrip + symlink tests: the token
    /// roundtrips, and `create_new(true)` (CREATE_NEW — the `O_EXCL|O_NOFOLLOW`
    /// analog) refuses ANY pre-existing object at the path, so the token is
    /// only ever written into a file we just created; `provision_token`'s
    /// unlink-first still lets a rotation succeed over our own stale token.
    #[test]
    #[cfg(windows)]
    fn provision_token_roundtrips_and_create_new_refuses_preexisting() {
        let dir = std::env::temp_dir().join(format!("aterm-auth-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let tokpath = dir.join(TOKEN_FILE);
        // A planted pre-existing file: the exclusive-create primitive refuses it.
        std::fs::write(&tokpath, b"planted").unwrap();
        let err = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tokpath)
            .expect_err("CREATE_NEW must refuse a pre-existing object");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        // provision_token unlinks first, then exclusively creates: rotation works.
        let written = provision_token(&tokpath).expect("token written");
        let read = read_token(&dir).expect("token readable");
        assert_eq!(written, read);
        assert_eq!(written.len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confine_image_path_allows_inside_and_rejects_escape() {
        let dir = std::env::temp_dir().join(format!("aterm-img-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();

        // Bare name resolves into images/.
        let ok = confine_image_path(&dir, "shot.png").expect("bare name allowed");
        assert_eq!(ok.file_name.to_str(), Some("shot.png"));
        assert!(
            ok.display_path().ends_with("images/shot.png"),
            "got {:?}",
            ok.display_path()
        );

        // `../` escape is rejected.
        assert!(confine_image_path(&dir, "../escape.png").is_none());
        assert!(confine_image_path(&dir, "../../etc/passwd").is_none());

        // Absolute path outside the subdir is rejected.
        assert!(confine_image_path(&dir, "/tmp/evil.png").is_none());

        // Absolute path that IS inside the subdir is allowed.
        let inside = dir.join(IMAGES_DIR).join("ok.png");
        let allowed =
            confine_image_path(&dir, inside.to_str().unwrap()).expect("absolute-inside allowed");
        assert!(allowed.display_path().ends_with("images/ok.png"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confine_image_path_rejects_nested_target_dir() {
        // TOCTOU-1: a NESTED target dir (images/sub/shot.png) is forbidden even
        // if the subdir exists and is inside images/ — so the writer only ever
        // opens the single canonical images/ directory and openat's one name,
        // leaving no intermediate dir component to symlink-swap between threads.
        let dir = std::env::temp_dir().join(format!("aterm-img-nested-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let images = dir.join(IMAGES_DIR);
        ensure_private_dir(&images).unwrap();
        ensure_private_dir(&images.join("sub")).unwrap();
        assert!(
            confine_image_path(&dir, "sub/shot.png").is_none(),
            "a nested target dir must be rejected (intermediate-dir TOCTOU)"
        );
        // The absolute form of the same nested path is rejected too.
        let nested_abs = images.join("sub").join("shot.png");
        assert!(confine_image_path(&dir, nested_abs.to_str().unwrap()).is_none());
        // A direct child still works.
        assert!(confine_image_path(&dir, "shot.png").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn confine_image_path_rejects_symlinked_final_component() {
        // A same-uid token-holding client plants a symlink AT the final
        // component (images/evil.png -> a file OUTSIDE the subdir). The parent
        // canonicalizes inside images/, so the old containment check passed and
        // the writer would follow the link and clobber an arbitrary file. This
        // is exactly the confused-deputy escape confine_image_path must stop.
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("aterm-img-symlink-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let images = dir.join(IMAGES_DIR);
        ensure_private_dir(&images).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, b"original").unwrap();
        symlink(&victim, images.join("evil.png")).unwrap();

        assert!(
            confine_image_path(&dir, "evil.png").is_none(),
            "a symlinked final component must be rejected (arbitrary-write escape)"
        );
        // Legit cases still work: a fresh name and an existing REGULAR file.
        assert!(confine_image_path(&dir, "fresh.png").is_some());
        std::fs::write(images.join("real.png"), b"x").unwrap();
        assert!(confine_image_path(&dir, "real.png").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn confine_proxy_sock_allows_in_dir_and_rejects_redirects() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("aterm-proxysock-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let canon = std::fs::canonicalize(&dir).unwrap();

        // The legit publisher writes `<sock_dir>/aterm-<pid>.sock` — accepted, and
        // returned rooted at the CANONICAL dir (so a later dial can't be redirected
        // through a symlinked prefix).
        let legit = dir.join("aterm-42.sock");
        let got =
            confine_proxy_sock(&dir, &legit.to_string_lossy()).expect("in-dir socket allowed");
        assert_eq!(got, canon.join("aterm-42.sock").to_string_lossy());

        // An absolute path OUTSIDE the runtime dir (the attacker overwrite) is rejected.
        assert!(confine_proxy_sock(&dir, "/tmp/evil-attacker.sock").is_none());
        // A `..` escape and a relative path are rejected.
        let escape = dir.join("../evil.sock");
        assert!(confine_proxy_sock(&dir, &escape.to_string_lossy()).is_none());
        assert!(
            confine_proxy_sock(&dir, "aterm-42.sock").is_none(),
            "relative rejected"
        );
        // A nested subdir under the runtime dir is rejected (single component only).
        let nested = dir.join("sub/aterm-42.sock");
        assert!(confine_proxy_sock(&dir, &nested.to_string_lossy()).is_none());

        // A SYMLINK planted at the final component (pointing outside) is rejected —
        // `connect()` would otherwise follow it to the attacker socket.
        symlink("/tmp/evil-attacker.sock", dir.join("aterm-redir.sock")).unwrap();
        assert!(
            confine_proxy_sock(&dir, &dir.join("aterm-redir.sock").to_string_lossy()).is_none(),
            "a symlinked socket name must be rejected (token-capture escape)",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn peer_uid_of_socketpair_is_our_uid() {
        // A connected UnixStream pair: the peer of each end is us.
        let (a, _b) = CtlStream::pair().expect("socketpair");
        if let Some(uid) = peer_uid(&a) {
            assert_eq!(uid, our_uid());
        }
        // On platforms without a peer-cred primitive, peer_uid is None — the
        // caller fails closed, which is the correct conservative behaviour.
        // And the accept gate accepts our own connection.
        assert_eq!(peer_check(&a), Ok(()));
    }

    /// Windows has no peer-cred primitive at all: the gate passes every
    /// same-machine peer (the mandatory token + dir ACL are the gates — the
    /// reduction the startup notice discloses). "None ⇒ refuse" here would
    /// refuse EVERY connection.
    #[test]
    #[cfg(windows)]
    fn peer_check_passes_on_windows() {
        let (a, _b) = CtlStream::pair().expect("socketpair");
        assert_eq!(peer_check(&a), Ok(()));
    }

    /// A minimal stand-in for the server's `serve` auth preamble, run over a
    /// real `CtlStream` pair, proving the on-the-wire handshake:
    /// * the `AUTH <hex>` line is consumed SILENTLY on success (no reply), so
    ///   the first reply a client reads is the response to its first verb;
    /// * a bad/missing token yields exactly `ERR auth\n` and closes.
    fn run_auth_preamble(mut server: CtlStream, token: &str) {
        use std::io::{BufRead, BufReader, Write};
        let reader = BufReader::new(server.try_clone().unwrap());
        let mut lines = reader.lines();
        let first = match lines.next() {
            Some(Ok(l)) => l,
            _ => return,
        };
        match check_auth_line(&first, token) {
            // A folded-in verb (`TOKEN <hex> <verb>`) is answered immediately;
            // we must NOT then block reading another line (the client sent only
            // one). A bare `AUTH`/empty `TOKEN` reads the next line as the verb.
            // This mirrors the real `serve` preamble exactly.
            AuthOutcome::Ok(Some(v)) if !v.is_empty() => {
                let _ = server.write_all(format!("OK {v}\n").as_bytes());
            }
            AuthOutcome::Ok(_) => {
                if let Some(Ok(next)) = lines.next() {
                    let _ = server.write_all(format!("OK {next}\n").as_bytes());
                }
            }
            AuthOutcome::Denied => {
                let _ = server.write_all(b"ERR auth\n");
            }
        }
        let _ = server.flush();
    }

    #[test]
    fn handshake_correct_token_runs_verb() {
        use std::io::{BufRead, BufReader, Write};
        let token = "e".repeat(64);
        let (client, server) = CtlStream::pair().unwrap();
        let tok = token.clone();
        let h = std::thread::spawn(move || run_auth_preamble(server, &tok));

        // Client: AUTH first (silently consumed), then the verb.
        (&client)
            .write_all(format!("AUTH {token}\n").as_bytes())
            .unwrap();
        (&client).write_all(b"text\n").unwrap();
        (&client).flush().unwrap();
        let mut reply = String::new();
        BufReader::new(&client).read_line(&mut reply).unwrap();
        h.join().unwrap();
        // The FIRST reply is the response to `text`, NOT to AUTH.
        assert_eq!(reply, "OK text\n");
    }

    #[test]
    fn handshake_missing_token_is_refused() {
        use std::io::{BufRead, BufReader, Write};
        let token = "f".repeat(64);
        let (client, server) = CtlStream::pair().unwrap();
        let h = std::thread::spawn(move || run_auth_preamble(server, &token));

        // Client skips AUTH and goes straight to a verb.
        (&client).write_all(b"send rm -rf /\n").unwrap();
        (&client).flush().unwrap();
        let mut reply = String::new();
        BufReader::new(&client).read_line(&mut reply).unwrap();
        h.join().unwrap();
        assert_eq!(reply, "ERR auth\n");
    }

    #[test]
    fn sweep_removes_only_dead_instances_files() {
        let dir = std::env::temp_dir().join(format!("aterm-sweep-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        // A certainly-dead pid: a reaped child cannot be signalled any more.
        #[cfg(unix)]
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("0")
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit"])
            .spawn()
            .unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let us = std::process::id();
        let touch = |name: &str| std::fs::write(dir.join(name), b"x").unwrap();
        touch(&control_socket::instance_sock_name(dead));
        touch(&control_socket::instance_token_name(dead));
        touch(&control_socket::instance_sock_name(us));
        touch(&control_socket::instance_token_name(us));
        touch(TOKEN_FILE);

        sweep_stale_instances(&dir);

        assert!(!dir.join(control_socket::instance_sock_name(dead)).exists());
        assert!(!dir.join(control_socket::instance_token_name(dead)).exists());
        // Our own (live) files and the fixed names survive.
        assert!(dir.join(control_socket::instance_sock_name(us)).exists());
        assert!(dir.join(control_socket::instance_token_name(us)).exists());
        assert!(dir.join(TOKEN_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latest_link_publishes_atomically_and_repoints() {
        let dir = std::env::temp_dir().join(format!("aterm-link-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let link = dir.join(SOCK_FILE);

        let first = dir.join("aterm-101.sock");
        publish_latest_link(&link, first.to_str().unwrap());
        // The target is the RELATIVE instance name (valid via any dir path);
        // read back through the portable alias helper (readlink on Unix, the
        // pointer file's validated contents on Windows).
        assert_eq!(
            aterm_uds::latest::target_name(&link).as_deref(),
            Some(std::ffi::OsStr::new("aterm-101.sock"))
        );

        // A newer instance wins the link; no temp residue is left behind.
        let second = dir.join("aterm-202.sock");
        publish_latest_link(&link, second.to_str().unwrap());
        assert_eq!(
            aterm_uds::latest::target_name(&link).as_deref(),
            Some(std::ffi::OsStr::new("aterm-202.sock"))
        );
        assert!(!dir.join("aterm-202.sock.lnk").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_removes_own_files_and_only_our_symlink() {
        let dir = std::env::temp_dir().join(format!("aterm-clean-test-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let link = dir.join(SOCK_FILE);
        let plan = SocketPlan {
            sock_path: dir.join("aterm-4242.sock").to_string_lossy().into_owned(),
            token_path: dir.join("aterm-4242.token"),
            latest_link: Some(link.clone()),
        };
        let provision = || {
            std::fs::write(&plan.sock_path, b"x").unwrap();
            std::fs::write(&plan.token_path, b"x").unwrap();
        };

        // Link points at us: everything goes.
        provision();
        publish_latest_link(&link, &plan.sock_path);
        cleanup_socket(&plan);
        assert!(!Path::new(&plan.sock_path).exists());
        assert!(!plan.token_path.exists());
        assert!(aterm_uds::latest::target_name(&link).is_none());

        // Link repointed by a newer instance: our files go, the link stays.
        provision();
        publish_latest_link(&link, dir.join("aterm-9.sock").to_str().unwrap());
        cleanup_socket(&plan);
        assert!(!Path::new(&plan.sock_path).exists());
        assert_eq!(
            aterm_uds::latest::target_name(&link).as_deref(),
            Some(std::ffi::OsStr::new("aterm-9.sock"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handshake_token_prefix_form_runs_inline_verb() {
        use std::io::{BufRead, BufReader, Write};
        let token = "1".repeat(64);
        let (client, server) = CtlStream::pair().unwrap();
        let tok = token.clone();
        let h = std::thread::spawn(move || run_auth_preamble(server, &tok));

        // One-line auth + verb.
        (&client)
            .write_all(format!("TOKEN {token} text\n").as_bytes())
            .unwrap();
        (&client).flush().unwrap();
        let mut reply = String::new();
        BufReader::new(&client).read_line(&mut reply).unwrap();
        h.join().unwrap();
        assert_eq!(reply, "OK text\n");
    }

    #[test]
    fn video_dir_prune_keeps_the_newest_recordings() {
        let dir = std::env::temp_dir().join(format!("aterm-vid-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Server-named stamps sort oldest-first; mint 12 fake past recordings
        // (zero-padded epochs sort before any real `rec-17…` stamp).
        let root = dir.join(super::VIDEO_DIR);
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..12 {
            let d = root.join(format!("rec-{:010}-000", 1_000_000 + i));
            std::fs::create_dir(&d).unwrap();
            // A COMPLETED recording owns an index.json (only completed ones prune).
            std::fs::write(d.join("index.json"), b"{\"frames\":[]}").unwrap();
        }
        // An IN-FLIGHT recording (older stamp, but NO index.json — its encode worker
        // hasn't written the completion marker yet) must survive the prune untouched.
        let in_flight = root.join("rec-0000000001-000");
        std::fs::create_dir(&in_flight).unwrap();

        let fresh = super::confine_video_dir(&dir).expect("create + prune");
        let mut recs: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("rec-"))
            .collect();
        recs.sort();
        assert!(
            in_flight.is_dir(),
            "an in-flight (index-less) recording is never pruned mid-encode"
        );
        assert_eq!(
            recs.len(),
            super::VIDEO_KEEP + 1,
            "prune keeps VIDEO_KEEP COMPLETED recordings (incl. fresh) + the in-flight one"
        );
        assert!(
            fresh.ends_with(recs.last().unwrap()),
            "the just-created dir survives as the newest"
        );
        // The in-flight (index-less) dir sorts first and is never pruned; the oldest
        // surviving COMPLETED stamp is the 6th (the oldest five completed were deleted).
        assert_eq!(
            recs[0], "rec-0000000001-000",
            "the in-flight dir survives, sorts first"
        );
        assert_eq!(recs[1], format!("rec-{:010}-000", 1_000_000 + 5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R37: the network-drive token PERSISTS — a second call (simulating a remote
    /// restart) returns the SAME token, so a saved dial credential is not
    /// invalidated. A fresh dir generates one; a corrupt file is regenerated.
    #[test]
    fn network_drive_token_persists_across_restarts() {
        let dir = std::env::temp_dir().join(format!("aterm-drive-tok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // First launch: mint + persist a valid 64-hex token; `minted` is true so the
        // caller reveals it to the operator's log exactly once.
        let (t1, minted1) = super::load_or_create_network_drive_token(&dir).expect("mint token");
        assert!(minted1, "first run freshly mints (reveal-once)");
        assert_eq!(t1.len(), 64, "64-char hex");
        assert!(
            aterm_session::EdgeToken::from_hex(&t1).is_some(),
            "valid EdgeToken"
        );
        // A file was written (0600 on unix — perms enforced by write_private).
        assert!(dir.join(super::NETWORK_DRIVE_TOKEN_FILE).exists());

        // SECOND launch (a "restart"): the SAME token — the persistence R37 needs —
        // and `minted` is now FALSE, so the raw secret is NOT re-logged.
        let (t2, minted2) = super::load_or_create_network_drive_token(&dir).expect("reload token");
        assert_eq!(t1, t2, "the drive token survives a restart");
        assert!(!minted2, "a reload does not re-reveal the token");

        // A corrupt file is regenerated (never a stuck invalid credential).
        std::fs::write(dir.join(super::NETWORK_DRIVE_TOKEN_FILE), b"garbage").unwrap();
        let (t3, minted3) = super::load_or_create_network_drive_token(&dir).expect("regen token");
        assert_ne!(t3, t1, "a corrupt token file is regenerated");
        assert!(minted3, "a regenerated token counts as freshly minted");
        assert!(aterm_session::EdgeToken::from_hex(&t3).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
