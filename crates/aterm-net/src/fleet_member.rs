// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FLEET MEMBERSHIP (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5):
//! what a machine keeps about its fleet beside the secret ([`KEY_FILE`]), and
//! the one exchange `aterm fleet join` makes with the fleet's issuer once the
//! handshake ([`crate::fleet`]) has proved both ends.
//!
//! **The join code** ([`JoinCode`]) carries the secret to another machine:
//! `atf1-`, the 160-bit secret in lowercase Crockford base32 (32 characters)
//! and four check characters, in nine groups of four, then `@<host>:<port>`
//! of the issuer — `atf1-q7xk-29fa-…-4hcz@m7.local:7100`. The check
//! characters are a Reed–Solomon code over GF(32) (four roots, so a weight of
//! up to four errors at positions apart by less than 31 never cancels): every
//! one-character typo in the secret and every swap of two neighbours there is
//! refused at once. The host and port carry no check: a typo there fails the
//! join's dial, and `aterm fleet join` writes nothing of the fleet until the
//! issuer has recorded it. `o` reads as `0` and `i`/`l` as `1`, as
//! Crockford's alphabet has it; `u` is no character of it.
//!
//! **The state files**, each directly in the state directory under the key
//! file's rules ([`read_private`]: this user's directory, 0700; the file
//! 0600, one link, a regular file, neither a symlink) and each written whole
//! through a new temporary file and a rename ([`write_private`]), so a reader
//! sees the old bytes or the new, never a part:
//!
//! | file | holds | written by |
//! |---|---|---|
//! | [`KEY_FILE`] | the fleet secret | `aterm fleet init`, `join` |
//! | [`CODE_FILE`] | the join code — the secret again | `aterm fleet init` |
//! | [`NODE_FILE`] | this machine's node id, 128 random bits in hex | the first `init` or `join` ([`node_id`]) |
//! | [`NODE_KEY_FILE`] | this machine's node key: the seed of its Ed25519 key pair, in hex | the first `init` or `join` after keys came ([`node_identity`]) |
//! | [`PEERS_FILE`] | the book of peers, one [`Peer`] per line | the issuer's listener ([`serve_join`]), `aterm fleet join` (the issuer), `aterm fleet peers add\|rm` |
//! | [`ISSUER_BOOK_FILE`] | a member's copy of its issuer's book: the issuer's node, then one [`Peer`] per line | a member's read of that book ([`write_issuer_book`]), never by hand |
//!
//! The book is edited under a lock beside it ([`update_peers`]) and holds at
//! most [`PEERS_MAX`] peers, and at most [`JOINED_PER_ADDRESS`] that
//! registered from one address: a registration past either is refused, never
//! one that evicts a member, so one secret holder cannot fill an issuer's
//! book with made-up node ids.
//!
//! **Node keys** ([`NODE_KEY_FILE`], `crate::fleet`'s **Nodes**). Each node
//! mints a key pair of its own at its first `join` (the issuer at `init`),
//! kept by `leave` as its node id is, and proves it on every connection. The
//! issuer binds a node id to the key its first registration proved — trust
//! on first registration — and refuses a later registration of that node
//! with another key, or with none, naming `aterm fleet peers rm <node>` on
//! the issuer as the remedy ([`record`]). An entry an older aterm recorded
//! names no key (an older book parses as before), and is bound at that
//! node's next registration from the address it was recorded from, which a
//! registration that proves no key never moves ([`Peer::key_from`]). A
//! member binds its issuer's key at its join, and copies the keys its
//! issuer's book binds when that book came from the issuer it binds
//! ([`copies_of`]). Every read checks the node that answered against those
//! bindings ([`Keyring`]), whatever entry it was dialled by: what the book
//! binds a key to answers as that node only when it proves that key.
//!
//! **The join exchange.** After the handshake (op `join`, proved with its own
//! key), the member sends one line ([`Registration`]):
//!
//! ```text
//! member → issuer   register node=<32 hex> host=<pct> port=<n> version=<pct> LF
//! issuer → member   OK node=<32 hex> host=<pct> port=<n> version=<pct> peers=<n> LF  |  ERR <why> LF
//! ```
//!
//! each at most [`RECORD_MAX`] bytes. The issuer — the machine that holds
//! the join code; any other answers `ERR` — records the member in its book,
//! keyed by node id, with the address the connection came from, and answers
//! naming itself, which the member records as its issuer ([`serve_join`],
//! [`register`]). The host name is a display label (and the
//! name tried when the address no longer answers); the node id is the key.
//!
//! **Reaching a peer** ([`dial`], [`read_whole`]): the recorded address first,
//! then the host name (and `<host>.local` for a name with no dot), IPv4 before
//! IPv6 — on one LAN segment every IPv6 host is one source to a listener's
//! slot rule (`fleet::serve`) — the name lookup (on a thread of its own),
//! each connect (at most [`CONNECT_WITHIN`]) and the handshake under one
//! wall-clock deadline, and a failure is a reason a person can act on
//! ([`Unreached`]). A recorded address where something else now answers —
//! no listener, another fleet's, one that refuses — gives way to the name,
//! and only addresses a fleet listener serves are dialled
//! ([`crate::fleet::is_fleet_source`]: a registered `host=` points no
//! reader past the LAN).
//!
//! **Which node answered**: the node a peer proved on the connection, else
//! (an older aterm, which proves none) the node its survey's `instance`
//! header names (`node=`). A peer that answers as another node than the
//! book's — the address passed to another member — gives way to the name
//! too, and is reported, never labelled as this peer's
//! ([`Unreached::OtherNode`]); one that claims the book's node and proves
//! another key, or proves none where the book binds one, is not read as that
//! node either ([`Unreached::Impostor`], [`Unreached::Unproved`]), and its
//! survey is never asked.
//!
//! **Reading the fleet** ([`survey_all`]): every peer asked `survey` at once,
//! at most [`AT_ONCE`] at a time, each under one deadline that covers its
//! dial, the request and the whole reply (at most [`REPLY_MAX`] bytes). The
//! level each answers at is its own `[fleet] allow`, which its listener
//! proved; the request asks no level.
//!
//! **The issuer's book, read by a member** ([`serve_book`], [`fetch_book`]):
//! after the handshake (op `book`, proved with the read key and served at
//! every level, the book being facts), the listener answers unasked
//!
//! ```text
//! issuer → member   OK <n> node=<32 hex> LF, then n × <a book line, via=joined|manual> LF
//! other  → member   ERR no-book [node=<32 hex>] <why> LF  |  ERR not-now [node=<32 hex>] <why> LF
//! ```
//!
//! The issuer sends its first-hand entries — the members that registered and
//! the peers its person named, at most [`PEERS_MAX`]. A machine that holds no
//! join code answers `no-book`, naming its node when it has one; an issuer
//! that cannot share its book this time answers `not-now`; an older aterm
//! answers the op `NO op`. A member keeps the book's entries as
//! `via=issuer-book` copies in a file of their own ([`ISSUER_BOOK_FILE`]),
//! which an older aterm never opens: the issuer's node, then the copies
//! ([`copies_of`]: this machine and the issuer itself left out, an entry at a
//! loopback address placed at the address the issuer answered from, as it
//! is on the issuer's machine). The file is replaced whole at each answer and
//! never edited by hand: `peers rm` refuses a copy ([`remove`]). It is read
//! only while the member's book names that node as its issuer
//! ([`read_issuer_book`]), so it goes with the issuer's entry. Copies are
//! never passed on, and a copy older than [`COPY_DRIVES_FOR`] admits no
//! drive until the issuer's book is read again ([`proven_driver`]). Read
//! after the machine's own entries ([`with_copies`]), a node or place they
//! hold stands for its copy, and together they hold at most [`PEERS_MAX`],
//! the machine's own first.
//!
//! **Reading the whole fleet** ([`read_whole`]): a member asks its issuer's
//! book, then the issuer's own survey where the book answered. It asks every
//! peer it knows meanwhile, and each member the book adds as soon as the book
//! comes, each under one deadline: the first [`AT_ONCE`] dials share it, and a
//! peer past them waits for a free place and is given the deadline of its
//! own. A copy that answers as this machine is left out. A refusal counts
//! only when it is the issuer's own: its node in the `ERR`, or its survey
//! answering with its node at the address that refused. Then the copies go
//! and the member reads the peers it knows, as before the book. When the
//! issuer's survey found it at another address, the book is asked there; any
//! other answer keeps the copies.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rustls::ClientConnection;

use crate::fleet::{
    Dialed, DriveGrant, FleetError, FleetOp, FleetScope, FleetSecret, KEY_FILE, NodeIdentity,
    NodeKey, NodeProof, REPLY_MAX, Served, client_handshake_as,
};
use crate::slots::{Claim, HandshakeWatchdog};
use crate::tls::TlsTransport;

/// The join code's file in the state directory (the issuer's: `aterm fleet
/// init` writes it, and prints its path, never the code).
pub const CODE_FILE: &str = "fleet-join-code";

/// This machine's node id's file in the state directory.
pub const NODE_FILE: &str = "fleet-node";

/// This machine's node key's file in the state directory: the 32-byte seed
/// of its node's Ed25519 key pair, as 64 lowercase hex digits and a newline,
/// under the key file's rules. Kept by `leave`, as [`NODE_FILE`] is: a
/// machine that joins again is the same node, with the same key.
pub const NODE_KEY_FILE: &str = "fleet-node-key";

/// The book of peers' file in the state directory.
pub const PEERS_FILE: &str = "fleet-peers";

/// A member's copy of its issuer's book, in the state directory: a file of
/// its own, so an older aterm reading [`PEERS_FILE`] never meets a copy, and
/// no edit of the book (`peers add`, `rm`, a join) ever writes one.
pub const ISSUER_BOOK_FILE: &str = "fleet-issuer-book";

/// The lock [`update_peers`] holds while it edits the book.
const PEERS_LOCK: &str = ".fleet-peers.lock";

/// The most peers one book holds: a fleet is one LAN's machines, and a secret
/// holder cannot grow an issuer's book past this with made-up node ids.
pub const PEERS_MAX: usize = 256;

/// The most members the book records from one address ([`record`]): several
/// aterm instances on one host, each on a fleet port of its own, and no more —
/// a secret holder cannot fill an issuer's book with made-up nodes from one
/// address. A legitimate fleet of MANY instances on ONE host (every member at
/// 127.0.0.1, the thirty-on-one-Mac proof `targo --unverified lab fleet-sim`,
/// `crates/aterm-lab/src/fleet_sim.rs`) reaches past this with the issuer's
/// own [`Via::Manual`] entries (`aterm fleet peers add 127.0.0.1:<port>`): the
/// join still writes each member's key and `[fleet]`, and the hand-added book
/// entry is what the reader dials. On a real LAN each machine has its own
/// address and registration alone fills the book.
pub const JOINED_PER_ADDRESS: usize = 4;

/// The fleet port when none is named: `[fleet] listen`'s documented example.
pub const DEFAULT_PORT: u16 = 7100;

/// How many bytes of secret a join code carries: 160 bits, the fleet's
/// shortest secret.
pub const SECRET_LEN: usize = FleetSecret::MIN_LEN;

/// The longest line either end of the join exchange sends.
pub const RECORD_MAX: usize = 512;

/// The longest book file read: [`PEERS_MAX`] lines with room to spare.
const PEERS_FILE_MAX: usize = 256 * 1024;

/// How long a writer of the book waits for another one to finish.
const LOCK_WAIT: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// private files
// ---------------------------------------------------------------------------

/// Why a private state file gives nothing ([`read_private`]). None carries a
/// byte of the file.
#[derive(Debug)]
pub enum PrivateFileError {
    /// No such file (or no state directory).
    Missing,
    /// The file or its directory is another user's, or another user could
    /// reach it (a group or other mode bit, a second link to the file).
    NotPrivate(&'static str),
    /// The file or its directory is a symbolic link.
    Symlink,
    /// No regular file, or longer than such a file can be.
    Malformed(&'static str),
    /// The file could not be read.
    Io(io::Error),
}

impl fmt::Display for PrivateFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("there is no such file"),
            Self::NotPrivate(why) => write!(f, "not private: {why}"),
            Self::Symlink => f.write_str("it or its directory is a symlink"),
            Self::Malformed(why) => f.write_str(why),
            Self::Io(e) => write!(f, "it could not be read: {e}"),
        }
    }
}

impl std::error::Error for PrivateFileError {}

/// Whether `meta` is this user's and nobody else's: `whose` and `open` name
/// the two ways it is not. The mode bits only; [`granted_to_others`] judges
/// an access list, which mode bits do not show.
fn private(
    meta: &std::fs::Metadata,
    whose: &'static str,
    open: &'static str,
) -> Result<(), PrivateFileError> {
    use std::os::unix::fs::MetadataExt as _;
    if meta.uid() != aterm_uds::peer::our_uid() {
        Err(PrivateFileError::NotPrivate(whose))
    } else if meta.mode() & 0o077 != 0 {
        Err(PrivateFileError::NotPrivate(open))
    } else {
        Ok(())
    }
}

/// What a file or directory an extended access list opens to others is
/// refused as ([`granted_to_others`]).
const ACL_OPEN: &str = "an access list (ACL) grants other users access; `chmod -N` removes it";

/// Whether `target` — a path, read without following a link, or an open
/// descriptor — carries an access list that ALLOWS anyone anything: on macOS
/// an extended ACL is checked before the mode bits, so a 0600 file in a 0700
/// directory with an `everyone allow read` entry (one inherited from a
/// parent's `file_inherit` entry, say) is open to every user. A deny entry
/// (the `group:everyone deny delete` macOS puts on `~/Library`) takes nothing
/// away from privacy, and is no reason to refuse. Elsewhere mode bits are the
/// rule, and this is `false`.
#[cfg(target_os = "macos")]
fn granted_to_others(target: AclOf<'_>) -> io::Result<bool> {
    use std::os::raw::{c_char, c_int, c_void};
    const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: c_int = 0;
    const ACL_NEXT_ENTRY: c_int = -1;
    const ACL_EXTENDED_ALLOW: c_int = 1;
    // SAFETY (the declarations): `<sys/acl.h>`'s, `acl_t` and `acl_entry_t`
    // opaque pointers and `acl_type_t`/`acl_tag_t` C enums (`int`).
    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
        fn acl_get_link_np(path: *const c_char, kind: c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
        fn acl_free(object: *mut c_void) -> c_int;
    }
    let acl = match target {
        // SAFETY: `fd` is open for the call (the caller holds its file).
        AclOf::Fd(fd) => unsafe { acl_get_fd_np(fd, ACL_TYPE_EXTENDED) },
        AclOf::Path(path) => {
            use std::os::unix::ffi::OsStrExt as _;
            let path = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in a path"))?;
            // SAFETY: `path` is a NUL-terminated string that outlives the call.
            unsafe { acl_get_link_np(path.as_ptr(), ACL_TYPE_EXTENDED) }
        }
    };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        // No extended ACL at all: ENOENT.
        return match error.raw_os_error() {
            Some(libc::ENOENT) => Ok(false),
            _ => Err(error),
        };
    }
    let mut allows = false;
    let mut entry: *mut c_void = std::ptr::null_mut();
    let mut which = ACL_FIRST_ENTRY;
    // SAFETY: `acl` is the list just returned, freed once below; each entry
    // and its tag are read only while it lives.
    while unsafe { acl_get_entry(acl, which, &mut entry) } == 0 {
        which = ACL_NEXT_ENTRY;
        let mut tag: c_int = 0;
        // SAFETY: `entry` is an entry of the live `acl`; `tag` is this frame's.
        if unsafe { acl_get_tag_type(entry, &mut tag) } == 0 && tag == ACL_EXTENDED_ALLOW {
            allows = true;
            break;
        }
    }
    // SAFETY: `acl` came from `acl_get_*_np` and is freed exactly once.
    unsafe { acl_free(acl) };
    Ok(allows)
}

#[cfg(not(target_os = "macos"))]
fn granted_to_others(target: AclOf<'_>) -> io::Result<bool> {
    match target {
        AclOf::Fd(_fd) => Ok(false),
        AclOf::Path(_path) => Ok(false),
    }
}

/// What [`granted_to_others`] reads the access list of.
#[derive(Clone, Copy)]
enum AclOf<'a> {
    /// An open descriptor.
    Fd(std::os::raw::c_int),
    /// A path, not followed if it is a link.
    Path(&'a Path),
}

/// [`granted_to_others`] as a privacy refusal.
fn no_acl(target: AclOf<'_>) -> Result<(), PrivateFileError> {
    match granted_to_others(target) {
        Ok(false) => Ok(()),
        Ok(true) => Err(PrivateFileError::NotPrivate(ACL_OPEN)),
        Err(e) => Err(PrivateFileError::Io(e)),
    }
}

/// The bytes of `name` directly in `dir`, read under the key file's rules:
/// the directory and the file are this user's and nobody else's — by their
/// mode bits and by any access list ([`granted_to_others`]) — neither is a
/// symlink, and the file is one link to a regular file of at most `max`
/// bytes. Read without following a link and without waiting on a FIFO. No
/// such file is [`PrivateFileError::Missing`] whatever the directory is: a
/// machine in no fleet has a state directory as a launch left it, and its
/// mode is judged only when there is a secret in it to keep.
///
/// # Errors
/// A [`PrivateFileError`] naming the first rule the file or directory breaks.
pub fn read_private(dir: &Path, name: &str, max: usize) -> Result<Vec<u8>, PrivateFileError> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let meta = match std::fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PrivateFileError::Missing),
        Err(e) => return Err(PrivateFileError::Io(e)),
    };
    if meta.file_type().is_symlink() {
        return Err(PrivateFileError::Symlink);
    }
    if !meta.is_dir() {
        return Err(PrivateFileError::Malformed(
            "the state directory is no directory",
        ));
    }
    match std::fs::symlink_metadata(dir.join(name)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PrivateFileError::Missing),
        Err(e) if e.kind() != io::ErrorKind::PermissionDenied => {
            return Err(PrivateFileError::Io(e));
        }
        // There is a file (or one this user cannot see past its directory's
        // mode, which the directory's judgement names): judge both.
        _ => {}
    }
    private(
        &meta,
        "the state directory is another user's",
        "the state directory is open to other users (want 0700)",
    )?;
    no_acl(AclOf::Path(dir))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(name));
    let file = match file {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(PrivateFileError::Missing),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(PrivateFileError::Symlink);
        }
        Err(e) => return Err(PrivateFileError::Io(e)),
    };
    let meta = file.metadata().map_err(PrivateFileError::Io)?;
    if !meta.file_type().is_file() {
        return Err(PrivateFileError::Malformed("not a regular file"));
    }
    if meta.nlink() != 1 {
        return Err(PrivateFileError::NotPrivate("it has another link"));
    }
    private(
        &meta,
        "it is another user's",
        "it is open to other users (want 0600)",
    )?;
    no_acl(AclOf::Fd(file.as_raw_fd()))?;
    let mut bytes = Vec::with_capacity(max.min(64 * 1024) + 1);
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(PrivateFileError::Io)?;
    if bytes.len() > max {
        return Err(PrivateFileError::Malformed(
            "longer than such a file can be",
        ));
    }
    Ok(bytes)
}

/// Make `dir` a private state directory, or say why it cannot be one: made
/// 0700 when it is missing (its parents as the platform makes them),
/// narrowed to 0700 when it is this user's but open to others — the mode it
/// had is returned, so the caller can say so — and refused when it is a
/// symlink, no directory, or another user's. Nothing in it is touched.
///
/// # Errors
/// The refusal, worded for a person, or the filesystem's own error.
pub fn ensure_private_dir(dir: &Path) -> io::Result<Option<u32>> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    let refused = |why: &str| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the state directory {} {why}", dir.display()),
        )
    };
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            Ok(None)
        }
        Err(e) => Err(e),
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(refused("is a symlink; it must be a directory of its own"))
        }
        Ok(meta) if !meta.is_dir() => Err(refused("is no directory")),
        Ok(meta) if meta.uid() != aterm_uds::peer::our_uid() => Err(refused("is another user's")),
        Ok(_) if granted_to_others(AclOf::Path(dir))? => Err(refused(&format!(
            "has {}",
            ACL_OPEN.replace("`chmod -N`", &format!("`chmod -N {}`", dir.display()))
        ))),
        Ok(meta) if meta.mode() & 0o077 != 0 => {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            Ok(Some(meta.mode() & 0o777))
        }
        Ok(_) => Ok(None),
    }
}

/// Whether `dir` is a private state directory as it stands: this user's,
/// 0700, no symlink. A writer checks it and never repairs it
/// ([`ensure_private_dir`] is the one step that does, and says so).
fn check_private_dir(dir: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    let refused = |why: &dyn fmt::Display| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the state directory {}: {why}", dir.display()),
        )
    };
    if meta.file_type().is_symlink() {
        return Err(refused(&PrivateFileError::Symlink));
    }
    if !meta.is_dir() {
        return Err(refused(&"no directory"));
    }
    private(
        &meta,
        "it is another user's",
        "it is open to other users (want 0700)",
    )
    .and_then(|()| no_acl(AclOf::Path(dir)))
    .map_err(|e| refused(&e))
}

/// Write `name` directly in the private state directory `dir`, 0600, whole:
/// through a new temporary file beside it and a rename
/// ([`aterm_uds::atomic_file::write`]), so a reader sees the old bytes or the
/// new, never a part, and nothing planted at a temporary name is written
/// through.
///
/// # Errors
/// `dir` is not private ([`ensure_private_dir`] makes it so), or a write's own
/// error.
pub fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    check_private_dir(dir)?;
    aterm_uds::atomic_file::write(&dir.join(name), bytes, 0o600)
}

/// Remove `name` from `dir`: `true` when there was one.
///
/// # Errors
/// The removal's own error.
pub fn remove_private(dir: &Path, name: &str) -> io::Result<bool> {
    match std::fs::remove_file(dir.join(name)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// the secret and the node id
// ---------------------------------------------------------------------------

/// A new fleet secret, [`SECRET_LEN`] bytes from the OS CSPRNG.
///
/// # Errors
/// No OS entropy source.
pub fn mint_secret() -> io::Result<[u8; SECRET_LEN]> {
    let mut secret = [0u8; SECRET_LEN];
    aterm_uds::rand::fill(&mut secret)?;
    Ok(secret)
}

/// Write `secret` as this machine's [`KEY_FILE`] in the private state
/// directory `dir`, in the one spelling `fleet::read_key_file` reads
/// (lowercase hex and a newline), whole: the listener reads the key every
/// 2 s, and never sees half of one.
///
/// # Errors
/// A secret of a length no fleet takes, `dir` not private, or a write's own
/// error.
pub fn write_key_file(dir: &Path, secret: &[u8]) -> io::Result<()> {
    FleetSecret::new(secret)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut text = aterm_codec::hex::encode_lower(secret);
    text.push('\n');
    write_private(dir, KEY_FILE, text.as_bytes())
}

/// Whether `word` is a node id: 32 lowercase hex digits.
#[must_use]
pub fn is_node_id(word: &str) -> bool {
    aterm_codec::hex::is_lower(word, 32)
}

/// This machine's node id, from [`NODE_FILE`] in `dir`, minted and written the
/// first time (128 bits from the OS CSPRNG). It outlives a `leave`, so a
/// machine that joins again is the same node. Only `aterm fleet init|join`
/// mint it; what reads it on a peer's behalf reads it with [`read_node_id`].
///
/// # Errors
/// The file is not private or holds no node id, or no entropy, or a write's
/// own error.
pub fn node_id(dir: &Path) -> io::Result<String> {
    if let Some(id) = read_node_id(dir)? {
        return Ok(id);
    }
    let id = aterm_uds::rand::hex_token::<16>()?;
    write_private(dir, NODE_FILE, format!("{id}\n").as_bytes())?;
    Ok(id)
}

/// The most of [`CODE_FILE`] [`read_join_code`] reads: a code is under 100
/// bytes.
const CODE_FILE_MAX: usize = 4096;

/// The join code this machine issued, as [`CODE_FILE`] in `dir` holds it
/// (`aterm fleet init` wrote it): the secret, and the host and port members
/// dial.
///
/// # Errors
/// Why it cannot be read or is no join code, never quoting it.
pub fn read_join_code(dir: &Path) -> Result<JoinCode, String> {
    let bytes = read_private(dir, CODE_FILE, CODE_FILE_MAX).map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes).map_err(|_| "it is not text".to_owned())?;
    JoinCode::parse(&text).map_err(|e| e.to_string())
}

/// This machine's node id as [`NODE_FILE`] in `dir` holds it, never minted:
/// `None` when there is none yet (no `aterm fleet init` or `join` ran here).
///
/// # Errors
/// The file is not private, or holds no node id.
pub fn read_node_id(dir: &Path) -> io::Result<Option<String>> {
    match read_private(dir, NODE_FILE, 64) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let word = text.strip_suffix('\n').unwrap_or(&text);
            if is_node_id(word) {
                Ok(Some(word.to_owned()))
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{NODE_FILE} holds no node id (32 lowercase hex digits)"),
                ))
            }
        }
        Err(PrivateFileError::Missing) => Ok(None),
        Err(e) => Err(io::Error::other(format!("{NODE_FILE}: {e}"))),
    }
}

/// The most of [`NODE_KEY_FILE`] is read: a seed's 64 digits and a newline.
const NODE_KEY_FILE_MAX: usize = 2 * NODE_SEED_LEN + 1;

/// A node key's seed's length.
const NODE_SEED_LEN: usize = 32;

/// This machine's node — its node id and its node key — minted the first
/// time each is missing ([`node_id`]; the key 256 bits from the OS CSPRNG,
/// written to [`NODE_KEY_FILE`]). Only `aterm fleet init|join` mint it; what
/// proves this machine on its listener's behalf reads it with
/// [`read_node_identity`]. Minting claims the book's lock before either file
/// is read, so concurrent init or join calls keep the first identity. A key
/// minted beside a node id an issuer already bound to another key is refused
/// there: `aterm fleet peers rm <node>` on the issuer, then a join again,
/// binds the new one.
///
/// # Errors
/// A file is not private or holds no node id or seed, no entropy, another
/// membership writer owns the lock, or a write's own error.
pub fn node_identity(dir: &Path) -> io::Result<NodeIdentity> {
    let _identity = BookLock::take(dir, Duration::ZERO)?;
    let node = node_id(dir)?;
    let seed = match read_node_seed(dir)? {
        Some(seed) => seed,
        None => {
            let mut seed = [0u8; NODE_SEED_LEN];
            aterm_uds::rand::fill(&mut seed)?;
            let mut text = aterm_codec::hex::encode_lower(&seed);
            text.push('\n');
            write_private(dir, NODE_KEY_FILE, text.as_bytes())?;
            seed
        }
    };
    identity_of(&node, &seed)
}

/// This machine's node as [`NODE_FILE`] and [`NODE_KEY_FILE`] in `dir` hold
/// it, never minted: `None` while either is missing (no `init` or `join`
/// ran here since keys came).
///
/// # Errors
/// A file is not private, or holds no node id or seed.
pub fn read_node_identity(dir: &Path) -> io::Result<Option<NodeIdentity>> {
    let Some(node) = read_node_id(dir)? else {
        return Ok(None);
    };
    read_node_seed(dir)?
        .map(|seed| identity_of(&node, &seed))
        .transpose()
}

/// The seed [`NODE_KEY_FILE`] in `dir` holds: `None` when there is none.
fn read_node_seed(dir: &Path) -> io::Result<Option<[u8; NODE_SEED_LEN]>> {
    let bytes = match read_private(dir, NODE_KEY_FILE, NODE_KEY_FILE_MAX) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(None),
        Err(e) => return Err(io::Error::other(format!("{NODE_KEY_FILE}: {e}"))),
    };
    let malformed = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{NODE_KEY_FILE} holds no node key (64 lowercase hex digits)"),
        )
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| malformed())?;
    let digits = text.strip_suffix('\n').unwrap_or(text);
    if !aterm_codec::hex::is_lower(digits, 2 * NODE_SEED_LEN) {
        return Err(malformed());
    }
    crate::hex_decode(digits)
        .and_then(|seed| seed.try_into().ok())
        .map(Some)
        .ok_or_else(malformed)
}

/// The node `node` with the key pair `seed` gives.
fn identity_of(node: &str, seed: &[u8]) -> io::Result<NodeIdentity> {
    NodeIdentity::from_seed(node, seed).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{NODE_KEY_FILE} gives no node key"),
        )
    })
}

// ---------------------------------------------------------------------------
// the join code
// ---------------------------------------------------------------------------

/// Crockford's base32 alphabet, lowercase: no `i`, `l`, `o` or `u`.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// What every join code starts with (the survey's redaction keys on it).
pub const CODE_PREFIX: &str = "atf1-";

/// The secret's characters: 160 bits, 5 to a character.
const DATA_SYMBOLS: usize = SECRET_LEN * 8 / 5;

/// The check characters: the Reed–Solomon code's roots.
const CHECK_SYMBOLS: usize = 4;

/// Every character of a code before its `@`.
const CODE_SYMBOLS: usize = DATA_SYMBOLS + CHECK_SYMBOLS;

/// Characters per group.
const GROUP: usize = 4;

/// GF(32)'s modulus, `x^5 + x^2 + 1`: primitive, so `x` (2) generates every
/// non-zero element.
const GF_POLY: u8 = 0b10_0101;

/// A join code: the fleet secret and where its issuer listens. Its `Debug`
/// shows the issuer, never the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCode {
    /// The fleet secret.
    pub secret: [u8; SECRET_LEN],
    /// The issuer's host name or address, as members dial it.
    pub host: String,
    /// The issuer's fleet port.
    pub port: u16,
}

impl fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JoinCode(.. @{})", host_port(&self.host, self.port))
    }
}

/// Why a text is no join code. None carries a character of the code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeError {
    /// It does not start `atf1-`.
    Prefix,
    /// No `@<host>:<port>` at its end.
    NoIssuer,
    /// The character at this place (1-based, counting only code characters)
    /// is no join-code character.
    Character(usize),
    /// This many code characters, not [`CODE_SYMBOLS`].
    Length(usize),
    /// The check characters do not match: a typo.
    Typo,
    /// The issuer part is no host and port, for this reason.
    Issuer(&'static str),
}

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prefix => f.write_str(
                "a join code starts `atf1-` (the file `aterm fleet init` wrote holds one)",
            ),
            Self::NoIssuer => {
                f.write_str("the join code names no issuer (`@<host>:<port>` at its end)")
            }
            Self::Character(at) => write!(
                f,
                "the join code's character {at} is none of 0-9 and a-z without i, l, o, u: \
                 it was not copied whole"
            ),
            Self::Length(n) => write!(
                f,
                "a join code has {CODE_SYMBOLS} characters before its `@`, not {n}: it was \
                 not copied whole"
            ),
            Self::Typo => f.write_str(
                "the join code's check characters do not match: a character is mistyped or \
                 two are swapped; copy it again",
            ),
            Self::Issuer(why) => write!(f, "the join code's issuer is no host and port: {why}"),
        }
    }
}

impl std::error::Error for CodeError {}

impl JoinCode {
    /// The code as text: `atf1-`, nine groups of four, `@<host>:<port>`.
    #[must_use]
    pub fn text(&self) -> String {
        let mut symbols = to_symbols(&self.secret);
        let check = check_symbols(&symbols);
        symbols.extend_from_slice(&check);
        let mut out = String::from(CODE_PREFIX);
        for (at, symbol) in symbols.iter().enumerate() {
            if at > 0 && at % GROUP == 0 {
                out.push('-');
            }
            out.push(char::from(ALPHABET[usize::from(*symbol & 31)]));
        }
        out.push('@');
        out.push_str(&host_port(&self.host, self.port));
        out
    }

    /// Read a join code: `atf1-` in either case, its characters in any case
    /// with `-` and white space between them ignored, then `@<host>:<port>`.
    ///
    /// # Errors
    /// A [`CodeError`] naming what is wrong, never quoting the code.
    pub fn parse(text: &str) -> Result<Self, CodeError> {
        let text = text.trim();
        let body = text
            .get(..CODE_PREFIX.len())
            .filter(|head| head.eq_ignore_ascii_case(CODE_PREFIX))
            .and_then(|_| text.get(CODE_PREFIX.len()..))
            .ok_or(CodeError::Prefix)?;
        let (code, issuer) = body.rsplit_once('@').ok_or(CodeError::NoIssuer)?;
        let mut symbols = Vec::with_capacity(CODE_SYMBOLS);
        let mut seen = 0usize;
        for c in code.chars() {
            if c == '-' || c.is_whitespace() {
                continue;
            }
            seen += 1;
            symbols.push(symbol_of(c).ok_or(CodeError::Character(seen))?);
        }
        if symbols.len() != CODE_SYMBOLS {
            return Err(CodeError::Length(symbols.len()));
        }
        if !is_codeword(&symbols) {
            return Err(CodeError::Typo);
        }
        let (host, port) = parse_host_port(issuer, None).map_err(CodeError::Issuer)?;
        Ok(Self {
            secret: from_symbols(&symbols[..DATA_SYMBOLS]),
            host,
            port,
        })
    }
}

/// WHERE A JOIN CODE SITS IN `text`, wherever it was pasted or typed: the
/// byte range of each run of [`CODE_SYMBOLS`] code characters (either case,
/// Crockford's `o`/`i`/`l` read as the parser reads them) that the check
/// characters accept, with `-` or white space between them as the code
/// prints them in groups and a person may type them — with its `atf1-` when
/// one leads it and its `@<host>:<port>` when one follows, and without
/// either. The survey's redaction replaces each (a code is the fleet's
/// secret), so a code is never quoted for want of its prefix or for its
/// groups sitting in separate words. Only runs that begin and end at a
/// word's edge are judged, and a run that is no codeword is passed over (the
/// four check characters accept a random one about once in a million), so
/// ordinary text is all but never taken for a code. In order, apart.
#[must_use]
pub fn code_spans(text: &str) -> Vec<std::ops::Range<usize>> {
    /// One word of code characters: where it starts and ends, its values.
    struct Word {
        start: usize,
        end: usize,
        symbols: Vec<u8>,
    }
    let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
    // The words since the last character that is neither a code character,
    // a `-` nor white space: the run a code may lie in.
    let mut run: Vec<Word> = Vec::new();
    let mut word: Option<Word> = None;
    let judge = |run: &[Word], spans: &mut Vec<std::ops::Range<usize>>| {
        let mut from = 0;
        while from < run.len() {
            let mut count = 0;
            let mut found = None;
            for (to, w) in run.iter().enumerate().skip(from) {
                count += w.symbols.len();
                if count >= CODE_SYMBOLS {
                    found = (count == CODE_SYMBOLS).then_some(to);
                    break;
                }
            }
            let Some(to) = found.filter(|&to| {
                let symbols: Vec<u8> = run[from..=to]
                    .iter()
                    .flat_map(|w| w.symbols.iter().copied())
                    .collect();
                is_codeword(&symbols)
            }) else {
                from += 1;
                continue;
            };
            let prefixed = from > 0
                && text[run[from - 1].start..run[from - 1].end]
                    .eq_ignore_ascii_case(CODE_PREFIX.trim_end_matches('-'));
            let start = if prefixed {
                run[from - 1].start
            } else {
                run[from].start
            };
            let mut end = run[to].end;
            if text[end..].starts_with('@') {
                end += text[end..]
                    .find(|c: char| {
                        !(c.is_ascii_alphanumeric()
                            || matches!(c, '@' | '.' | '-' | '_' | ':' | '[' | ']'))
                    })
                    .unwrap_or(text.len() - end);
            }
            spans.push(start..end);
            from = to + 1;
        }
    };
    for (at, c) in text.char_indices() {
        if let Some(symbol) = symbol_of(c) {
            word.get_or_insert_with(|| Word {
                start: at,
                end: at,
                symbols: Vec::new(),
            })
            .symbols
            .push(symbol);
            continue;
        }
        if let Some(mut w) = word.take() {
            w.end = at;
            run.push(w);
        }
        if c != '-' && !c.is_whitespace() {
            judge(&run, &mut spans);
            run.clear();
        }
    }
    if let Some(mut w) = word.take() {
        w.end = text.len();
        run.push(w);
    }
    judge(&run, &mut spans);
    spans
}

/// The value of one code character, Crockford's way: either case, `o` as
/// `0`, `i` and `l` as `1`.
fn symbol_of(c: char) -> Option<u8> {
    let c = match c.to_ascii_lowercase() {
        'o' => '0',
        'i' | 'l' => '1',
        c => c,
    };
    let byte = u8::try_from(c).ok()?;
    ALPHABET
        .iter()
        .position(|a| *a == byte)
        .and_then(|at| u8::try_from(at).ok())
}

/// `bytes` as 5-bit symbols, most significant bit first. 160 bits make 32
/// symbols exactly.
fn to_symbols(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(CODE_SYMBOLS);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &byte in bytes {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(((acc >> bits) & 31) as u8);
        }
        acc &= (1 << bits) - 1;
    }
    out
}

/// [`to_symbols`] undone: [`DATA_SYMBOLS`] symbols back into the secret.
fn from_symbols(symbols: &[u8]) -> [u8; SECRET_LEN] {
    let mut out = [0u8; SECRET_LEN];
    let (mut acc, mut bits, mut at) = (0u32, 0u32, 0usize);
    for &symbol in symbols {
        acc = (acc << 5) | u32::from(symbol & 31);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            if let Some(slot) = out.get_mut(at) {
                *slot = ((acc >> bits) & 0xff) as u8;
            }
            at += 1;
        }
        acc &= (1 << bits) - 1;
    }
    out
}

/// Multiplication in GF(32) modulo [`GF_POLY`].
fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0;
    while b != 0 {
        if b & 1 != 0 {
            product ^= a;
        }
        b >>= 1;
        a <<= 1;
        if a & 0x20 != 0 {
            a ^= GF_POLY;
        }
    }
    product
}

/// The code's generator, `(x + 1)(x + α)(x + α²)(x + α³)` with `α = x`,
/// highest power first.
fn generator() -> [u8; CHECK_SYMBOLS + 1] {
    let mut g = [0u8; CHECK_SYMBOLS + 1];
    g[0] = 1;
    let mut root = 1u8;
    for degree in 0..CHECK_SYMBOLS {
        for i in (1..=degree + 1).rev() {
            g[i] ^= gf_mul(g[i - 1], root);
        }
        root = gf_mul(root, 2);
    }
    g
}

/// The check symbols of `data` (highest power first): the remainder of
/// `data(x)·x⁴` by the generator, so data and check together are a multiple
/// of it and vanish at its four roots.
fn check_symbols(data: &[u8]) -> [u8; CHECK_SYMBOLS] {
    let g = generator();
    let mut rem = [0u8; CHECK_SYMBOLS];
    for &symbol in data {
        let factor = symbol ^ rem[0];
        rem.copy_within(1.., 0);
        rem[CHECK_SYMBOLS - 1] = 0;
        for (slot, coefficient) in rem.iter_mut().zip(&g[1..]) {
            *slot ^= gf_mul(*coefficient, factor);
        }
    }
    rem
}

/// Whether `symbols` (data then check) vanish at every root of the generator.
fn is_codeword(symbols: &[u8]) -> bool {
    let mut root = 1u8;
    for _ in 0..CHECK_SYMBOLS {
        let syndrome = symbols
            .iter()
            .fold(0u8, |acc, symbol| gf_mul(acc, root) ^ symbol);
        if syndrome != 0 {
            return false;
        }
        root = gf_mul(root, 2);
    }
    true
}

// ---------------------------------------------------------------------------
// hosts and ports
// ---------------------------------------------------------------------------

/// Whether `host` is a host a fleet names: an IP address, or a name of
/// letters, digits, `.`, `-` and `_` (at most 253, not starting with `.` or
/// `-`). Nothing that could carry a control character, a space or a `:` into
/// a line, a file or a screen.
#[must_use]
pub fn is_host(host: &str) -> bool {
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    (1..=253).contains(&host.len())
        && !host.starts_with(['.', '-'])
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// `host` and `port` as one dialable word: `[v6]:port` for an IPv6 address.
#[must_use]
pub fn host_port(host: &str, port: u16) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Read `host[:port]` (`[v6]:port` for an IPv6 address with a port, a bare
/// IPv6 address without one): the port is `default` when none is written.
///
/// # Errors
/// Why it is no host and port.
pub fn parse_host_port(text: &str, default: Option<u16>) -> Result<(String, u16), &'static str> {
    let text = text.trim();
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or("an unclosed `[`")?;
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err("only an IPv6 address goes in `[…]`");
        }
        match after {
            "" => (host, None),
            _ => (
                host,
                Some(
                    after
                        .strip_prefix(':')
                        .ok_or("`]` is followed by no `:port`")?,
                ),
            ),
        }
    } else if text.parse::<std::net::Ipv6Addr>().is_ok() {
        (text, None)
    } else {
        match text.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (text, None),
        }
    };
    if !is_host(host) {
        return Err("a host is an address or a name of letters, digits, `.`, `-` and `_`");
    }
    let port = match port {
        Some(port) => port
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or("a port is 1 to 65535")?,
        None => default.ok_or("no port")?,
    };
    Ok((host.to_owned(), port))
}

// ---------------------------------------------------------------------------
// the book of peers
// ---------------------------------------------------------------------------

/// How a peer came into the book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// It registered with this machine, the fleet's issuer (`aterm fleet join`
    /// there).
    Joined,
    /// This machine joined the fleet it issues.
    Issuer,
    /// A person named it (`aterm fleet peers add`).
    Manual,
    /// A member's copy of an entry of its issuer's book ([`copies_of`]):
    /// kept in [`ISSUER_BOOK_FILE`], never in [`PEERS_FILE`], written only by
    /// a read of that book, replaced whole at each answer, never by hand, and
    /// never passed on.
    IssuerBook,
}

impl Via {
    /// The word the book spells it with.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Issuer => "issuer",
            Self::Manual => "manual",
            Self::IssuerBook => "issuer-book",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        [Self::Joined, Self::Issuer, Self::Manual, Self::IssuerBook]
            .into_iter()
            .find(|via| via.word() == word)
    }

    /// Whether an entry that came in this way is the issuer's own knowledge,
    /// which its book shares ([`serve_book`]): a registration, or a peer its
    /// person named. Never its issuer's entry, nor a copy of another book.
    const fn first_hand(self) -> bool {
        matches!(self, Self::Joined | Self::Manual)
    }
}

/// One peer in the book. The node id is the key; the host is a display label
/// and the name tried when the address no longer answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Its node id; `None` for a peer a person named (or a copy of one).
    pub node: Option<String>,
    /// The node key the book binds that node id to (the module doc's **Node
    /// keys**); `None` for a node an older aterm recorded, bound at its next
    /// registration, and for a peer a person named.
    pub key: Option<NodeKey>,
    /// Where a node bound to no key may bind one, when that is not
    /// [`Peer::addr`]: the address its entry was recorded from before a
    /// registration that proved no key moved it ([`record`]). No such
    /// registration moves it again, so no secret holder moves an older
    /// entry to its own address and then binds its key there. `None` for a
    /// bound node, and where it is `addr` itself.
    pub key_from: Option<IpAddr>,
    /// Its host name ([`is_host`]).
    pub host: String,
    /// The address it was last reached at, or registered from.
    pub addr: Option<IpAddr>,
    /// Its fleet port.
    pub port: u16,
    /// The aterm version it registered with.
    pub version: Option<String>,
    /// When it registered (Unix milliseconds): the last this book heard of it.
    pub seen_ms: Option<u64>,
    /// How it came into the book.
    pub via: Via,
}

impl Peer {
    /// The peer as one line of the book: `key_from=` only where there is one.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "peer node={} key={}{} host={} addr={} port={} version={} seen_ms={} via={}",
            self.node.as_deref().unwrap_or("-"),
            self.key.map_or_else(|| "-".to_owned(), |key| key.hex()),
            self.key_from
                .map_or_else(String::new, |from| format!(" key_from={from}")),
            aterm_codec::pct::encode(&self.host),
            self.addr.map_or_else(|| "-".to_owned(), |a| a.to_string()),
            self.port,
            self.version
                .as_deref()
                .map_or_else(|| "-".to_owned(), aterm_codec::pct::encode),
            self.seen_ms
                .map_or_else(|| "-".to_owned(), |ms| ms.to_string()),
            self.via.word(),
        )
    }

    /// One line of the book as a peer; `None` when it is not one. Unknown
    /// fields are passed over, so a later aterm may add some, and a line with
    /// no `key=` (an older aterm's) binds no key.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let fields = fields(line.strip_prefix("peer ")?);
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| *v)
                .filter(|v| *v != "-")
        };
        let node = match get("node") {
            Some(node) if is_node_id(node) => Some(node.to_owned()),
            Some(_) => return None,
            None => None,
        };
        // A key is bound to a node id: none without one.
        let key = match get("key") {
            Some(key) if node.is_some() => Some(NodeKey::from_hex(key)?),
            Some(_) => return None,
            None => None,
        };
        // Where a key may be bound from is an unbound node's alone.
        let key_from = match get("key_from") {
            Some(from) => Some(from.parse::<IpAddr>().ok()?),
            None => None,
        }
        .filter(|_| node.is_some() && key.is_none());
        let host = aterm_codec::pct::decode(get("host")?);
        if !is_host(&host) {
            return None;
        }
        let addr = match get("addr") {
            Some(addr) => Some(addr.parse().ok()?),
            None => None,
        };
        let port = get("port")?.parse::<u16>().ok().filter(|p| *p != 0)?;
        let version = match get("version") {
            Some(v) => Some(version_word(&aterm_codec::pct::decode(v))?),
            None => None,
        };
        let seen_ms = match get("seen_ms") {
            Some(ms) => Some(ms.parse().ok()?),
            None => None,
        };
        let via = Via::from_word(get("via")?)?;
        if !matches!(via, Via::Manual | Via::IssuerBook) && node.is_none() {
            return None;
        }
        Some(Self {
            node,
            key,
            key_from,
            host,
            addr,
            port,
            version,
            seen_ms,
            via,
        })
    }

    /// Where to dial it, as a person reads it: the address when there is one.
    #[must_use]
    pub fn target(&self) -> String {
        match self.addr {
            Some(addr) => host_port(&addr.to_string(), self.port),
            None => host_port(&self.host, self.port),
        }
    }

    /// Whether `other` is the same place: one port, and one address or one
    /// host name ([`same_host`]: `lab-mini` and `lab-mini.local` are one).
    #[must_use]
    pub fn same_place(&self, other: &Self) -> bool {
        self.port == other.port
            && ((self.addr.is_some() && self.addr == other.addr)
                || same_host(&self.host, &other.host))
    }

    /// Whether `what` names this peer: its node id (or 8 digits or more of
    /// its start), its host name or the host name's first label, its address.
    fn answers_to(&self, what: &str) -> bool {
        let what = what.trim().to_ascii_lowercase();
        let short = self.host.split('.').next().unwrap_or(&self.host);
        self.node
            .as_deref()
            .is_some_and(|node| node == what || (what.len() >= 8 && node.starts_with(&what)))
            || self.host.eq_ignore_ascii_case(&what)
            || short.eq_ignore_ascii_case(&what)
            || self.addr.is_some_and(|addr| addr.to_string() == what)
            || self.target().eq_ignore_ascii_case(&what)
    }
}

/// Whether host names `a` and `b` name one machine: equal but for case, or
/// one a bare name (no dot) that is the other's first label — `lab-mini` is
/// what a person types for the `lab-mini.local` a Mac registers with. Two
/// dotted names (`m7.local`, `m7.lan`) are two machines, and an address is
/// only itself.
fn same_host(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    if a.parse::<IpAddr>().is_ok() || b.parse::<IpAddr>().is_ok() {
        return false;
    }
    let bare_of = |bare: &str, dotted: &str| {
        !bare.contains('.')
            && dotted
                .split_once('.')
                .is_some_and(|(label, _)| label.eq_ignore_ascii_case(bare))
    };
    bare_of(a, b) || bare_of(b, a)
}

/// `version` if it is a version word: 1 to 64 of letters, digits, `.`, `+`,
/// `-` and `_` (`0.95.0+g1a2b3c4`).
fn version_word(version: &str) -> Option<String> {
    ((1..=64).contains(&version.len())
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-' | b'_')))
    .then(|| version.to_owned())
}

/// A record's `key=value` fields, space-separated.
fn fields(rest: &str) -> Vec<(&str, &str)> {
    rest.split(' ')
        .filter_map(|field| field.split_once('='))
        .collect()
}

/// Why the book could not be read.
#[derive(Debug)]
pub enum PeersError {
    /// The file breaks a private file's rules, or could not be read.
    File(PrivateFileError),
    /// This line (1-based) is no peer: the file was edited by hand.
    Line(usize),
}

impl fmt::Display for PeersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(e) => write!(f, "{PEERS_FILE}: {e}"),
            Self::Line(n) => write!(
                f,
                "{PEERS_FILE} line {n} is no peer record; fix it or remove the line"
            ),
        }
    }
}

impl std::error::Error for PeersError {}

/// The book of peers in the state directory `dir`: empty when there is none.
/// A copy of the issuer's book found in it (an aterm built before the copy
/// had a file of its own wrote it there) is passed over: the next read of
/// that book writes it where it belongs, and an edit of the book drops it.
///
/// # Errors
/// The file breaks a private file's rules, or holds a line that is no peer.
pub fn read_peers(dir: &Path) -> Result<Vec<Peer>, PeersError> {
    let bytes = match read_private(dir, PEERS_FILE, PEERS_FILE_MAX) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(Vec::new()),
        Err(e) => return Err(PeersError::File(e)),
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut book = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let peer = Peer::parse(line).ok_or(PeersError::Line(n + 1))?;
        if peer.via != Via::IssuerBook {
            book.push(peer);
        }
    }
    Ok(book)
}

/// The first line of [`ISSUER_BOOK_FILE`]: the node whose book it copies.
const ISSUER_BOOK_HEAD: &str = "issuer node=";

/// Whether `dir` holds a copy of the book `issuer` shared ([`ISSUER_BOOK_FILE`]
/// whose head names that node), however few members it named: the last
/// whole read got that issuer's book (`aterm fleet peers`, `survey --fleet`
/// keep it), where an issuer that shares none — an older aterm — or a
/// member that never read leaves none.
#[must_use]
pub fn holds_issuer_book(dir: &Path, issuer: &str) -> bool {
    read_private(dir, ISSUER_BOOK_FILE, PEERS_FILE_MAX).is_ok_and(|bytes| {
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .and_then(|head| head.strip_prefix(ISSUER_BOOK_HEAD))
            == Some(issuer)
    })
}

/// The member's copy of its issuer's book in `dir` ([`ISSUER_BOOK_FILE`]),
/// when it was read from `issuer`, the node this machine's book names as its
/// issuer: none when the file is missing or copies another node's book —
/// the issuer's entry went, or names another machine now — or cannot be
/// read whole (not private, a line that is no copy). A copy is only ever a
/// cache of the issuer's answer, so none is the safe reading: the next read
/// of the book writes it again.
#[must_use]
pub fn read_issuer_book(dir: &Path, issuer: &str) -> Vec<Peer> {
    let Ok(bytes) = read_private(dir, ISSUER_BOOK_FILE, PEERS_FILE_MAX) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    if lines
        .next()
        .and_then(|head| head.strip_prefix(ISSUER_BOOK_HEAD))
        != Some(issuer)
    {
        return Vec::new();
    }
    let copies: Option<Vec<Peer>> = lines
        .map(|line| Peer::parse(line).filter(|peer| peer.via == Via::IssuerBook))
        .collect();
    let mut copies = copies.unwrap_or_default();
    copies.truncate(PEERS_MAX);
    copies
}

/// Write `copies` as the member's copy of the book of its issuer `issuer`
/// (a node id) in `dir`, whole ([`write_private`]): what [`copies_of`] made
/// of that issuer's answer.
///
/// # Errors
/// `dir` is not private, or the write's own error.
pub fn write_issuer_book(dir: &Path, issuer: &str, copies: &[Peer]) -> io::Result<()> {
    let mut text = format!(
        "# A copy of this machine's fleet issuer's book: written by aterm at each read of that \
         book (`aterm fleet peers`, `aterm ctl survey --fleet`, a drive's check of an old copy), \
         never by hand.\n\
         {ISSUER_BOOK_HEAD}{issuer}\n"
    );
    for copy in copies.iter().filter(|copy| copy.via == Via::IssuerBook) {
        text.push_str(&copy.line());
        text.push('\n');
    }
    write_private(dir, ISSUER_BOOK_FILE, text.as_bytes())
}

/// The book as its file holds it.
fn book_text(book: &[Peer]) -> String {
    let mut text = String::from(
        "# The fleet's peers, one per line: written by aterm (`aterm fleet peers`).\n",
    );
    for peer in book {
        text.push_str(&peer.line());
        text.push('\n');
    }
    text
}

/// Edit the book in `dir` under its lock: read it, run `edit`, and write it
/// back whole when `edit` changed it.
///
/// # Errors
/// `edit`'s refusal, the lock held by another writer past 2 s, a book that
/// cannot be read, or a write's own error — the book as it was in every case.
pub fn update_peers<R>(
    dir: &Path,
    edit: impl FnOnce(&mut Vec<Peer>) -> Result<R, String>,
) -> Result<R, String> {
    let _lock = BookLock::take(dir, LOCK_WAIT).map_err(|e| format!("{PEERS_FILE}'s lock: {e}"))?;
    let mut book = read_peers(dir).map_err(|e| e.to_string())?;
    let before = book.clone();
    let out = edit(&mut book)?;
    if book != before {
        write_private(dir, PEERS_FILE, book_text(&book).as_bytes())
            .map_err(|e| format!("{PEERS_FILE}: {e}"))?;
    }
    Ok(out)
}

/// What [`record`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// A peer the book did not hold.
    New,
    /// A peer it held under that node id, brought up to date.
    Updated,
    /// A person's peer the book already holds at that place.
    Already,
}

/// Put `peer` in `book`: one with a node id replaces the entry with that id
/// (and a person's entry for the place it stands for: for a member's
/// registration the address and port it registered from, never the host name
/// it gives itself; for an issuer its place); one a person named is added
/// unless the book holds that place already ([`Peer::same_place`]).
///
/// No registration takes another node out of the book. One from an address
/// and port the book holds for another node is a second entry beside it: on
/// DHCP the address passed from that machine to this one, and the old entry
/// is how the reader still finds the machine that moved (its address answers
/// as this node, so the reader asks its name, [`survey_all`]) until it joins
/// again from where it is now or a person runs `aterm fleet peers rm`.
///
/// A REGISTRATION OF A KNOWN NODE MUST PROVE ITS KEY ([`bound_as`]): the key
/// its entry is bound to, or none where an older aterm recorded none — bound
/// now, from the address that entry was recorded from. A registration that
/// proves none moves such an entry (an older member's DHCP lease) and never
/// where its key may be bound from ([`Peer::key_from`]): else a secret
/// holder would move it to its own address keyless, then bind its key there.
/// Another key, or none for a bound node, is refused, naming `aterm fleet
/// peers rm <node>` on the issuer. So no secret holder moves a member's
/// entry, or answers as it: a member's reader holds that node to its key
/// ([`Keyring`]).
///
/// Every node registered from one address — stale ones included — counts
/// toward that address's [`JOINED_PER_ADDRESS`], and the share still bounds
/// something keys do not: one secret holder mints a key per made-up node id
/// as freely as the node ids themselves, so without it one address could
/// fill the book's [`PEERS_MAX`] — every later join refused, every member's
/// read spending its deadlines on nodes that never answer. With it, that
/// takes [`PEERS_MAX`] / [`JOINED_PER_ADDRESS`] addresses (more with address
/// aliases on its LAN interface, which a member's read then dials; a node it
/// made up is only ever itself). Never more than [`PEERS_MAX`].
///
/// A copy of the issuer's book ([`Via::IssuerBook`]) is never recorded: only
/// a read of that book writes copies, and in a file of their own
/// ([`write_issuer_book`]).
///
/// # Errors
/// The book is full, or holds its share of the address's registrations; a
/// known node's registration proves another key than its entry binds, or
/// none; or `peer` is a copy of the issuer's book.
pub fn record(book: &mut Vec<Peer>, peer: Peer) -> Result<Recorded, String> {
    let full = || {
        format!("the book holds {PEERS_MAX} peers already: remove one with `aterm fleet peers rm`")
    };
    if peer.via == Via::IssuerBook {
        return Err(
            "a copy of the issuer's book is written by a read of that book, never recorded"
                .to_owned(),
        );
    }
    if peer.via == Via::Manual {
        if book.iter().any(|known| known.same_place(&peer)) {
            return Ok(Recorded::Already);
        }
        if book.len() >= PEERS_MAX {
            return Err(full());
        }
        book.push(peer);
        return Ok(Recorded::New);
    }
    let joined = peer.via == Via::Joined;
    if joined
        && let Some(known) = book
            .iter()
            .find(|known| known.node.is_some() && known.node == peer.node)
    {
        bound_as(known, &peer)?;
    }
    let at_addr = |known: &Peer| known.addr.is_some() && known.addr == peer.addr;
    // The address's share: every other node registered from it, the entry
    // this one brings up to date aside.
    if joined
        && book
            .iter()
            .filter(|known| known.via == Via::Joined && at_addr(known) && known.node != peer.node)
            .count()
            >= JOINED_PER_ADDRESS
    {
        return Err(format!(
            "{} registered {JOINED_PER_ADDRESS} members already: remove one with `aterm fleet \
             peers rm`",
            peer.addr
                .map_or_else(|| "its address".to_owned(), |a| a.to_string())
        ));
    }
    book.retain(|known| {
        let stands_for = if joined {
            known.port == peer.port && at_addr(known)
        } else {
            known.same_place(&peer)
        };
        !(known.via == Via::Manual && stands_for)
    });
    if let Some(known) = book
        .iter_mut()
        .find(|known| known.node.is_some() && known.node == peer.node)
    {
        // A registration that proves no key, of a node bound to none, keeps
        // where that node's key may be bound from.
        let key_from = (joined && known.key.is_none() && peer.key.is_none())
            .then(|| known.key_from.or(known.addr))
            .flatten()
            .filter(|from| Some(*from) != peer.addr);
        *known = Peer { key_from, ..peer };
        return Ok(Recorded::Updated);
    }
    if book.len() >= PEERS_MAX {
        return Err(full());
    }
    book.push(peer);
    Ok(Recorded::New)
}

/// Whether `peer`, a registration of the node `known` is the book's entry
/// of, may stand for it: it proves the key `known` is bound to; or `known`
/// is bound to none (an older aterm recorded it), and `peer` proves none
/// either, or a key from the address `known` was recorded from
/// ([`Peer::key_from`], else its address) — what an older entry is upgraded
/// by. A key bound from anywhere else would be whoever asked first:
/// refused, as another key is, in words of their own ([`BEFORE_KEYS`]).
///
/// # Errors
/// Why not, naming `aterm fleet peers rm <node>` on the issuer: what lets
/// that node register afresh (a machine that lost its node key, an entry
/// whose machine moved before its key was bound).
fn bound_as(known: &Peer, peer: &Peer) -> Result<(), String> {
    let node = peer.node.as_deref().unwrap_or("-");
    let node = node.get(..8).unwrap_or(node);
    let rm = format!("`aterm fleet peers rm {node}` {REBIND}");
    match (known.key, peer.key) {
        (Some(bound), Some(proved)) if bound == proved => Ok(()),
        (Some(_), Some(_)) => Err(format!(
            "node {node} is bound to another key in the issuer's book: not that node, or one \
             that lost its node key — then {rm}"
        )),
        (Some(_), None) => Err(format!(
            "node {node} is bound to a key in the issuer's book, and this registration proves \
             none (an older aterm?): update aterm there, or {rm}"
        )),
        (None, Some(_)) => match known.key_from.or(known.addr) {
            Some(from) if Some(from) != peer.addr => Err(format!(
                "node {node} was recorded from {from} {BEFORE_KEYS}, and its key is bound only \
                 from there: {rm}"
            )),
            _ => Ok(()),
        },
        (None, None) => Ok(()),
    }
}

/// How each refusal of a registration by its node's key ends ([`record`]),
/// after the `aterm fleet peers rm <node>` that lets the node register
/// afresh: what a member's `join` reads such a refusal by.
pub const REBIND: &str = "on the issuer, and join again";

/// What the refusal of a key from another address than the one an entry
/// was recorded from before keys came says, and no other refusal does
/// ([`bound_as`]): that no key is bound to the node, so a member's `join`
/// does not read it as one bound to another key.
pub const BEFORE_KEYS: &str = "before keys came";

/// Where in `peers` the one peer `what` names is ([`Peer::answers_to`]):
/// `None` when no peer answers to it.
///
/// # Errors
/// More than one does: each is named, so the person can pick one by its node
/// id or its address.
pub fn find(peers: &[Peer], what: &str) -> Result<Option<usize>, String> {
    let shown = aterm_codec::tty::one_line(what, 80);
    let matches: Vec<usize> = peers
        .iter()
        .enumerate()
        .filter(|(_, peer)| peer.answers_to(what))
        .map(|(at, _)| at)
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [at] => Ok(Some(*at)),
        many => Err(format!(
            "`{shown}` names {} peers ({}): name one by its node id or its address and port",
            many.len(),
            many.iter()
                .filter_map(|at| peers.get(*at))
                .map(|peer| format!(
                    "{} at {}",
                    peer.node
                        .as_deref()
                        .map_or("-", |n| n.get(..8).unwrap_or(n)),
                    peer.target()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Take the one peer `what` names out of `book` ([`find`]). A peer only
/// `copies` holds — this machine's copy of its issuer's book, which stands
/// only while `book` names that issuer ([`read_issuer_book`]) — is never
/// taken out by hand: the next read of that book would put it back, so it
/// is refused, naming where it is removed.
///
/// # Errors
/// No peer answers to it, more than one does (each is named, so the person
/// can pick one by its node id), or only a copy of the issuer's book does.
pub fn remove(book: &mut Vec<Peer>, copies: &[Peer], what: &str) -> Result<Peer, String> {
    let shown = aterm_codec::tty::one_line(what, 80);
    match find(book, what)? {
        Some(at) => Ok(book.remove(at)),
        None if !matches!(find(copies, what), Ok(None)) => Err(format!(
            "`{shown}` is in the fleet issuer's book, which this machine copies at each read: \
             `aterm fleet peers rm` on the issuer takes it out, and this copy follows"
        )),
        None => Err(format!("no peer is `{shown}`")),
    }
}

// ---------------------------------------------------------------------------
// node keys: which node a peer is
// ---------------------------------------------------------------------------

/// The node keys this machine binds, node id to key (the module doc's **Node
/// keys**): its book's — on the issuer every member that registered since
/// keys came, on a member its issuer — then a member's copy of its issuer's
/// book's. Each node id once, the book's key over the copy's. Gathered from
/// every entry, never from a read's targets: a person's entry at a copy's
/// place is dialled in the copy's stead ([`with_copies`]), and whatever
/// answers it as that copy's node is still held to the copy's key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keyring {
    /// The book's.
    own: Vec<(String, NodeKey)>,
    /// The copy of the issuer's book's: what a fresh read of that book
    /// replaces whole ([`Keyring::with_book`]).
    copied: Vec<(String, NodeKey)>,
    /// This machine's issuer (its book's first `via=issuer` node): a key it
    /// no longer proves is taken out here, never on the issuer, whose book
    /// never holds itself ([`Unreached::Impostor`]).
    issuer: Option<String>,
}

impl Keyring {
    /// The keys `peers` bind — a copy of the issuer's book's
    /// ([`Via::IssuerBook`]) under every other entry's, wherever it stands —
    /// and the issuer they name.
    #[must_use]
    pub fn of<'a>(peers: impl IntoIterator<Item = &'a Peer>) -> Self {
        let mut ring = Self::default();
        for peer in peers {
            if peer.via == Via::Issuer && ring.issuer.is_none() {
                ring.issuer.clone_from(&peer.node);
            }
            if let (Some(node), Some(key)) = (&peer.node, peer.key) {
                let tier = if peer.via == Via::IssuerBook {
                    &mut ring.copied
                } else {
                    &mut ring.own
                };
                if !tier.iter().any(|(bound, _)| bound == node) {
                    tier.push((node.clone(), key));
                }
            }
        }
        ring
    }

    /// The keys this machine binds, as the state directory `dir` holds them:
    /// its book, then its copy of its issuer's book ([`read_issuer_book`]).
    /// A book that cannot be read binds nothing: every peer then reads as
    /// unproven, never as proven.
    #[must_use]
    pub fn read(dir: &Path) -> Self {
        let (peers, copies) = book_and_copies(dir);
        Self::of(peers.iter().chain(&copies))
    }

    /// Whether the key `node` is bound to is the copy's alone: no entry of
    /// this machine's own book binds it, so it stands only as of the last
    /// read of the issuer's book ([`proven_driver`]).
    fn copied_alone(&self, node: &str) -> bool {
        !self.own.iter().any(|(bound, _)| bound == node)
            && self.copied.iter().any(|(bound, _)| bound == node)
    }

    /// These keys once `book`, a fresh read of the issuer's book, is the
    /// copy: the copy's replaced whole by the book's ([`copies_of`], none
    /// from a book its issuer did not prove), so a member the issuer took
    /// out and bound afresh is held to its new key at once.
    #[must_use]
    pub fn with_book(&self, book: &Book, own: Option<&str>) -> Self {
        Self {
            copied: Self::of(&copies_of(book, own)).copied,
            ..self.clone()
        }
    }

    /// The key `node` is bound to, if any.
    #[must_use]
    pub fn bound(&self, node: &str) -> Option<NodeKey> {
        self.own
            .iter()
            .chain(&self.copied)
            .find(|(bound, _)| bound == node)
            .map(|(_, key)| *key)
    }

    /// Whether `node` is this machine's issuer.
    fn is_issuer(&self, node: &str) -> bool {
        self.issuer.as_deref() == Some(node)
    }

    /// THE PROVEN NODE behind `proof`, what a connection's peer proved
    /// ([`Served::peer_node`], [`Reached::peer`]): the node id it named, when
    /// this keyring binds that id to the key it proved. A claim is never an
    /// identity: no proof, a node bound to no key, and another key are each
    /// [`Unproven`].
    ///
    /// # Errors
    /// [`Unproven`]: why the peer is no node this machine can name.
    pub fn proven(&self, proof: Option<&NodeProof>) -> Result<String, Unproven> {
        let proof = proof.ok_or(Unproven::NoProof)?;
        match self.bound(&proof.node) {
            Some(key) if key == proof.key => Ok(proof.node.clone()),
            Some(_) => Err(Unproven::OtherKey(proof.node.clone())),
            None => Err(Unproven::Unbound(proof.node.clone())),
        }
    }
}

/// Why a peer is no node this machine can name ([`Keyring::proven`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unproven {
    /// It proved no node: an older aterm, or one that holds no node key.
    NoProof,
    /// It proved a key for this node id, which this machine binds no key to:
    /// an entry an older aterm recorded, a node this machine never heard of,
    /// or a peer named by hand.
    Unbound(String),
    /// It claimed this node id and proved another key than the one this
    /// machine binds it to: not that node.
    OtherKey(String),
}

impl fmt::Display for Unproven {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = |node: &str| node.get(..8).unwrap_or(node).to_owned();
        match self {
            Self::NoProof => {
                f.write_str("the peer proved no node (an older aterm, or one with no node key)")
            }
            Self::Unbound(node) => write!(
                f,
                "node {} is bound to no key here: it is bound when it joins the issuer again",
                short(node)
            ),
            Self::OtherKey(node) => write!(
                f,
                "the peer claims node {} and proved another key than this machine binds to it",
                short(node)
            ),
        }
    }
}

impl std::error::Error for Unproven {}

/// THE PROVEN NODE of the peer of a fleet connection this machine's listener
/// serves, for an authorization that names nodes (a remote step-in): the
/// node `served`'s peer proved ([`Served::peer_node`]) when this machine's
/// book in `state_dir` binds that node to the key it proved
/// ([`Keyring::read`], [`Keyring::proven`]). Read fresh on each call: on the
/// issuer a `peers rm` or a join counts at once; on a member, what its copy
/// of the issuer's book binds is as of its last read of that book (`aterm
/// fleet peers`, `aterm ctl survey --fleet`).
///
/// # Errors
/// [`Unproven`]: the peer proved no node, or a node this machine binds no
/// key or another key to.
pub fn proven_peer(served: &Served, state_dir: &Path) -> Result<String, Unproven> {
    Keyring::read(state_dir).proven(served.peer_node())
}

/// Why a peer may not drive this machine ([`Keyring::driver`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Undriven {
    /// It is no node this machine can name ([`Keyring::proven`]).
    Unproven(Unproven),
    /// No grant names its node.
    Unlisted(String),
    /// A grant names its node under another key than the one it proved: the
    /// node id was bound afresh since the person here let it drive.
    Rekeyed(String),
    /// Only this machine's copy of its issuer's book binds its node, that
    /// copy is older than [`COPY_DRIVES_FOR`], and the issuer's book could
    /// not be read again to check it ([`proven_driver`]).
    Unchecked {
        /// The node.
        node: String,
        /// Why the issuer's book was not read.
        why: String,
    },
}

impl fmt::Display for Undriven {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = |node: &str| node.get(..8).unwrap_or(node).to_owned();
        match self {
            Self::Unproven(why) => write!(f, "{why}"),
            Self::Unlisted(node) => write!(
                f,
                "node {} is not one this Mac lets drive it (aterm fleet allow drive --from, \
                 there)",
                short(node)
            ),
            Self::Rekeyed(node) => write!(
                f,
                "node {} proved another key than the one it held when this Mac's person let it \
                 drive: it drives again only once they run aterm fleet allow drive --from, there",
                short(node)
            ),
            Self::Unchecked { node, why } => write!(
                f,
                "node {} is bound here only by this Mac's copy of its issuer's book, which is \
                 over {} s old, and this Mac could not read that book again to check it ({}): it \
                 drives again once this Mac reaches its issuer (aterm fleet peers, there, says why \
                 it does not)",
                short(node),
                COPY_DRIVES_FOR.as_secs(),
                aterm_codec::tty::one_line(why, 160)
            ),
        }
    }
}

impl std::error::Error for Undriven {}

impl Keyring {
    /// THE DRIVER behind `proof`: the node it proved under the key this
    /// keyring binds it to ([`Self::proven`]), when `grants` names that node
    /// with that same key. The book's binding and the grant's pin must both
    /// hold: a `peers rm` here counts at once, and a node id the book has
    /// bound to another key since the grant drives nothing.
    ///
    /// # Errors
    /// [`Undriven`]: why the peer may not drive.
    pub fn driver(
        &self,
        proof: Option<&NodeProof>,
        grants: &[DriveGrant],
    ) -> Result<String, Undriven> {
        let node = self.proven(proof).map_err(Undriven::Unproven)?;
        match grants.iter().find(|grant| grant.node == node) {
            None => Err(Undriven::Unlisted(node)),
            Some(grant) if proof.is_some_and(|proof| proof.key == grant.key) => Ok(node),
            Some(_) => Err(Undriven::Rekeyed(node)),
        }
    }
}

/// HOW LONG A MEMBER'S COPY OF ITS ISSUER'S BOOK SPEAKS FOR THE ISSUER WHEN
/// A DRIVE IS ADMITTED ([`proven_driver`]): one minute since the copy was
/// written. A driver whose key only that copy binds is admitted against a
/// copy this young; past it the issuer's book is read again first, so the
/// issuer's `peers rm <node>` stops that node driving its members within a
/// minute (and a drive's connection it already holds, at most
/// [`crate::fleet::SERVED_WITHIN`] more), whether or not anyone on a member
/// reads the fleet. A minute is what a person reads as "at once" for an
/// eviction made on another Mac, while each verb of a drive is a connection
/// of its own: a bound much shorter would dial the issuer on nearly every
/// one, from every member a Mac drives.
pub const COPY_DRIVES_FOR: Duration = Duration::from_secs(60);

/// The one deadline of the read of the issuer's book a drive's admission
/// makes ([`proven_driver`]): a fleet read's own default (3 s), inside what
/// the drive's connection leaves — it lasts [`crate::fleet::SERVED_WITHIN`]
/// (30 s), and a turn over it at most 22 s of that.
pub const RECHECK_WITHIN: Duration = Duration::from_secs(3);

/// THE DRIVER of a fleet connection this machine's listener serves:
/// [`Keyring::driver`] over the book in `state_dir`, read fresh on each call
/// ([`Keyring::read`]) — on the issuer a `peers rm` or a join counts at once.
///
/// REVOCATION IS FRESH ON A MEMBER TOO. A node bound only by the member's
/// copy of its issuer's book is admitted against that copy while it is
/// younger than [`COPY_DRIVES_FOR`]; an older copy is checked first by
/// reading the issuer's book again, proving `secret` and this machine's node
/// `me`, within [`RECHECK_WITHIN`]. A book the issuer proved replaces the
/// copy ([`write_issuer_book`]) and judges the driver; no book, or one the
/// issuer did not prove, refuses it ([`Undriven::Unchecked`]): an evicted
/// node never drives on because its issuer went dark. A driver this
/// machine's own book binds — its issuer, recorded at its join — is judged
/// by that book alone, as on the issuer.
///
/// # Errors
/// As [`Keyring::driver`], and [`Undriven::Unchecked`].
pub fn proven_driver(
    served: &Served,
    state_dir: &Path,
    grants: &[DriveGrant],
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
) -> Result<String, Undriven> {
    driver_as_of(
        served.peer_node(),
        (state_dir, grants),
        std::time::SystemTime::now(),
        |issuer| {
            fetch_book(
                issuer,
                (secret, me),
                (Instant::now() + RECHECK_WITHIN, RECHECK_WITHIN),
            )
        },
    )
}

/// The driver of a fleet connection as this machine's book and its copy of
/// its issuer's book hold it now, the copy however old ([`Keyring::driver`]
/// over [`Keyring::read`]): what a survey tells its reader about driving
/// here, never what admits a drive ([`proven_driver`]).
///
/// # Errors
/// As [`Keyring::driver`].
pub fn listed_driver(
    served: &Served,
    state_dir: &Path,
    grants: &[DriveGrant],
) -> Result<String, Undriven> {
    Keyring::read(state_dir).driver(served.peer_node(), grants)
}

/// [`proven_driver`] of `proof` as of `now`, the issuer's book read with
/// `fetch` when it must be.
fn driver_as_of(
    proof: Option<&NodeProof>,
    (state_dir, grants): (&Path, &[DriveGrant]),
    now: std::time::SystemTime,
    fetch: impl FnOnce(&Target) -> Result<Book, BookUnread>,
) -> Result<String, Undriven> {
    // Its age is read before the copy: a copy written in between is read
    // with the older time, and checked once more than it needs.
    let written = issuer_book_written(state_dir);
    let (peers, copies) = book_and_copies(state_dir);
    let keys = Keyring::of(peers.iter().chain(&copies));
    let node = keys.driver(proof, grants)?;
    let fresh = written
        .and_then(|written| now.duration_since(written).ok())
        .is_some_and(|age| age <= COPY_DRIVES_FOR);
    if fresh || !keys.copied_alone(&node) {
        return Ok(node);
    }
    let unchecked = |why: String| Undriven::Unchecked {
        node: node.clone(),
        why,
    };
    // A copy is read only beside the issuer it came from.
    let issuer = issuer_of(&peers).ok_or_else(|| unchecked("this Mac names no issuer".into()))?;
    let book = fetch(&target_of(issuer)).map_err(|why| unchecked(unread_words(&why)))?;
    if !book.proven {
        return Err(unchecked(
            "this Mac binds no key to its issuer, so its book binds none".into(),
        ));
    }
    let own = read_node_id(state_dir).ok().flatten();
    // Best effort: the next admission past the bound reads it again.
    let _ = write_issuer_book(state_dir, &book.node, &copies_of(&book, own.as_deref()));
    keys.with_book(&book, own.as_deref()).driver(proof, grants)
}

/// When this machine's copy of its issuer's book was written ([`ISSUER_BOOK_FILE`]'s
/// modification time; each write is a whole new file), if it can be told.
fn issuer_book_written(state_dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::symlink_metadata(state_dir.join(ISSUER_BOOK_FILE))
        .and_then(|meta| meta.modified())
        .ok()
}

/// Why a read of the issuer's book brought none, as a refusal says it.
fn unread_words(why: &BookUnread) -> String {
    let short = |node: &str| node.get(..8).unwrap_or(node).to_owned();
    match why {
        BookUnread::Older(at) => {
            format!("the issuer at {at} runs an older aterm, which shares no book")
        }
        BookUnread::NoBook { at, why, .. } => format!("the machine at {at} shares no book: {why}"),
        BookUnread::NotNow { at, why, .. } => {
            format!("the issuer at {at} cannot share its book now: {why}")
        }
        BookUnread::OtherNode(at, node) => {
            format!(
                "{at} answered as node {}, not this Mac's issuer",
                short(node)
            )
        }
        BookUnread::Unreached(unreached) => unreached.to_string(),
    }
}

/// This machine's issuer in `book`: its first `via=issuer` entry.
fn issuer_of(book: &[Peer]) -> Option<&Peer> {
    book.iter().find(|peer| peer.via == Via::Issuer)
}

/// This machine's book in `dir` and its copy of its issuer's book
/// ([`read_issuer_book`]), each whole: a book that cannot be read is none.
fn book_and_copies(dir: &Path) -> (Vec<Peer>, Vec<Peer>) {
    let peers = read_peers(dir).unwrap_or_default();
    let copies = issuer_of(&peers)
        .and_then(|issuer| issuer.node.as_deref())
        .map(|issuer| read_issuer_book(dir, issuer))
        .unwrap_or_default();
    (peers, copies)
}

/// The entry this machine's book in `state_dir`, or its copy of its
/// issuer's book, holds for `node` ([`Keyring::read`]'s order): how a node a
/// connection proved is named to a person (its host). `None` where neither
/// names it, or the book cannot be read.
#[must_use]
pub fn peer_of(state_dir: &Path, node: &str) -> Option<Peer> {
    let (mut peers, copies) = book_and_copies(state_dir);
    with_copies(&mut peers, copies);
    peers
        .into_iter()
        .find(|peer| peer.node.as_deref() == Some(node))
}

/// THE PEERS A KEY BINDS, as this machine's keys are gathered
/// ([`Keyring::read`]): each entry of its book in `state_dir`, then of its
/// WHOLE copy of its issuer's book, that names a node with the key the
/// keyring binds it to — each node once, its book's entry first. No copy is
/// left out for a person's entry at its place, as a read leaves it out
/// ([`with_copies`]): what names a node to grant it a drive or to drive it
/// resolves here ([`find`]), so a hand-named peer never hides the node the
/// drive's admission knows by its key.
#[must_use]
pub fn bound_peers(state_dir: &Path) -> Vec<Peer> {
    let (peers, copies) = book_and_copies(state_dir);
    let keys = Keyring::of(peers.iter().chain(&copies));
    let mut bound: Vec<Peer> = Vec::new();
    for peer in peers.into_iter().chain(copies) {
        let Some(node) = peer.node.as_deref() else {
            continue;
        };
        if peer.key.is_some()
            && keys.bound(node) == peer.key
            && !bound.iter().any(|held| held.node.as_deref() == Some(node))
        {
            bound.push(peer);
        }
    }
    bound
}

/// The membership write lock, `.fleet-peers.lock` beside the book: opened without
/// following a link or waiting on a FIFO, `flock` exclusive, released by
/// `LOCK_UN` before the close (`clippy.toml`'s flock release rule).
struct BookLock(std::fs::File);

impl BookLock {
    fn take(dir: &Path, within: Duration) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        check_private_dir(dir)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(dir.join(PEERS_LOCK))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the lock is no regular file",
            ));
        }
        let deadline = Instant::now() + within;
        let mut backoff = Duration::from_millis(1);
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(50));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "another aterm is writing the book; run this again",
                    ));
                }
            }
        }
    }
}

impl Drop for BookLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

// ---------------------------------------------------------------------------
// the join exchange
// ---------------------------------------------------------------------------

/// What a member tells the issuer it joins (the module doc's wire).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    /// The member's node id ([`node_id`]).
    pub node: String,
    /// The member's host name.
    pub host: String,
    /// The member's fleet port.
    pub port: u16,
    /// The member's aterm version.
    pub version: String,
    /// The member's node key ([`node_identity`]), which the connection it is
    /// sent on proves it holds; `None` from an older aterm.
    pub key: Option<NodeKey>,
}

impl Registration {
    /// The registration as its line, newline included: `key=` last, which
    /// an older issuer passes over.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "register node={} host={} port={} version={}{}\n",
            self.node,
            aterm_codec::pct::encode(&self.host),
            self.port,
            aterm_codec::pct::encode(&self.version),
            self.key
                .map_or_else(String::new, |key| format!(" key={}", key.hex())),
        )
    }

    /// A registration line, its newline dropped.
    ///
    /// # Errors
    /// Why it is none.
    pub fn parse(line: &str) -> Result<Self, &'static str> {
        let rest = line
            .strip_prefix("register ")
            .ok_or("a join sends `register node= host= port= version=`")?;
        let fields = fields(rest);
        let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        let node = get("node")
            .filter(|node| is_node_id(node))
            .ok_or("node= is 32 lowercase hex digits")?;
        let host = aterm_codec::pct::decode(get("host").ok_or("no host=")?);
        if !is_host(&host) {
            return Err("host= is an address or a name of letters, digits, `.`, `-` and `_`");
        }
        let port = get("port")
            .and_then(|port| port.parse::<u16>().ok())
            .filter(|port| *port != 0)
            .ok_or("port= is 1 to 65535")?;
        let version = version_word(&aterm_codec::pct::decode(
            get("version").ok_or("no version=")?,
        ))
        .ok_or("version= is a version word")?;
        let key = get("key")
            .map(|key| NodeKey::from_hex(key).ok_or("key= is 64 lowercase hex digits"))
            .transpose()?;
        Ok(Self {
            node: node.to_owned(),
            host,
            port,
            version,
            key,
        })
    }
}

/// The issuer, as it names itself in its answer to a join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issuer {
    /// Its node id.
    pub node: String,
    /// Its host name.
    pub host: String,
    /// Its fleet port.
    pub port: u16,
    /// Its aterm version.
    pub version: String,
}

/// What a join's registration came to: the issuer, and how many peers its
/// book holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Joined {
    /// The issuer.
    pub issuer: Issuer,
    /// Its book's size, this member included.
    pub peers: usize,
}

/// Read one line of at most [`RECORD_MAX`] bytes, its newline dropped.
fn read_record<S: Read>(stream: &mut S) -> io::Result<String> {
    let mut line = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the line ended before its newline",
                ));
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                if line.len() >= RECORD_MAX {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "a line longer than a record",
                    ));
                }
                line.push(byte[0]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    String::from_utf8(line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a line that is not UTF-8"))
}

/// The refusal a machine that holds no join code answers a `join` with
/// ([`serve_join`]): its `aterm fleet join` names it as no issuer.
pub const NOT_THE_ISSUER: &str = "this machine is no fleet's issuer: it holds no join code, so \
                                  it records no members (join at the machine whose `aterm fleet \
                                  init` wrote the code)";

/// The issuer's end of a join, on a stream the handshake proved for op
/// `join`: read the member's [`Registration`], record it in the book in
/// `state_dir` with the address `from` it came from and the key it names —
/// which `proof`, the node the connection's dialer proved
/// ([`Served::peer_node`]), must be: that node and that key, or the record is
/// refused — and answer — `OK` naming
/// the issuer `issuer` gives and the book's size, or `ERR <why>` (a malformed
/// record, a key the connection did not prove, a known node's registration
/// that proves another key than the book binds or none ([`record`]), the
/// issuer's own node id, a full or unreadable book, an issuer
/// that cannot name itself — no node id, or a host that is no [`is_host`],
/// which the member's [`register`] would refuse after the book recorded it —
/// or a machine that is no fleet's issuer: one whose
/// `state_dir` holds no [`CODE_FILE`], so that a member's book holds only what
/// its own `join` and `peers add` put there). `issuer` is asked only on an
/// issuer, after that check, so a `join` at a member reads and writes nothing
/// of its state. Bound the stream's reads before calling: nothing here waits
/// on its own clock.
///
/// Returns what the answer said, for the caller's audit trail: what the book
/// did and the peer as it recorded it — its node, host, and FLEET address
/// (the address the record came from with the fleet port it names, not the
/// connection's source port) — or the refusal (the words after `ERR`).
///
/// # Errors
/// The stream's own error: the record could not be read, or the answer not
/// written.
pub fn serve_join<S: Read + Write>(
    stream: &mut S,
    (from, proof): (IpAddr, Option<&NodeProof>),
    state_dir: &Path,
    issuer: impl FnOnce() -> Result<Issuer, String>,
    now_ms: u64,
) -> io::Result<Result<(Recorded, Peer), String>> {
    let line = read_record(stream)?;
    let issues = !matches!(
        std::fs::symlink_metadata(state_dir.join(CODE_FILE)),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    );
    let outcome = match Registration::parse(&line) {
        Err(why) => Err(why.to_owned()),
        Ok(_) if !issues => Err(NOT_THE_ISSUER.to_owned()),
        Ok(reg)
            if reg.key.is_some_and(|key| {
                proof.is_none_or(|proof| proof.node != reg.node || proof.key != key)
            }) =>
        {
            Err(
                "the registration names a node key this connection did not prove: a member \
                 proves its key as it dials (`aterm fleet join` does)"
                    .to_owned(),
            )
        }
        Ok(reg) => issuer()
            .and_then(|issuer| {
                if is_host(&issuer.host) {
                    Ok(issuer)
                } else {
                    Err("its host name is no host a member can dial".to_owned())
                }
            })
            .map_err(|why| format!("the issuer cannot name itself: {why}"))
            .and_then(|issuer| {
                if reg.node == issuer.node {
                    return Err(
                        "that node is this machine, the fleet's issuer: it does not join itself"
                            .to_owned(),
                    );
                }
                let peer = Peer {
                    node: Some(reg.node),
                    key: reg.key,
                    key_from: None,
                    host: reg.host,
                    addr: Some(from.to_canonical()),
                    port: reg.port,
                    version: Some(reg.version),
                    seen_ms: Some(now_ms),
                    via: Via::Joined,
                };
                let recorded_as = peer.clone();
                update_peers(state_dir, |book| {
                    record(book, peer).map(|recorded| (recorded, book.len()))
                })
                .map(|(recorded, peers)| (recorded, peers, issuer, recorded_as))
            }),
    };
    let (answer, outcome) = match outcome {
        Ok((recorded, peers, issuer, recorded_as)) => (
            format!(
                "OK node={} host={} port={} version={} peers={peers}",
                issuer.node,
                aterm_codec::pct::encode(&issuer.host),
                issuer.port,
                aterm_codec::pct::encode(&issuer.version),
            ),
            Ok((recorded, recorded_as)),
        ),
        Err(why) => {
            let why = aterm_codec::tty::one_line(&why, 400);
            (format!("ERR {why}"), Err(why))
        }
    };
    stream.write_all(format!("{answer}\n").as_bytes())?;
    stream.flush()?;
    Ok(outcome)
}

/// The member's end of a join, on a stream the handshake proved for op
/// `join`: send `reg`, read the issuer's answer. Bound the stream's reads
/// before calling.
///
/// # Errors
/// The issuer's refusal (its words), or why its answer never came or was no
/// answer.
pub fn register<S: Read + Write>(stream: &mut S, reg: &Registration) -> Result<Joined, String> {
    stream
        .write_all(reg.line().as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| format!("the registration could not be sent: {e}"))?;
    let line = read_record(stream).map_err(|e| format!("the issuer did not answer: {e}"))?;
    if let Some(why) = line.strip_prefix("ERR ") {
        return Err(format!(
            "the issuer refused: {}",
            aterm_codec::tty::one_line(why, 400)
        ));
    }
    let not_an_answer = || "the issuer's answer is no join answer (an older aterm?)".to_owned();
    let fields = fields(line.strip_prefix("OK ").ok_or_else(not_an_answer)?);
    let get = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    let node = get("node")
        .filter(|node| is_node_id(node))
        .ok_or_else(not_an_answer)?;
    let host = aterm_codec::pct::decode(get("host").ok_or_else(not_an_answer)?);
    let port = get("port")
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(not_an_answer)?;
    let version = version_word(&aterm_codec::pct::decode(
        get("version").ok_or_else(not_an_answer)?,
    ))
    .ok_or_else(not_an_answer)?;
    let peers = get("peers")
        .and_then(|n| n.parse().ok())
        .ok_or_else(not_an_answer)?;
    if !is_host(&host) {
        return Err(not_an_answer());
    }
    Ok(Joined {
        issuer: Issuer {
            node: node.to_owned(),
            host,
            port,
            version,
        },
        peers,
    })
}

/// [`register`] on a peer `reached` for op `join`, the whole exchange by
/// `deadline`: each socket read and write bounded by what is left of it, and
/// the exchange under a [`HandshakeWatchdog`] that shuts the socket at it —
/// rustls reads a whole TLS record before a read returns, so an issuer that
/// sends its answer a byte at a time, each just inside a per-read timeout,
/// would otherwise hold the join for as long as it liked. An answer that came
/// whole as the deadline did is kept.
///
/// # Errors
/// [`register`]'s, or the deadline's: `the issuer did not answer within …`.
pub fn register_by(
    reached: &mut Reached,
    reg: &Registration,
    deadline: Instant,
) -> Result<Joined, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    let late = || {
        format!(
            "the issuer did not answer within {:.1}s",
            left.as_secs_f64()
        )
    };
    if left.is_zero() {
        return Err(late());
    }
    let tcp = reached.transport.stream().get_ref();
    tcp.set_read_timeout(Some(left))
        .and_then(|()| tcp.set_write_timeout(Some(left)))
        .map_err(|e| format!("could not bound the registration: {e}"))?;
    let watchdog = HandshakeWatchdog::arm(tcp, left)
        .map_err(|e| format!("could not bound the registration: {e}"))?;
    let joined = register(reached.transport.stream(), reg);
    match (watchdog.finish(), joined) {
        (_, Ok(joined)) => Ok(joined),
        (Claim::Authed, Err(why)) => Err(why),
        (_, Err(_)) => Err(late()),
    }
}

// ---------------------------------------------------------------------------
// the issuer's book, read by a member
// ---------------------------------------------------------------------------

/// The refusal a machine that holds no join code answers a `book` read with
/// ([`serve_book`]): it keeps no roster of the fleet.
pub const NO_BOOK: &str = "this machine is no fleet's issuer: it holds no join code, so it \
                           keeps no book of the fleet's members to share";

/// The word of a book read's `ERR` from a machine that holds no join code:
/// it has no book to share, so the member's copy of its issuer's book goes
/// when it is the issuer that says so ([`read_whole`]).
pub const BOOK_NONE: &str = "no-book";

/// The word of a book read's `ERR` from an issuer that could not share its
/// book this time (it cannot read it, or cannot name itself): the member
/// keeps its copy.
pub const BOOK_NOT_NOW: &str = "not-now";

/// The listener's end of a book read, on a stream the handshake proved for
/// op `book`: answered at once, no request read. On the issuer — its
/// `state_dir` holds a [`CODE_FILE`] — `OK <n> node=<its node id>` and its
/// book's first-hand entries, one [`Peer::line`] each, at most
/// [`PEERS_MAX`]: the members that registered with it and the peers its
/// person named (`via=joined`, `via=manual`), never a second-hand entry, so
/// a book travels one hop. Its `[[fleet.peers]]` in aterm.toml is that
/// machine's own reading list, not the fleet's roster, and is not shared.
/// On any other machine `ERR no-book node=<its node id> <`[`NO_BOOK`]`>`
/// ([`BOOK_NONE`]); an issuer that cannot name itself (`node` fails) or
/// read its book answers `ERR not-now` ([`BOOK_NOT_NOW`]). Each `ERR` names
/// the node when `node` gives one, so a member can tell its issuer's
/// refusal from another machine's that holds the issuer's old address. The
/// same at every level: the book is facts. Bound the stream's writes before
/// calling.
///
/// Returns how many entries it shared, or why it refused, for the caller's
/// audit trail.
///
/// # Errors
/// The stream's own error: the answer could not be written.
pub fn serve_book<S: Write>(
    stream: &mut S,
    state_dir: &Path,
    node: impl FnOnce() -> Result<String, String>,
) -> io::Result<Result<usize, String>> {
    let issues = !matches!(
        std::fs::symlink_metadata(state_dir.join(CODE_FILE)),
        Err(e) if e.kind() == io::ErrorKind::NotFound
    );
    let node = node().and_then(|node| {
        if is_node_id(&node) {
            Ok(node)
        } else {
            Err("its node id is no node id".to_owned())
        }
    });
    let shared = match (issues, &node) {
        (false, _) => Err((BOOK_NONE, NO_BOOK.to_owned())),
        (true, Err(why)) => Err((
            BOOK_NOT_NOW,
            format!("the issuer cannot name itself: {why}"),
        )),
        (true, Ok(_)) => read_peers(state_dir).map_err(|e| {
            (
                BOOK_NOT_NOW,
                format!("the issuer cannot read its book: {e}"),
            )
        }),
    };
    let named = node
        .as_ref()
        .map_or_else(|_| String::new(), |node| format!("node={node} "));
    let (answer, outcome) = match shared {
        Ok(book) => {
            let first_hand: Vec<&Peer> = book
                .iter()
                .filter(|peer| peer.via.first_hand())
                .take(PEERS_MAX)
                .collect();
            let mut answer = format!("OK {} {}\n", first_hand.len(), named.trim_end());
            for peer in &first_hand {
                answer.push_str(&peer.line());
                answer.push('\n');
            }
            (answer, Ok(first_hand.len()))
        }
        Err((word, why)) => {
            let why = aterm_codec::tty::one_line(&why, 400);
            (format!("ERR {word} {named}{why}\n"), Err(why))
        }
    };
    stream.write_all(answer.as_bytes())?;
    stream.flush()?;
    Ok(outcome)
}

/// The issuer's book as a member read it ([`fetch_book`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Book {
    /// The node that answered: the issuer's, checked against the member's
    /// record of it.
    pub node: String,
    /// Its first-hand entries as it sent them; a line that is no such entry
    /// (a later aterm's, say) is passed over.
    pub peers: Vec<Peer>,
    /// The address the issuer answered at: an entry at a loopback address is
    /// on the issuer's machine ([`copies_of`]).
    pub at: IpAddr,
    /// Whether the issuer proved the key the member binds it to: only then
    /// are the keys its book binds copied ([`copies_of`]). An issuer the
    /// member binds no key to (it joined before keys came) is a claim, and
    /// so is every key its book names.
    pub proven: bool,
}

/// Why a member's read of its issuer's book brought no book.
#[derive(Debug)]
pub enum BookUnread {
    /// A machine that proved the fleet's secret at this address serves no
    /// `book`: an older aterm (`NO op`). Which node it is the handshake does
    /// not say.
    Older(SocketAddr),
    /// The machine at `at` holds no join code (`ERR no-book`): no issuer, no
    /// book. `node` is the node it named, if it named one.
    NoBook {
        /// Where it answered.
        at: SocketAddr,
        /// The node it named.
        node: Option<String>,
        /// Its words.
        why: String,
    },
    /// Any other `ERR` (`not-now`, or a later aterm's): no book this time.
    NotNow {
        /// Where it answered.
        at: SocketAddr,
        /// The node it named.
        node: Option<String>,
        /// Its words.
        why: String,
    },
    /// The address answered with this other node's book (`node=`).
    OtherNode(SocketAddr, String),
    /// Not reached, or its answer was no book.
    Unreached(Unreached),
}

/// The member's end of a book read: dial `issuer` for op `book` by
/// `deadline` (and the `within` it was set from, which a deadline's reason
/// names), proving `me`, then read its answer under the same deadline and the
/// same socket-shutting watchdog a survey's reply is read under. A peer that
/// proves another node than `issuer.node`, or a head naming another, is
/// another machine's book ([`BookUnread::OtherNode`]); one that claims the
/// issuer's node and proves another key than `issuer.key`, or none where it
/// binds one, is no book ([`Unreached::Impostor`], [`Unreached::Unproved`]);
/// an answer promising more than [`PEERS_MAX`] entries, or past
/// [`REPLY_MAX`] bytes, is no book. Which machine a refusal came from is said
/// with it, for [`read_whole`] to judge: the name is not tried here, so an
/// older issuer costs its member one exchange.
///
/// # Errors
/// A [`BookUnread`] naming why there is no book.
pub fn fetch_book(
    issuer: &Target,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    (deadline, within): (Instant, Duration),
) -> Result<Book, BookUnread> {
    let mut reached = dial_avoiding(
        &issuer.host,
        issuer.addr,
        issuer.port,
        (secret, me),
        FleetOp::Book,
        (deadline, within),
        (&[], resolve),
    )
    .map_err(|unreached| match unreached {
        Unreached::Handshake(at, FleetError::UnknownOp(_)) => BookUnread::Older(at),
        other => BookUnread::Unreached(other),
    })?;
    let at = reached.addr;
    // The issuer, held to the key its target names.
    let keys = Keyring {
        issuer: issuer.node.clone(),
        ..Keyring::default()
    };
    let proven = match answered_as(issuer, &keys, at, reached.peer.as_ref()) {
        Ok(answered) => answered.is_some_and(|(_, proven)| proven),
        Err(Unreached::OtherNode { at, said, .. }) => return Err(BookUnread::OtherNode(at, said)),
        Err(unreached) => return Err(BookUnread::Unreached(unreached)),
    };
    let (head, lines) =
        ask_lines(&mut reached.transport, None, deadline, book_head).map_err(|why| {
            BookUnread::Unreached(match why {
                Asked::Deadline => Unreached::Deadline(Stage::Book, within),
                Asked::Failed(why) => Unreached::Reply(at, why),
                Asked::Local(why) => Unreached::Local(why),
            })
        })?;
    if let Some(rest) = head.strip_prefix("ERR ") {
        let (word, rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let (node, why) = match rest.split_once(' ') {
            Some((field, why)) => match field.strip_prefix("node=").filter(|n| is_node_id(n)) {
                Some(node) => (Some(node.to_owned()), why),
                None => (None, rest),
            },
            None => (None, rest),
        };
        let why = aterm_codec::tty::one_line(why, 400);
        return Err(if word == BOOK_NONE {
            BookUnread::NoBook { at, node, why }
        } else {
            BookUnread::NotNow { at, node, why }
        });
    }
    let node = head
        .split(' ')
        .find_map(|field| field.strip_prefix("node="))
        .filter(|node| is_node_id(node))
        .map(str::to_owned)
        .ok_or_else(|| {
            BookUnread::Unreached(Unreached::Reply(
                at,
                "answered a book naming no node".into(),
            ))
        })?;
    if issuer.node.as_deref().is_some_and(|want| want != node) {
        return Err(BookUnread::OtherNode(at, node));
    }
    let peers = lines
        .iter()
        .filter_map(|line| Peer::parse(line))
        .filter(|peer| peer.via.first_hand())
        .collect();
    Ok(Book {
        node,
        peers,
        at: at.ip().to_canonical(),
        proven,
    })
}

/// How many entries a book's head `OK <n> node=<id>` promises — none for an
/// `ERR <why>`, which is the whole answer; anything else is a reason.
fn book_head(line: &str) -> Result<usize, String> {
    if line.starts_with("ERR ") {
        return Ok(0);
    }
    let shown = aterm_codec::tty::one_line(line, 120);
    match line
        .strip_prefix("OK ")
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse::<usize>().ok())
    {
        Some(n) if n <= PEERS_MAX => Ok(n),
        Some(n) => Err(format!(
            "promised {n} entries, past the {PEERS_MAX} a book holds: refused"
        )),
        None => Err(format!("answered no book: {shown}")),
    }
}

/// Whether `host` names the machine it is written on: a loopback address or
/// `localhost`.
fn names_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `entry` of a book the issuer served from `issuer_at`, as its member's
/// copy: one at a loopback address, or named by one, is on the issuer's
/// machine, so when the issuer answered at another address the copy is at
/// that one (and named by it, where its name was the loopback's).
fn copy_of(entry: &Peer, issuer_at: IpAddr) -> Peer {
    let mut copy = Peer {
        via: Via::IssuerBook,
        // The issuer's to judge its registrations by, never a reader's.
        key_from: None,
        ..entry.clone()
    };
    if issuer_at.is_loopback() {
        return copy;
    }
    let named_local = names_loopback(&copy.host);
    if copy.addr.is_some_and(|addr| addr.is_loopback()) || (copy.addr.is_none() && named_local) {
        copy.addr = Some(issuer_at);
    }
    if named_local {
        copy.host = issuer_at.to_string();
    }
    copy
}

/// The member's copy of `fetched`, its issuer's book: each first-hand entry,
/// in its order, as a copy ([`copy_of`]) — unless it is this machine (`own`,
/// its node id), the issuer itself, or a node or place an earlier copy holds
/// — at most [`PEERS_MAX`]. Each keeps the key the book binds it to only
/// when the book came from the issuer the member binds ([`Book::proven`]):
/// otherwise it is a claim, and a key in it would let whoever answered for
/// the issuer name the keys this machine then reads its members by. What
/// [`write_issuer_book`] keeps; a copy is never passed on, as
/// [`serve_book`] shares first-hand entries only.
#[must_use]
pub fn copies_of(fetched: &Book, own: Option<&str>) -> Vec<Peer> {
    let mut copies = Vec::new();
    with_copies(
        &mut copies,
        fetched
            .peers
            .iter()
            .filter(|peer| peer.via.first_hand())
            .map(|entry| copy_of(entry, fetched.at))
            .map(|copy| Peer {
                key: copy.key.filter(|_| fetched.proven),
                ..copy
            })
            .filter(|copy| {
                copy.node
                    .as_deref()
                    .is_none_or(|node| Some(node) != own && node != fetched.node)
            }),
    );
    copies
}

/// `book` with `copies` (of its issuer's book) after its own entries, as a
/// read reads them: a copy whose node or place `book` (or an earlier copy)
/// holds already is left out — this machine's own entry stands for it, and
/// the copy's key still binds its node ([`Keyring::of`] of both) — and
/// `book` never grows past [`PEERS_MAX`], its own entries kept first.
pub fn with_copies(book: &mut Vec<Peer>, copies: impl IntoIterator<Item = Peer>) {
    for copy in copies {
        if book.len() >= PEERS_MAX {
            break;
        }
        let held = book.iter().any(|known| {
            (copy.node.is_some() && known.node == copy.node) || known.same_place(&copy)
        });
        if !held {
            book.push(copy);
        }
    }
}

/// A member's `book` with its copy of its issuer's book replaced by
/// `fetched`, WHOLE: every [`Via::IssuerBook`] entry dropped, then
/// [`copies_of`] `fetched` read beside it ([`with_copies`]).
pub fn merge_book(book: &mut Vec<Peer>, fetched: &Book, own: Option<&str>) {
    book.retain(|peer| peer.via != Via::IssuerBook);
    with_copies(book, copies_of(fetched, own));
}

// ---------------------------------------------------------------------------
// reaching a peer
// ---------------------------------------------------------------------------

/// The longest one connect may take: a LAN host answers a SYN in
/// milliseconds, so a recorded address that has gone dark gives the name
/// (and `<host>.local`) the rest of the deadline.
pub const CONNECT_WITHIN: Duration = Duration::from_secs(1);

/// The most peers a fleet read asks at once (`peers`, `survey --fleet`):
/// each holds one socket while it is asked, so a reader under a Dock-launched
/// app's 256 descriptors keeps most of them.
pub const AT_ONCE: usize = 32;

/// A peer reached and proved: the stream, ready for the op's request.
pub struct Reached {
    /// The proved stream, its socket deadlines cleared.
    pub transport: TlsTransport<ClientConnection>,
    /// The level the peer proved it serves.
    pub scope: FleetScope,
    /// The address it answered at.
    pub addr: SocketAddr,
    /// The node it proved it holds the key of, when it proved one: which
    /// node that makes it is the book's to say ([`Keyring::proven`]).
    pub peer: Option<NodeProof>,
}

/// Why a peer was not reached, as a person can act on it.
#[derive(Debug)]
pub enum Unreached {
    /// Its host name resolves to no address.
    Resolve(String),
    /// No connection to this address.
    Connect(SocketAddr, io::Error),
    /// Connected; the handshake failed.
    Handshake(SocketAddr, FleetError),
    /// The one deadline came at this stage.
    Deadline(Stage, Duration),
    /// Its host name was still being looked up when the deadline came.
    Lookup(String, Duration),
    /// Proved; its answer to the request was none ([`survey_all`]).
    Reply(SocketAddr, String),
    /// The address is none a fleet listener serves a reader from
    /// ([`crate::fleet::is_fleet_source`]), so it was not dialled.
    NotFleet(SocketAddr),
    /// The address answered as another node — the one it proved, or its
    /// survey's `node=` — not the book's: it passed to another member, and
    /// the peer's name did not answer as the book's node either.
    OtherNode {
        /// The address that answered.
        at: SocketAddr,
        /// The node it answered as.
        said: String,
        /// The node the book holds there: what `aterm fleet peers rm`
        /// takes when that machine left.
        want: String,
    },
    /// The address claimed this node and proved another key than the one
    /// this machine binds it to (the module doc's **Node keys**): not that
    /// node, so its survey was never asked; and the peer's name did not
    /// answer as that node either.
    Impostor {
        /// The address that answered.
        at: SocketAddr,
        /// The node it claimed.
        node: String,
        /// Whether that node is this machine's issuer, whose old key is
        /// taken out here (its book never holds itself), never on the
        /// issuer.
        issuer: bool,
    },
    /// This machine binds the book's node a key, and the address proved no
    /// node: an older aterm, one that holds no node key, or a peer that will
    /// not say — never read as that node; and the peer's name did not answer
    /// as that node either.
    Unproved {
        /// The address that answered.
        at: SocketAddr,
        /// The node this machine binds a key to.
        node: String,
    },
    /// A drive ([`drive`]) reached a peer that proved no node this machine
    /// binds a key to — no proof, or a node bound to none: what a read takes
    /// as the peer's claim, a drive never does.
    Unbound {
        /// The address that answered.
        at: SocketAddr,
        /// The node it claimed, when it named one.
        node: Option<String>,
    },
    /// This machine could not try.
    Local(String),
}

/// Where a peer's one deadline came ([`Unreached::Deadline`]): each says
/// something different of the peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Before any connection was made.
    Connect,
    /// Connected, and the fleet handshake unfinished: an aterm that is
    /// stopped or overloaded, or something that is no aterm on its port.
    Handshake,
    /// Proved, and its answer not come: that aterm's control server is stuck.
    Answer,
    /// Proved for its book, and the book not come: the listener answers a
    /// book itself, at once and unasked, so that aterm is stopped or
    /// overloaded ([`fetch_book`]).
    Book,
}

impl Unreached {
    /// Whether it came before the peer proved itself — no name, no address,
    /// no connection, no handshake — so a second dial of that peer in the same
    /// deadline would fail the same way. A peer that refused for capacity may
    /// have a share free a moment later, and one that proved itself and then
    /// gave no answer may answer another request.
    #[must_use]
    pub fn before_proof(&self) -> bool {
        match self {
            Self::Handshake(_, FleetError::AddressBusy) => false,
            Self::Resolve(_)
            | Self::Connect(..)
            | Self::Handshake(..)
            | Self::Lookup(..)
            | Self::NotFleet(_)
            | Self::Local(_)
            | Self::Deadline(Stage::Connect | Stage::Handshake, _) => true,
            Self::Deadline(Stage::Answer | Stage::Book, _)
            | Self::Reply(..)
            | Self::OtherNode { .. }
            | Self::Impostor { .. }
            | Self::Unproved { .. }
            | Self::Unbound { .. } => false,
        }
    }

    /// Whether the connect failed with no route to the HOST
    /// (`EHOSTUNREACH`): what macOS's local-network privacy gives at once an
    /// app the person has not allowed, and what a host off the LAN gives too.
    /// Never `ENETUNREACH` ([`Unreached::no_network`]), which privacy does
    /// not give.
    #[must_use]
    pub fn no_route(&self) -> bool {
        matches!(
            self,
            Self::Connect(_, e) if e.kind() == io::ErrorKind::HostUnreachable
        )
    }

    /// Whether the connect failed with no route to the peer's NETWORK
    /// (`ENETUNREACH`): this machine has no interface on the LAN — offline,
    /// or a full-tunnel VPN holding the route — whatever the peer does.
    #[must_use]
    pub fn no_network(&self) -> bool {
        matches!(
            self,
            Self::Connect(_, e) if e.kind() == io::ErrorKind::NetworkUnreachable
        )
    }

    /// Whether a connect to the peer was tried: `false` for a name that did
    /// not resolve (or not in time), an address past the LAN that was never
    /// dialled, and a failure of this machine's own before any connect —
    /// none of which says anything of this machine's route into the LAN.
    #[must_use]
    pub fn dialled(&self) -> bool {
        !matches!(
            self,
            Self::Resolve(_) | Self::Lookup(..) | Self::NotFleet(_) | Self::Local(_)
        )
    }

    /// Whether the connect was refused to this process itself (`EPERM`,
    /// `EACCES`): a sandbox around it, or a firewall, not the peer.
    #[must_use]
    pub fn not_permitted(&self) -> bool {
        matches!(
            self,
            Self::Connect(_, e) if e.kind() == io::ErrorKind::PermissionDenied
        )
    }
}

/// Each reason as a person acts on it, the cause and the remedy first and
/// whole within the room a survey row keeps for a reason beside its label
/// (`aterm_ctl::UNANSWERED_REASON_CHARS`, 103 characters), for an IPv4 LAN
/// address: a row is capped at 160 characters. An io error that broke a
/// proved peer's answer off is worded by its kind ([`broke_off`]), never by
/// a library's text; this machine's own failures ([`Self::Local`], a socket
/// option the exchange could not set) are its OS's words, which are short.
impl fmt::Display for Unreached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolve(host) => write!(f, "{host} resolves to no address"),
            Self::Connect(addr, e) => match e.kind() {
                io::ErrorKind::ConnectionRefused => write!(
                    f,
                    "nothing listens on {addr}: no aterm running there, or no [fleet] listen"
                ),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                    write!(f, "{addr} did not answer in time")
                }
                io::ErrorKind::HostUnreachable => write!(
                    f,
                    "no route to {}: off the LAN, or not allowed in Privacy & Security › Local \
                     Network",
                    addr.ip()
                ),
                io::ErrorKind::NetworkUnreachable => write!(
                    f,
                    "this machine has no route to {addr}: offline, or a VPN holds the LAN's route"
                ),
                // `EPERM`/`EACCES` from connect: no file's privacy consent,
                // which a Mac's `Operation not permitted` usually is. Refused
                // to this process, whatever it dials, so no address.
                io::ErrorKind::PermissionDenied => f.write_str(
                    "a sandbox here (Claude Code's) or firewall refused: run outside it",
                ),
                _ => write!(f, "{addr}: {e}"),
            },
            Self::Handshake(addr, e) => match e {
                // Either side may hold the old secret, so the remedy names
                // the rotation's: the machine with the old code joins again.
                FleetError::ListenerWrongSecret => write!(
                    f,
                    "another secret at {addr}: the fleet rotated (join again with the new \
                     code) or it left"
                ),
                FleetError::Refused => write!(
                    f,
                    "{addr} refused this machine's proof: this machine holds another fleet's \
                     secret"
                ),
                FleetError::Alpn => write!(f, "{addr} is a pre-fleet aterm listener (update it)"),
                FleetError::AddressBusy => write!(
                    f,
                    "{addr} is busy with other reads from this machine's address: try again when \
                     one ends"
                ),
                // A listener serves `drive` only where its person allows it,
                // so the remedy is that Mac's person's, or an update.
                FleetError::UnknownOp(op) if op == FleetOp::Drive.word() => write!(
                    f,
                    "{addr} lets no one drive it: its person runs `aterm fleet allow drive \
                     --from <this Mac>` there (or it runs an older aterm)"
                ),
                FleetError::UnknownOp(op) => write!(
                    f,
                    "{addr} does not serve `{op}`: an older aterm (update it)"
                ),
                // The listener proved a level this end does not know: it is
                // the newer one, so the remedy is here. The word is the
                // listener's, so it comes last, and short.
                FleetError::UnknownScope(scope) => write!(
                    f,
                    "{addr} serves a fleet level unknown here: update this aterm (`{}`)",
                    scope.chars().take(16).collect::<String>()
                ),
                FleetError::Tls(e) => write!(f, "{addr} is no fleet listener (TLS: {e})"),
                other => write!(f, "{addr}: {other}"),
            },
            Self::Deadline(stage, within) => {
                let within = within.as_secs_f64();
                match stage {
                    Stage::Connect => write!(f, "no connection within {within:.1}s"),
                    Stage::Handshake => write!(
                        f,
                        "connected, but no fleet handshake within {within:.1}s: an aterm \
                         stopped or overloaded, or no aterm there"
                    ),
                    Stage::Answer => write!(
                        f,
                        "proved itself, but sent no survey within {within:.1}s: its aterm's \
                         control server is stuck"
                    ),
                    Stage::Book => write!(
                        f,
                        "proved itself, but sent no book within {within:.1}s: an aterm stopped \
                         or overloaded"
                    ),
                }
            }
            Self::Lookup(host, within) => write!(
                f,
                "{host} did not resolve within {:.1}s (no DNS or multicast-DNS answer)",
                within.as_secs_f64()
            ),
            Self::Reply(addr, why) => write!(f, "{addr} {why}"),
            Self::NotFleet(addr) => write!(
                f,
                "{} is no LAN address (loopback, private, link-local): not dialled, a fleet is \
                 one LAN",
                addr.ip()
            ),
            // Both remedies: a machine that moved joins again from where it is; one that left (re-imaged,
            // its state dir lost, so it joined as a new node) never will, and
            // its entry counts toward the address's share until it goes.
            Self::OtherNode { at, said, want } => write!(
                f,
                "{} is node {} now: rejoin this peer, or `aterm fleet peers rm {}` if it left",
                at.ip(),
                said.get(..8).unwrap_or(said),
                want.get(..8).unwrap_or(want)
            ),
            // Not that node: the remedy is the issuer's, for a node that
            // lost its key and joined with a new one; for the issuer itself,
            // this machine's, which then joins it again.
            Self::Impostor {
                at,
                node,
                issuer: false,
            } => write!(
                f,
                "{} claims node {} with another key: not read (re-keyed? peers rm it on the \
                 issuer)",
                at.ip(),
                node.get(..8).unwrap_or(node)
            ),
            Self::Impostor {
                at,
                node,
                issuer: true,
            } => write!(
                f,
                "{} claims issuer {} with another key: not read (re-keyed? peers rm it here, \
                 rejoin)",
                at.ip(),
                node.get(..8).unwrap_or(node)
            ),
            Self::Unproved { at, node } => write!(
                f,
                "{} proves no key for node {}, which is bound to one: not read (update aterm \
                 there)",
                at.ip(),
                node.get(..8).unwrap_or(node)
            ),
            Self::Unbound { at, node } => write!(
                f,
                "{} proved no node this machine binds a key to{}: not driven (`aterm fleet \
                 peers` reads the issuer's book)",
                at.ip(),
                node.as_deref().map_or_else(String::new, |node| format!(
                    " (it claims {})",
                    node.get(..8).unwrap_or(node)
                ))
            ),
            Self::Local(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Unreached {}

/// Reach the peer at `addr` (when one is recorded) or `host`, on `port`, and
/// prove `op` to it with `secret` — and this machine's node, `me`, when it
/// holds one — all within the ONE wall-clock deadline
/// `within`: the recorded address first; then `host`'s addresses (or
/// `<host>.local`'s, for a name with no dot, both looked up at once), IPv4
/// first, at most four — each connect at most [`CONNECT_WITHIN`], and each
/// name looked up on a thread of its own, which is left to end by itself
/// when the deadline comes first ([`resolve_within`]). So this returns by the
/// deadline, whatever the network does.
///
/// An address that answers but is no member of this fleet — another
/// fleet's listener, one that refuses this machine's proof, a pre-fleet
/// aterm — is passed over like one that refuses the connect: an address
/// recorded long ago may belong to another machine now, and the name may
/// still reach the peer. One that proves this fleet's secret and serves no
/// such op (`NO op`: an older aterm of this fleet) ends the dial there, so
/// an older issuer asked its book costs its member one exchange, never a
/// name lookup out of the read's one deadline; which machine of the fleet
/// that was is the caller's to judge ([`read_whole`] asks the issuer's own
/// survey, which names its node). An address no fleet listener serves a
/// reader from ([`crate::fleet::is_fleet_source`]) is never dialled.
///
/// # Errors
/// What went wrong at the first address tried — the recorded one, which is
/// the one a person sees listed — or the deadline's stage ([`Unreached`]).
pub fn dial(
    host: &str,
    addr: Option<IpAddr>,
    port: u16,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    op: FleetOp,
    within: Duration,
) -> Result<Reached, Unreached> {
    dial_avoiding(
        host,
        addr,
        port,
        (secret, me),
        op,
        (Instant::now() + within, within),
        (&[], resolve),
    )
}

/// How a name is looked up: the system resolver ([`resolve`]) for every
/// caller but a test, which passes one that never reaches the network.
type Resolver = fn(&str, u16) -> Vec<SocketAddr>;

/// [`dial`] by `deadline` (and the `within` it was set from, which a
/// deadline's reason names), never trying an address in `avoid`, names
/// looked up by `resolver`.
fn dial_avoiding(
    host: &str,
    addr: Option<IpAddr>,
    port: u16,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    op: FleetOp,
    (deadline, within): (Instant, Duration),
    (avoid, resolver): (&[SocketAddr], Resolver),
) -> Result<Reached, Unreached> {
    let mut tried: Vec<SocketAddr> = avoid.to_vec();
    let mut first = None;
    // One address: `Some` ends the dial (reached, or the deadline came);
    // `None` goes on to the next, the first failure kept as the reason.
    let attempt = |to: SocketAddr, first: &mut Option<Unreached>| {
        if !crate::fleet::is_fleet_source(to.ip()) {
            first.get_or_insert(Unreached::NotFleet(to));
            return None;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Some(Err(Unreached::Deadline(Stage::Connect, within)));
        }
        match TcpStream::connect_timeout(&to, left.min(CONNECT_WITHIN)) {
            Ok(tcp) => {
                let left = deadline.saturating_duration_since(Instant::now());
                match client_handshake_as(tcp, secret, me, op, left) {
                    Ok(Dialed {
                        transport,
                        scope,
                        peer,
                    }) => Some(Ok(Reached {
                        transport,
                        scope,
                        addr: to,
                        peer,
                    })),
                    Err(FleetError::Timeout(_)) => {
                        Some(Err(Unreached::Deadline(Stage::Handshake, within)))
                    }
                    // It proved this fleet's secret: a machine of this
                    // fleet, an older aterm, though maybe not the peer the
                    // address was recorded for. The caller judges which
                    // ([`read_whole`]); a name lookup here would spend the
                    // deadline on every older issuer.
                    Err(e @ FleetError::UnknownOp(_)) => Some(Err(Unreached::Handshake(to, e))),
                    Err(e) => {
                        first.get_or_insert(Unreached::Handshake(to, e));
                        None
                    }
                }
            }
            Err(e) => {
                first.get_or_insert(Unreached::Connect(to, e));
                None
            }
        }
    };
    if let Some(ip) = addr {
        let to = SocketAddr::new(ip, port);
        if !tried.contains(&to) {
            tried.push(to);
            if let Some(done) = attempt(to, &mut first) {
                return done;
            }
        }
    }
    let left = deadline.saturating_duration_since(Instant::now());
    let resolved = match resolve_within(host, port, left, resolver) {
        Lookup::Found(resolved) => resolved,
        Lookup::Late => {
            return Err(first.unwrap_or_else(|| Unreached::Lookup(host.to_owned(), within)));
        }
        Lookup::Unstarted(e) => {
            return Err(
                first.unwrap_or_else(|| Unreached::Local(format!("could not look {host} up: {e}")))
            );
        }
    };
    for to in resolved {
        if tried.contains(&to) {
            continue;
        }
        tried.push(to);
        if let Some(done) = attempt(to, &mut first) {
            return done;
        }
    }
    Err(first.unwrap_or_else(|| Unreached::Resolve(host.to_owned())))
}

/// What [`resolve_within`] found.
enum Lookup {
    /// The addresses, IPv4 first, at most four; none when no name resolves.
    Found(Vec<SocketAddr>),
    /// No answer by the deadline.
    Late,
    /// No lookup thread could start.
    Unstarted(io::Error),
}

/// Two name families for each active fleet read, across the process.
const RESOLVER_THREADS_MAX: usize = 2 * AT_ONCE;
static RESOLVERS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Owned by the actual OS resolver thread, including after its caller timed out.
struct ResolverPermit<'a>(&'a AtomicUsize);

impl<'a> ResolverPermit<'a> {
    fn take(in_flight: &'a AtomicUsize) -> Option<Self> {
        let mut held = in_flight.load(Ordering::Relaxed);
        loop {
            if held >= RESOLVER_THREADS_MAX {
                return None;
            }
            match in_flight.compare_exchange_weak(
                held,
                held + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self(in_flight)),
                Err(now) => held = now,
            }
        }
    }
}

impl Drop for ResolverPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// `host`'s addresses on `port`, waited for at most `within`: itself when it
/// is an address; else `resolver`'s answer for it and, for a name
/// with no dot, for `<host>.local` (multicast DNS) — the two asked at once,
/// each on a thread of its own, so a slow unicast lookup of the bare name
/// never spends the deadline multicast DNS would answer in. The first
/// non-empty answer wins; a thread still asking at the deadline ends by
/// itself, later, since the system resolver has no deadline of its own. Its
/// permit stays with that thread: at most `2 * AT_ONCE` OS lookups are active
/// across this process, and an exhausted budget refuses without queuing work.
fn resolve_within(host: &str, port: u16, within: Duration, resolver: Resolver) -> Lookup {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Lookup::Found(vec![SocketAddr::new(ip, port)]);
    }
    if within.is_zero() {
        return Lookup::Late;
    }
    let deadline = Instant::now() + within;
    let mut names = vec![host.to_owned()];
    if !host.contains('.') {
        names.push(format!("{host}.local"));
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(names.len());
    let mut asking = 0usize;
    let mut unstarted = None;
    for name in names {
        let Some(permit) = ResolverPermit::take(&RESOLVERS_IN_FLIGHT) else {
            return Lookup::Unstarted(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the fleet resolver is at capacity",
            ));
        };
        let tx = tx.clone();
        match std::thread::Builder::new()
            .name("aterm-fleet-resolve".to_owned())
            .spawn(move || {
                let _permit = permit;
                let _ = tx.send(resolver(&name, port));
            }) {
            Ok(_) => asking += 1,
            Err(e) => unstarted = Some(e),
        }
    }
    drop(tx);
    if asking == 0 {
        return unstarted.map_or(Lookup::Late, Lookup::Unstarted);
    }
    for _ in 0..asking {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(found) if !found.is_empty() => return Lookup::Found(found),
            Ok(_) => {}
            Err(_) => return Lookup::Late,
        }
    }
    Lookup::Found(Vec::new())
}

/// The system resolver's addresses for `name` on `port`, IPv4 first, at most
/// four; none when it resolves to none.
fn resolve(name: &str, port: u16) -> Vec<SocketAddr> {
    let Ok(found) = (name, port).to_socket_addrs() else {
        return Vec::new();
    };
    let mut found: Vec<SocketAddr> = found.collect();
    found.sort_by_key(SocketAddr::is_ipv6);
    found.dedup();
    found.truncate(4);
    found
}

/// One peer to reach: what [`dial`] takes, and the node it should be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Its host name.
    pub host: String,
    /// Its recorded address, tried first.
    pub addr: Option<IpAddr>,
    /// Its fleet port.
    pub port: u16,
    /// Its node id, when the book knows it: a reply that names another is
    /// another member's ([`Unreached::OtherNode`]).
    pub node: Option<String>,
    /// The key the book binds that node to, when it binds one: a peer that
    /// claims the node is read as it only when it proves this key
    /// ([`Unreached::Impostor`], [`Unreached::Unproved`]).
    pub key: Option<NodeKey>,
}

/// `ask` of every one of `items`, at most `limit` at once, each on a thread
/// of its own (this one among them, so a refused thread only makes the walk
/// slower): the answers in `items`' order.
pub(crate) fn at_once<T, R>(items: &[T], limit: usize, ask: impl Fn(&T) -> R + Sync) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    let next = AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let at = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(at) else {
                return done;
            };
            done.push((at, ask(item)));
        }
    };
    let mut answers: Vec<(usize, R)> = std::thread::scope(|scope| {
        let helpers: Vec<_> = (1..limit.clamp(1, items.len().max(1)))
            .filter_map(|_| {
                std::thread::Builder::new()
                    .name("aterm-fleet-ask".to_owned())
                    .spawn_scoped(scope, work)
                    .ok()
            })
            .collect();
        let mut answers = work();
        for helper in helpers {
            // A helper that panicked took its items' answers with it: the
            // panic goes on, rather than a shorter list in the wrong order.
            match helper.join() {
                Ok(done) => answers.extend(done),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        answers
    });
    answers.sort_by_key(|(at, _)| *at);
    answers.into_iter().map(|(_, answer)| answer).collect()
}

/// What a check of one peer found when it answered (`aterm fleet peers`:
/// [`read_whole`] with `survey facts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// The level it proved it serves.
    pub scope: FleetScope,
    /// The address it answered at.
    pub addr: SocketAddr,
    /// The node that answered: the one it proved, else the one its survey
    /// header names (`node=`); `None` for an aterm that says neither.
    pub node: Option<String>,
    /// Whether that node is proved: it proved the key this machine binds it
    /// to ([`Keyring`]), or this machine's own ([`Surveyed::proven`]).
    pub proven: bool,
}

/// One peer's survey, as it answered it ([`survey_all`]).
#[derive(Debug)]
pub struct Surveyed {
    /// The level it proved it serves.
    pub scope: FleetScope,
    /// The address it answered at.
    pub addr: SocketAddr,
    /// The node that answered: the one it proved on the connection, else
    /// (an older aterm) the one its header names (`node=`); `None` for an
    /// aterm that says neither.
    pub node: Option<String>,
    /// Whether that node is proved: it proved the key this machine binds it
    /// to, or this machine's own key (this machine, at a place named by
    /// hand). A node bound to no key (an entry an older aterm recorded, a
    /// peer named by hand) is the peer's claim.
    pub proven: bool,
    /// Its reply's lines after the `OK <n>` head, newlines dropped.
    pub lines: Vec<String>,
}

impl Surveyed {
    /// What a check reads of it: the level, the address and the node.
    #[must_use]
    pub fn checked(&self) -> Checked {
        Checked {
            scope: self.scope,
            addr: self.addr,
            node: self.node.clone(),
            proven: self.proven,
        }
    }
}

/// THE FLEET READ (`aterm ctl survey --fleet`, design §3): every target
/// asked `survey` at once — at most [`AT_ONCE`] at a time — each under ONE
/// wall-clock deadline `within` that covers the name lookup, the connects,
/// TLS, both proofs, the request and the whole reply ([`dial`], then the
/// exchange); a reply past [`REPLY_MAX`] bytes is refused. One result per
/// target, in order: its survey, or why there is none. Up to [`AT_ONCE`]
/// targets the whole takes no longer than `within`; past that a target waits
/// for a free place and is then given `within` of its own.
///
/// Each dial proves `me`, and each answer's node is judged by `keys` and the
/// target's own key before its survey is asked ([`Unreached::Impostor`],
/// [`Unreached::Unproved`]). A peer that answers as another node than the
/// target's — the node it proved, or (proving none) its header's — is
/// another member's: the target's name is asked in what is left of the
/// deadline, never that address again, and an answer from it that is the
/// target's (or does not say) is the answer; otherwise the first refusal
/// ([`Unreached::OtherNode`] among them).
#[must_use]
pub fn survey_all(
    targets: &[Target],
    keys: &Keyring,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    within: Duration,
) -> Vec<Result<Surveyed, Unreached>> {
    at_once(targets, AT_ONCE, |target| {
        read_one(target, keys, (secret, me), within, "survey")
    })
}

/// What [`dial`] takes for `peer`, and the node and key it should be.
#[must_use]
pub fn target_of(peer: &Peer) -> Target {
    Target {
        host: peer.host.clone(),
        addr: peer.addr,
        port: peer.port,
        node: peer.node.clone(),
        key: peer.key,
    }
}

/// Whether `a` and `b` are read the same way: one node held to one key, at
/// one host, address and port.
fn same_target(a: &Peer, b: &Peer) -> bool {
    a.node == b.node
        && a.key == b.key
        && a.port == b.port
        && a.addr == b.addr
        && a.host.eq_ignore_ascii_case(&b.host)
}

/// One peer of a whole read ([`read_whole`]): the entry it was read by, and
/// its answer or why there is none.
#[derive(Debug)]
pub struct PeerRead {
    /// The entry: the machine's own, or a copy of the issuer's book as the
    /// book read now has it.
    pub peer: Peer,
    /// Its answer, or why there is none.
    pub read: Result<Surveyed, Unreached>,
}

/// What a whole read made of the issuer's book ([`read_whole`]).
#[derive(Debug)]
pub enum BookRead {
    /// The peers name no issuer (`via=issuer`): this machine issues the
    /// fleet, or knows only peers named by hand. No book was asked.
    NoIssuer,
    /// The issuer's book: [`copies_of`] it is the member's copy now.
    Fetched(Book),
    /// The issuer itself answered that it shares no book —
    /// [`BookUnread::Older`] or [`BookUnread::NoBook`], its own by its node
    /// or by its survey's — so the copy of it goes.
    Refused(BookUnread),
    /// The issuer answered its survey, and no book of it came, for this
    /// reason: the copy stands, and the read is short of a fresh one.
    Unshared(String),
    /// The issuer was not reached (its own row says why): the copy stands.
    Unread(String),
}

/// THE MEMBER'S WHOLE READ (`aterm ctl survey --fleet`, `aterm fleet peers`):
/// `request` (`survey`, or `survey facts` for a check) of every one of
/// `peers` — this machine's book, the peers aterm.toml names beside it, then
/// its copy of its issuer's book ([`with_copies`]) — and of every member its
/// issuer's book adds, the first [`AT_ONCE`] dials under the ONE deadline
/// `within` from now. Every dial proves `me`, and every answer's node is
/// judged by `keys` — what this machine binds, gathered before `peers` left
/// out a copy for a person's entry at its place ([`Keyring::of`] of its book
/// and its copy) — and its own entry's, as [`survey_all`] judges it. The
/// book's keys replace the copy's the moment it comes ([`Keyring::with_book`]),
/// and a copy it holds under another key is read again under the new one:
/// a member the issuer took out and bound afresh is read as itself at once.
///
/// The issuer (the first `via=issuer` entry) has a seat of its own: its book
/// first ([`fetch_book`]), then its own `request` in what is left of the
/// deadline, asked first at the address its book's dial was answered at —
/// one connection to it at a time, so a read never holds two of the issuer's
/// per-address share — and a dial that failed for the book, or a book the
/// deadline cut short, is the issuer's answer too, never dialled twice. A
/// copy that answers as this machine (`own`, a place the issuer's person
/// named by hand) is left out of the answer. Every other peer is asked
/// meanwhile, at most [`AT_ONCE`] at once with the issuer's seat. The book's
/// members ([`merge_book`] of `peers`) that are not among `peers` are queued
/// the moment the book comes and given hands then, before the issuer's own
/// read: an issuer whose control server is slow to answer its survey holds
/// up none of them.
///
/// A refusal counts only when it is the issuer's: `ERR no-book` naming the
/// issuer's node, or one naming no node (`NO op` among them) from the
/// address where the issuer's own survey then answered naming its node.
/// Then every copy `peers` held is left out of the answer
/// ([`BookRead::Refused`]). An address the issuer's survey did not answer
/// at may belong to another machine now: when the survey found the issuer
/// at another address, its book is asked there; otherwise the copies stand
/// ([`BookRead::Unshared`], [`BookRead::Unread`]). A book that came leaves
/// out the copies it no longer holds. With no issuer it is [`survey_all`]
/// of `peers`.
///
/// Up to [`AT_ONCE`] dials, the issuer's seat among them, the whole read
/// takes no longer than `within`; past that a peer waits for a free place
/// and is then given `within` of its own, as [`survey_all`] gives one.
///
/// One result per peer, `peers` first in their order, then the book's new
/// members in its; and what came of the book.
#[must_use]
pub fn read_whole(
    peers: &[Peer],
    keys: &Keyring,
    own: Option<&str>,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    within: Duration,
    request: &str,
) -> (Vec<PeerRead>, BookRead) {
    let Some(issuer_at) = peers.iter().position(|peer| peer.via == Via::Issuer) else {
        let targets: Vec<Target> = peers.iter().map(target_of).collect();
        let reads = at_once(&targets, AT_ONCE, |target| {
            read_one(target, keys, (secret, me), within, request)
        });
        let reads = peers
            .iter()
            .cloned()
            .zip(reads)
            .map(|(peer, read)| PeerRead { peer, read })
            .collect();
        return (reads, BookRead::NoIssuer);
    };
    let deadline = (Instant::now() + within, within);
    let given = peers.len();
    let queue = std::sync::Mutex::new(Queue {
        peers: peers.to_vec(),
        next: 0,
        skip: vec![issuer_at],
        // The issuer's seat, reading from the start, and its dial.
        live: 1,
        started: 1,
        keys: std::sync::Arc::new(keys.clone()),
    });
    let lock = || {
        queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    // One worker: the next peer queued, until none is left. A peer that
    // waited for a free place is given `within` of its own.
    let work = || {
        let mut done = Vec::new();
        loop {
            let next = {
                let mut queued = lock();
                let next = queued.take();
                if next.is_none() {
                    queued.live = queued.live.saturating_sub(1);
                }
                next.map(|next| (next, std::sync::Arc::clone(&queued.keys)))
            };
            let Some(((at, target, waited), keys)) = next else {
                return done;
            };
            let by = if waited {
                (Instant::now() + within, within)
            } else {
                deadline
            };
            done.push((at, read_one_by(&target, &keys, (secret, me), by, request)));
        }
    };
    let issuer = target_of(&peers[issuer_at]);
    let (reads, book, dropped) = std::thread::scope(|scope| {
        // A helper reading the queue, its seat counted, or none (and its seat
        // given back) when no thread could start.
        let helper = move || {
            let spawned = std::thread::Builder::new()
                .name("aterm-fleet-ask".to_owned())
                .spawn_scoped(scope, work);
            if spawned.is_err() {
                let mut queued = lock();
                queued.live = queued.live.saturating_sub(1);
            }
            spawned.ok()
        };
        // Hands for the peers but the issuer, this thread one of them.
        let hands = {
            let mut queued = lock();
            let hands = (given - 1).clamp(1, AT_ONCE - 1);
            queued.live += hands;
            hands
        };
        let helpers: Vec<_> = (1..hands).filter_map(|_| helper()).collect();
        // The issuer's seat: its book, its members' hands, then its own
        // read; then a worker.
        let seat = std::thread::Builder::new()
            .name("aterm-fleet-book".to_owned())
            .spawn_scoped(scope, move || {
                // A book that came: its members queued, the copies it no
                // longer holds skipped, and hands for the members started
                // now, while the issuer's own read is still to come.
                let take = |book: &Book| {
                    let (dropped, more) = {
                        let mut queued = lock();
                        let dropped = queue_book(&mut queued, given, book, own);
                        (dropped, queued.hands())
                    };
                    let started: Vec<_> = (0..more).filter_map(|_| helper()).collect();
                    (dropped, started)
                };
                let mut helpers = Vec::new();
                let mut dropped = Vec::new();
                let fetched = fetch_book(&issuer, (secret, me), deadline);
                if let Ok(book) = &fetched {
                    (dropped, helpers) = take(book);
                }
                let (issuer_read, book) = match fetched {
                    // A dial that failed for the book fails the same for the
                    // issuer's own read, and a book the deadline cut short
                    // leaves that read no time: its reason, never a second
                    // dial.
                    Err(BookUnread::Unreached(unreached))
                        if ends_the_seat(&unreached, deadline.0, Instant::now()) =>
                    {
                        let why = unreached.to_string();
                        (Err(unreached), BookRead::Unread(why))
                    }
                    mut fetched => {
                        // Asked first where its book's dial was answered: the
                        // recorded address, when that dial passed over it,
                        // already failed in this deadline.
                        let surveyed = Target {
                            addr: answered_at(&fetched).or(issuer.addr),
                            ..issuer.clone()
                        };
                        let keys = std::sync::Arc::clone(&lock().keys);
                        let issuer_read =
                            read_one_by(&surveyed, &keys, (secret, me), deadline, request);
                        let want = issuer.node.as_deref();
                        let found = match &issuer_read {
                            Ok(surveyed) if want.is_some() && surveyed.node.as_deref() == want => {
                                Some(surveyed.addr)
                            }
                            _ => None,
                        };
                        let mut may_ask = true;
                        let book = loop {
                            match judge(fetched, found, want, may_ask) {
                                Judged::Book(book) => break BookRead::Fetched(book),
                                Judged::Refused(refused) => {
                                    dropped = drop_copies(&mut lock(), given);
                                    break BookRead::Refused(refused);
                                }
                                Judged::AskAt(at) => {
                                    may_ask = false;
                                    let there = Target {
                                        host: at.ip().to_string(),
                                        addr: Some(at.ip()),
                                        port: at.port(),
                                        node: issuer.node.clone(),
                                        key: issuer.key,
                                    };
                                    fetched = fetch_book(&there, (secret, me), deadline);
                                    if let Ok(book) = &fetched {
                                        let (gone, started) = take(book);
                                        dropped = gone;
                                        helpers.extend(started);
                                    }
                                }
                                Judged::Kept(why) if issuer_read.is_ok() => {
                                    break BookRead::Unshared(why);
                                }
                                Judged::Kept(why) => break BookRead::Unread(why),
                            }
                        };
                        (issuer_read, book)
                    }
                };
                let more = lock().hands();
                helpers.extend((0..more).filter_map(|_| helper()));
                let mut done = work();
                done.push((issuer_at, issuer_read));
                for helper in helpers {
                    match helper.join() {
                        Ok(theirs) => done.extend(theirs),
                        Err(panic) => std::panic::resume_unwind(panic),
                    }
                }
                (done, book, dropped)
            });
        let mut done = work();
        for helper in helpers {
            match helper.join() {
                Ok(theirs) => done.extend(theirs),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        let (book, dropped) = match seat {
            Ok(seat) => match seat.join() {
                Ok((theirs, book, dropped)) => {
                    done.extend(theirs);
                    (book, dropped)
                }
                Err(panic) => std::panic::resume_unwind(panic),
            },
            // No thread for the issuer's seat: the other peers read, the
            // issuer and its book not, and the copies kept.
            Err(e) => {
                let why = format!("could not start the issuer's read: {e}");
                done.push((issuer_at, Err(Unreached::Local(why.clone()))));
                (BookRead::Unread(why), Vec::new())
            }
        };
        (done, book, dropped)
    });
    let queued = queue
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut slots: Vec<Option<Result<Surveyed, Unreached>>> = std::iter::repeat_with(|| None)
        .take(queued.peers.len())
        .collect();
    for (at, read) in reads {
        if let Some(slot) = slots.get_mut(at) {
            *slot = Some(read);
        }
    }
    // A copy that answered as this machine is this machine — a place its
    // issuer's person named by hand, which names no node — and no peer.
    let itself = |peer: &Peer, read: &Option<Result<Surveyed, Unreached>>| {
        peer.via == Via::IssuerBook
            && own.is_some()
            && matches!(read, Some(Ok(surveyed)) if surveyed.node.as_deref() == own)
    };
    let answers = queued
        .peers
        .into_iter()
        .zip(slots)
        .enumerate()
        .filter(|(at, (peer, read))| !dropped.contains(at) && !itself(peer, read))
        .filter_map(|(_, (peer, read))| Some(PeerRead { peer, read: read? }))
        .collect();
    (answers, book)
}

/// Where a book read of the issuer was answered, when it was: the address its
/// book, or its refusal, came from — or one that proved the fleet's secret
/// and then gave no book (a reply that failed, a listener at capacity),
/// which [`dial_avoiding`] may have reached past a recorded address that
/// failed before proof: the survey dialled there again would spend the
/// deadline on that address a second time.
fn answered_at(fetched: &Result<Book, BookUnread>) -> Option<IpAddr> {
    match fetched {
        Ok(book) => Some(book.at),
        Err(
            BookUnread::Older(at)
            | BookUnread::NoBook { at, .. }
            | BookUnread::NotNow { at, .. }
            | BookUnread::OtherNode(at, _)
            | BookUnread::Unreached(
                Unreached::Reply(at, _) | Unreached::Handshake(at, FleetError::AddressBusy),
            ),
        ) => Some(at.ip()),
        Err(BookUnread::Unreached(_)) => None,
    }
}

/// Whether a book read of the issuer that failed with `unreached`, at `now`
/// in a read whose deadline is `deadline`, is the issuer's answer too: a
/// dial that failed before proof fails the same for its survey, and a book
/// the deadline cut short leaves that survey no time — said once, never
/// dialled a second time with nothing left ([`read_whole`]).
fn ends_the_seat(unreached: &Unreached, deadline: Instant, now: Instant) -> bool {
    unreached.before_proof() || now >= deadline
}

/// What a book read's outcome comes to ([`judge`]).
enum Judged {
    /// The issuer's book.
    Book(Book),
    /// The issuer itself shares no book.
    Refused(BookUnread),
    /// Another machine answered where the book was asked, and the issuer's
    /// own survey answered here: ask its book here.
    AskAt(SocketAddr),
    /// No book this time, for this reason: the copy stands.
    Kept(String),
}

/// What `fetched`, a book read of the issuer `want` (its node), comes to,
/// given `found`, where the issuer's own survey answered naming `want`. A
/// refusal is the issuer's when it names `want`, or names no node and came
/// from `found`; one from any other machine — an address recorded long ago
/// may belong to another machine now, and one that claimed `want` without
/// its key is another — is asked again at `found` while `may_ask` holds,
/// and otherwise keeps the copy.
fn judge(
    fetched: Result<Book, BookUnread>,
    found: Option<SocketAddr>,
    want: Option<&str>,
    may_ask: bool,
) -> Judged {
    let short = |node: &str| node.get(..8).unwrap_or(node).to_owned();
    // Where a refusal came from, and which node it named.
    let (at, node) = match &fetched {
        Err(BookUnread::Unreached(
            Unreached::Impostor { at, .. } | Unreached::Unproved { at, .. },
        )) => (Some(*at), None),
        Ok(_) | Err(BookUnread::Unreached(_)) => (None, None),
        Err(BookUnread::Older(at)) => (Some(*at), None),
        Err(BookUnread::NoBook { at, node, .. } | BookUnread::NotNow { at, node, .. }) => {
            (Some(*at), node.clone())
        }
        Err(BookUnread::OtherNode(at, node)) => (Some(*at), Some(node.clone())),
    };
    let theirs = at.is_some_and(|at| match node.as_deref() {
        Some(node) => want == Some(node),
        None => found == Some(at),
    });
    let why = match fetched {
        Ok(book) => return Judged::Book(book),
        // A machine that claimed the issuer's node without its key is
        // another machine: asked again where the issuer's survey proved it.
        Err(BookUnread::Unreached(
            unreached @ (Unreached::Impostor { .. } | Unreached::Unproved { .. }),
        )) => unreached.to_string(),
        Err(BookUnread::Unreached(unreached)) => return Judged::Kept(unreached.to_string()),
        Err(refused @ (BookUnread::Older(_) | BookUnread::NoBook { .. })) if theirs => {
            return Judged::Refused(refused);
        }
        Err(BookUnread::NotNow { why, .. }) if theirs => return Judged::Kept(why),
        Err(BookUnread::Older(at)) => {
            format!(
                "{at} answered as an older aterm, which the issuer's own survey did not confirm"
            )
        }
        Err(BookUnread::NoBook { at, node, why } | BookUnread::NotNow { at, node, why }) => {
            match node {
                Some(node) => {
                    format!(
                        "{at} answered as node {}, not the issuer: {why}",
                        short(&node)
                    )
                }
                None => {
                    format!("{at} answered, which the issuer's own survey did not confirm: {why}")
                }
            }
        }
        Err(BookUnread::OtherNode(at, node)) => format!(
            "{at} answered with the book of node {}, not the issuer's",
            short(&node)
        ),
    };
    match (found, at) {
        (Some(issuer), Some(at)) if may_ask && issuer != at => Judged::AskAt(issuer),
        _ => Judged::Kept(why),
    }
}

/// [`read_whole`]'s peers: every one queued, the next to start, the places
/// not to read there (the issuer's, read on its own seat, and the copies the
/// book's answer left out), how many threads are reading (the workers and
/// the issuer's seat) and how many dials have begun (the seat's among them);
/// and the keys each dial that starts is judged by, the book's once it came.
struct Queue {
    peers: Vec<Peer>,
    next: usize,
    skip: Vec<usize>,
    live: usize,
    started: usize,
    keys: std::sync::Arc<Keyring>,
}

impl Queue {
    /// The next peer to read and where it is, passing over every place to
    /// skip, and whether it waited for a free place: a dial past the first
    /// [`AT_ONCE`].
    fn take(&mut self) -> Option<(usize, Target, bool)> {
        while self.next < self.peers.len() {
            let at = self.next;
            self.next += 1;
            if !self.skip.contains(&at) {
                self.started += 1;
                return Some((at, target_of(&self.peers[at]), self.started > AT_ONCE));
            }
        }
        None
    }

    /// How many more hands to start for the peers queued and not yet taken,
    /// at most [`AT_ONCE`] reading in all; their seats counted.
    fn hands(&mut self) -> usize {
        let left = (self.next..self.peers.len())
            .filter(|at| !self.skip.contains(at))
            .count();
        let more = left.min(AT_ONCE.saturating_sub(self.live));
        self.live += more;
        more
    }
}

/// What the issuer's book `book` does to `queued`, whose first `given` peers
/// are the read's own: its members not among them are queued ([`merge_book`]
/// of them), the copies the book no longer holds — or holds under another
/// key, queued again under the new one — are skipped if not yet read, their
/// places returned, and its keys replace the copy's ([`Keyring::with_book`]).
fn queue_book(queued: &mut Queue, given: usize, book: &Book, own: Option<&str>) -> Vec<usize> {
    queued.keys = std::sync::Arc::new(queued.keys.with_book(book, own));
    let mut merged = queued.peers[..given].to_vec();
    merge_book(&mut merged, book, own);
    let fresh: Vec<Peer> = merged
        .into_iter()
        .filter(|peer| peer.via == Via::IssuerBook)
        .collect();
    let dropped: Vec<usize> = (0..given)
        .filter(|at| {
            queued.peers[*at].via == Via::IssuerBook
                && !fresh
                    .iter()
                    .any(|copy| same_target(copy, &queued.peers[*at]))
        })
        .collect();
    queued.skip.extend(&dropped);
    for copy in fresh {
        let known = queued.peers.iter().enumerate().any(|(at, peer)| {
            !dropped.contains(&at) && (same_target(peer, &copy) || peer.same_place(&copy))
        });
        if !known {
            queued.peers.push(copy);
        }
    }
    dropped
}

/// Every copy of the issuer's book among `queued`'s first `given` peers,
/// skipped if not yet read: the issuer shares no book. Their places.
fn drop_copies(queued: &mut Queue, given: usize) -> Vec<usize> {
    let dropped: Vec<usize> = (0..given)
        .filter(|at| queued.peers[*at].via == Via::IssuerBook)
        .collect();
    queued.skip.extend(&dropped);
    dropped
}

/// One target's `request` ([`survey_all`], [`read_whole`]) within `within`
/// of now, its node checked ([`read_one_by`]).
fn read_one(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    within: Duration,
    request: &str,
) -> Result<Surveyed, Unreached> {
    read_one_by(
        target,
        keys,
        (secret, me),
        (Instant::now() + within, within),
        request,
    )
}

/// One target's `request` by `deadline` (and the `within` it was set from),
/// its node checked: a peer that answers as another node (the one it proved,
/// else its header's), claims the target's with another key or proves none
/// where one is bound ([`answered_as`]) is not the target, and the target's
/// name is asked in what is left of the deadline, never that address again.
fn read_one_by(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    deadline: (Instant, Duration),
    request: &str,
) -> Result<Surveyed, Unreached> {
    let (at, other) = match ask_at(
        target,
        keys,
        target.addr,
        (secret, me),
        deadline,
        request,
        &[],
    ) {
        Ok(first) => {
            let (Some(want), Some(got)) = (target.node.as_deref(), first.node.as_deref()) else {
                return Ok(first);
            };
            if want == got {
                return Ok(first);
            }
            let other = Unreached::OtherNode {
                at: first.addr,
                said: got.to_owned(),
                want: want.to_owned(),
            };
            (first.addr, other)
        }
        Err(
            other @ (Unreached::OtherNode { at, .. }
            | Unreached::Impostor { at, .. }
            | Unreached::Unproved { at, .. }),
        ) => (at, other),
        Err(unreached) => return Err(unreached),
    };
    match ask_at(target, keys, None, (secret, me), deadline, request, &[at]) {
        Ok(again)
            if again
                .node
                .as_deref()
                .is_none_or(|node| Some(node) == target.node.as_deref()) =>
        {
            Ok(again)
        }
        _ => Err(other),
    }
}

/// A peer reached to be DRIVEN (§5.1 of the sessions-as-subagents design,
/// [`drive`]): the proved stream, ready for the control lines the peer's
/// drive gate admits, with the node it proved. Every read and write on it
/// ends by the deadline [`drive`] was given: a watchdog shuts the socket
/// then, since rustls reads a whole TLS record before a read returns and a
/// per-read timeout alone bounds nothing a peer sends a byte at a time.
pub struct Driven {
    stream: rustls::StreamOwned<ClientConnection, TcpStream>,
    watchdog: Option<HandshakeWatchdog>,
    /// The address it answered at.
    pub addr: SocketAddr,
    /// The node it proved, which this machine binds the proved key to.
    pub node: String,
}

impl Read for Driven {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

impl Write for Driven {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl Drop for Driven {
    fn drop(&mut self) {
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.finish();
        }
    }
}

/// DIAL `target` TO DRIVE IT (`aterm ctl --host`): the [`FleetOp::Drive`]
/// op, proved with `secret` and this machine's node `me` (a drive with no
/// node of its own is never admitted, so none is dialled), within the one
/// deadline `within` for the dial and every exchange after it. The peer
/// must prove the node `keys` (or `target`'s entry) binds a key to: a read
/// takes an unbound node as the peer's claim, a drive never does
/// ([`Unreached::Unbound`]). As a read does ([`read_one_by`]), a recorded
/// address that answers as another node is passed over for the target's
/// name, never asked again.
///
/// # Errors
/// Why the peer was not reached, or proved no node bound here; a peer that
/// does not serve `drive` (its person allows none, or it predates the op) is
/// [`Unreached::Handshake`] with [`FleetError::UnknownOp`].
pub fn drive(
    target: &Target,
    keys: &Keyring,
    (secret, me): (&FleetSecret, &NodeIdentity),
    within: Duration,
) -> Result<Driven, Unreached> {
    let deadline = (Instant::now() + within, within);
    match drive_at(target, keys, target.addr, (secret, me), deadline, &[]) {
        Err(
            other @ (Unreached::OtherNode { at, .. }
            | Unreached::Impostor { at, .. }
            | Unreached::Unproved { at, .. }
            | Unreached::Unbound { at, .. }),
        ) => drive_at(target, keys, None, (secret, me), deadline, &[at]).map_err(|_| other),
        done => done,
    }
}

/// [`drive`] at `addr` first, never at one in `avoid`, by `deadline`.
fn drive_at(
    target: &Target,
    keys: &Keyring,
    addr: Option<IpAddr>,
    (secret, me): (&FleetSecret, &NodeIdentity),
    (deadline, within): (Instant, Duration),
    avoid: &[SocketAddr],
) -> Result<Driven, Unreached> {
    let reached = dial_avoiding(
        &target.host,
        addr,
        target.port,
        (secret, Some(me)),
        FleetOp::Drive,
        (deadline, within),
        (avoid, resolve),
    )?;
    let at = reached.addr;
    let node = match answered_as(target, keys, at, reached.peer.as_ref())? {
        Some((node, true)) => node,
        claimed => {
            return Err(Unreached::Unbound {
                at,
                node: claimed.map(|(node, _)| node),
            });
        }
    };
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Unreached::Deadline(Stage::Answer, within));
    }
    let stream = reached.transport.into_stream();
    let tcp = stream.get_ref();
    let bounded = tcp
        .set_read_timeout(Some(left))
        .and_then(|()| tcp.set_write_timeout(Some(left)))
        .and_then(|()| HandshakeWatchdog::arm(tcp, left));
    let watchdog = bounded
        .map_err(|e| Unreached::Local(format!("could not bound the drive's exchange: {e}")))?;
    Ok(Driven {
        stream,
        watchdog: Some(watchdog),
        addr: at,
        node,
    })
}

/// Dial `target` (at `addr` first, never at one in `avoid`) by `deadline`,
/// judge the node that answered ([`answered_as`]) and only then ask
/// `request`: its reply, and the node that answered — the one it proved,
/// else the one its header names, which is refused ([`Unreached::Unproved`])
/// when `keys` bind that node to a key. A peer that proved `me`, this
/// machine's own node under its own key, is this machine (a place a person
/// named by hand that is this one), proven though no keyring binds it.
fn ask_at(
    target: &Target,
    keys: &Keyring,
    addr: Option<IpAddr>,
    (secret, me): (&FleetSecret, Option<&NodeIdentity>),
    (deadline, within): (Instant, Duration),
    request: &str,
    avoid: &[SocketAddr],
) -> Result<Surveyed, Unreached> {
    let mut reached = dial_avoiding(
        &target.host,
        addr,
        target.port,
        (secret, me),
        FleetOp::Survey,
        (deadline, within),
        (avoid, resolve),
    )?;
    let addr = reached.addr;
    let answered = answered_as(target, keys, addr, reached.peer.as_ref())?;
    let (_, lines) = ask_lines(&mut reached.transport, Some(request), deadline, reply_head)
        .map_err(|why| match why {
            Asked::Deadline => Unreached::Deadline(Stage::Answer, within),
            Asked::Failed(why) => Unreached::Reply(addr, why),
            Asked::Local(why) => Unreached::Local(why),
        })?;
    let (node, proven) = match answered {
        Some((node, proven)) => (Some(node), proven || proves_me(me, reached.peer.as_ref())),
        None => match lines.first().and_then(|head| header_node(head)) {
            // A header that names a node bound to a key, from a peer that
            // proved none, is that node's name without its key.
            Some(said) if keys.bound(&said).is_some() => {
                return Err(Unreached::Unproved {
                    at: addr,
                    node: said,
                });
            }
            said => (said, false),
        },
    };
    Ok(Surveyed {
        scope: reached.scope,
        addr,
        node,
        proven,
        lines,
    })
}

/// Whether `proof` is this machine's own node under its own key (`me`): only
/// this machine holds that key, which it never binds in a [`Keyring`].
fn proves_me(me: Option<&NodeIdentity>, proof: Option<&NodeProof>) -> bool {
    me.zip(proof)
        .is_some_and(|(me, proof)| proof.node == me.node() && proof.key == me.key())
}

/// WHICH NODE ANSWERED `target` at `at`, by the node it proved there
/// (`proof`) against the key `target` names, else the one `keys` binds that
/// node to: `Some` with the node and whether it is proved (it proved the
/// bound key; a node bound to none is its claim), or `None` when it proved
/// none and none is bound — an older aterm, whose survey's header then says
/// which node it is. Judged before anything is asked of it.
///
/// # Errors
/// [`Unreached::OtherNode`] for a proof of another node than the target's;
/// [`Unreached::Impostor`] for a proof of the target's node, or of a node a
/// person's entry did not name, under another key than the bound one;
/// [`Unreached::Unproved`] for no proof where a key is bound.
fn answered_as(
    target: &Target,
    keys: &Keyring,
    at: SocketAddr,
    proof: Option<&NodeProof>,
) -> Result<Option<(String, bool)>, Unreached> {
    let bound = |node: &str| {
        target
            .key
            .filter(|_| target.node.as_deref() == Some(node))
            .or_else(|| keys.bound(node))
    };
    let Some(proof) = proof else {
        return match target.node.as_deref() {
            Some(want) if bound(want).is_some() => Err(Unreached::Unproved {
                at,
                node: want.to_owned(),
            }),
            _ => Ok(None),
        };
    };
    if let Some(want) = target.node.as_deref()
        && want != proof.node
    {
        return Err(Unreached::OtherNode {
            at,
            said: proof.node.clone(),
            want: want.to_owned(),
        });
    }
    match bound(&proof.node) {
        Some(key) if key == proof.key => Ok(Some((proof.node.clone(), true))),
        Some(_) => Err(Unreached::Impostor {
            at,
            node: proof.node.clone(),
            issuer: keys.is_issuer(&proof.node),
        }),
        None => Ok(Some((proof.node.clone(), false))),
    }
}

/// The node a survey's `instance` header names (`node=<32 hex>`), if any.
fn header_node(head: &str) -> Option<String> {
    head.strip_prefix("instance ")?
        .split(' ')
        .find_map(|field| field.strip_prefix("node="))
        .filter(|node| is_node_id(node))
        .map(str::to_owned)
}

/// Why [`ask_lines`] has no reply.
enum Asked {
    /// The deadline came first.
    Deadline,
    /// The peer's answer, or its silence, as a reason.
    Failed(String),
    /// This machine could not ask.
    Local(String),
}

/// What a reply's head line promises: how many lines follow it, or why it
/// is no reply ([`reply_head`], [`book_head`]).
type Head = fn(&str) -> Result<usize, String>;

/// Send `request` (when there is one: a book is answered unasked) on a
/// proved stream and read its Lines reply by `deadline`: the head line, whose
/// count `head` reads, and the lines after it. The whole exchange runs under
/// a [`HandshakeWatchdog`] that shuts the socket at `deadline`: rustls reads
/// a whole TLS record before a read returns, across as many socket reads as
/// it takes, so the per-read timeout alone lets a peer that sends a record a
/// byte at a time hold the read for as long as it likes. A reply that came
/// whole as the deadline did is kept.
fn ask_lines(
    transport: &mut TlsTransport<ClientConnection>,
    request: Option<&str>,
    deadline: Instant,
    head: Head,
) -> Result<(String, Vec<String>), Asked> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Asked::Deadline);
    }
    let watchdog = HandshakeWatchdog::arm(transport.stream().get_ref(), left)
        .map_err(|e| Asked::Local(format!("could not bound the reply's read: {e}")))?;
    let result = exchange_lines(transport, request, deadline, head);
    match (watchdog.finish(), result) {
        (_, Ok(lines)) => Ok(lines),
        (Claim::Authed, Err(why)) => Err(why),
        (_, Err(_)) => Err(Asked::Deadline),
    }
}

/// [`ask_lines`]' exchange, each socket read and write bounded by what is
/// left of `deadline`.
fn exchange_lines(
    transport: &mut TlsTransport<ClientConnection>,
    request: Option<&str>,
    deadline: Instant,
    head: Head,
) -> Result<(String, Vec<String>), Asked> {
    let left = |deadline: Instant| {
        let left = deadline.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left).ok_or(Asked::Deadline)
    };
    let timed_out = |e: &io::Error| {
        matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        )
    };
    if let Some(request) = request {
        let tcp_left = left(deadline)?;
        transport
            .stream()
            .get_ref()
            .set_write_timeout(Some(tcp_left))
            .map_err(|e| Asked::Failed(format!("could not be asked: {e}")))?;
        if let Err(e) = transport
            .stream()
            .write_all(format!("{request}\n").as_bytes())
            .and_then(|()| transport.stream().flush())
        {
            return Err(if timed_out(&e) {
                Asked::Deadline
            } else {
                Asked::Failed(broke_off(false, &e))
            });
        }
    }
    let mut pending: Vec<u8> = Vec::new();
    let mut read = 0usize;
    let mut want: Option<(String, usize)> = None;
    let mut lines: Vec<String> = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if read > REPLY_MAX {
            return Err(Asked::Failed(format!(
                "answered past {} KiB: refused",
                REPLY_MAX / 1024
            )));
        }
        while let Some(nl) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line[..nl]).into_owned();
            match want {
                Some(_) => lines.push(line),
                None => {
                    let n = head(&line).map_err(Asked::Failed)?;
                    want = Some((line, n));
                }
            }
            if let Some((head, n)) = &want
                && *n == lines.len()
            {
                return Ok((head.clone(), lines));
            }
        }
        if let Some((head, n)) = &want
            && *n == lines.len()
        {
            return Ok((head.clone(), lines));
        }
        let tcp_left = left(deadline)?;
        transport
            .stream()
            .get_ref()
            .set_read_timeout(Some(tcp_left))
            .map_err(|e| Asked::Failed(format!("could not be read: {e}")))?;
        match transport.stream().read(&mut chunk) {
            Ok(0) => {
                return Err(Asked::Failed(broke_off(
                    true,
                    &io::ErrorKind::UnexpectedEof.into(),
                )));
            }
            Ok(n) => {
                read += n;
                pending.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if timed_out(&e) => return Err(Asked::Deadline),
            Err(e) => return Err(Asked::Failed(broke_off(true, &e))),
        }
    }
}

/// Why a proved peer's exchange broke off on `e`, before it was asked
/// (`answering` false) or while it answered, as the words after its address
/// ([`Unreached::Reply`]): by the error's KIND (`connection reset`), never its
/// text. A TLS library words a peer whose aterm died mid-answer as
/// `peer closed connection without sending TLS close_notify: <a URL>`, past
/// what a fleet read's row keeps for a reason; a hang-up is said as one, the
/// same words as a connection closed cleanly mid-answer. Public so the
/// reasons it gives are held to that row where the row is made
/// (`aterm_ctl::UNANSWERED_REASON_CHARS`).
#[must_use]
pub fn broke_off(answering: bool, e: &io::Error) -> String {
    match (answering, e.kind()) {
        (false, kind) => format!("closed before it was asked: {kind}"),
        (true, io::ErrorKind::UnexpectedEof) => {
            "closed the connection before its answer ended".to_owned()
        }
        (true, kind) => format!("cut its answer short: {kind}"),
    }
}

/// The most characters of a peer's own line [`reply_head`] quotes: what a
/// fleet read's row keeps whole beside the longest IPv4 address and port
/// and the reason's cause.
const REPLY_SHOWN_CHARS: usize = 60;

/// How many lines a Lines reply's head `OK <n>` promises. Public so the
/// reasons it gives are held to a fleet read's row where that row is made
/// (`aterm_ctl::UNANSWERED_REASON_CHARS`).
///
/// # Errors
///
/// A refusal or anything else is a reason ([`Unreached::Reply`]): its cause
/// first, then the peer's line, cut to [`REPLY_SHOWN_CHARS`], so a row cuts
/// nothing of it. An older aterm's refusal is its cause and remedy alone:
/// the verbs it serves add nothing to "update it".
pub fn reply_head(line: &str) -> Result<usize, String> {
    if let Some(n) = line
        .strip_prefix("OK ")
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
    {
        return Ok(n);
    }
    let shown = aterm_codec::tty::one_line(line, REPLY_SHOWN_CHARS);
    Err(if line.starts_with("ERR denied: the fleet serves only") {
        "serves the fleet no survey: an older aterm (update it)".to_owned()
    } else if line.starts_with("ERR ") {
        format!("refused the survey: {shown}")
    } else {
        format!("answered no survey: {shown}")
    })
}

#[cfg(test)]
#[path = "fleet_member_tests.rs"]
mod tests;
