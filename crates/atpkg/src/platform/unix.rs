// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Unix backend of [`crate::platform`]: symlink activation, `sh` exec-stub shims,
//! POSIX mode setting, `statvfs` free space, `getuid`-based ownership predicates and
//! `execve`.

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use aterm_types::fs_restricted::dir_safe_for_private_write;

/// Appended to a tool name to form the concrete executable name. Empty on Unix.
/// Applied ONLY by [`crate::store::ToolName::exe_file`]: both suffixes are `""` here, so a
/// hand-written append would go unnoticed until a Windows build.
pub const EXE_SUFFIX: &str = "";
/// Appended to a tool name to form the concrete `bin/` shim filename. Empty on Unix
/// (the shim is an exec stub named `<tool>`). Applied ONLY by
/// [`crate::store::ToolName::shim_file`] and stripped ONLY by
/// [`crate::store::ToolName::from_shim_file`].
pub const SHIM_SUFFIX: &str = "";

/// The default install prefix under `home`. On macOS
/// `…/Library/Application Support/aterm/pkg`, a sibling of the updater's
/// `Updates` dir (so the two share the hardened support root); on every other
/// Unix `…/.local/share/aterm/pkg` — the XDG data-dir default, because
/// `~/Library` is an Apple convention a Linux home has no business growing.
///
/// MIRRORED (deliberately, not depended on) by
/// `aterm-spec::verify::unix_store_bin_dir` — the verification tier's
/// discovery probes `<prefix>/bin` without dragging this crate into every
/// conformance consumer. Moving this prefix means updating that mirror too.
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".local").join("share").join("aterm").join("pkg")
    }
}

/// Our effective uid.
#[must_use]
pub fn our_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// SIGKILL the process group `leader` leads (a child started with `process_group(0)`),
/// so whatever it started goes with it. The caller must not have reaped `leader` yet,
/// or its id could have been reused. Best effort: a group already gone is not an error.
pub fn kill_process_group(leader: u32) {
    if let Ok(pgid) = libc::pid_t::try_from(leader) {
        // SAFETY: `killpg` takes two integers and touches no memory of ours.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
}

/// The kernel's start-time stamp for `pid` ([`crate::platform::Birth`]), used with the pid
/// to detect reuse within the current boot:
/// `proc_pidinfo(PROC_PIDTBSDINFO)`'s `pbi_start_tvsec/usec` in microseconds on macOS,
/// `/proc/<pid>/stat`'s `starttime` (clock ticks since boot) on Linux, the reading
/// `aterm_pty::process_birth` makes, which this crate's shipped graph does not link.
/// [`Birth::Gone`](crate::platform::Birth::Gone) for no such process or a zombie (libproc
/// refuses one with `ESRCH`); [`Birth::Unreadable`](crate::platform::Birth::Unreadable) when
/// the kernel would not say — a sandbox denying process-info (`EPERM`, while `kill(pid, 0)`
/// still answers), another account's process, a record about another pid, another Unix.
#[must_use]
pub fn process_birth(pid: u32) -> crate::platform::Birth {
    match libc::pid_t::try_from(pid).ok().filter(|p| *p > 0) {
        Some(pid) => process_birth_platform(pid),
        None => crate::platform::Birth::Gone,
    }
}

#[cfg(target_os = "macos")]
fn process_birth_platform(pid: libc::pid_t) -> crate::platform::Birth {
    use crate::platform::Birth;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let Ok(size) = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()) else {
        return Birth::Unreadable;
    };
    // SAFETY: `info` points at `size` writable bytes of exactly the structure
    // PROC_PIDTBSDINFO fills; libproc returns the number of bytes it wrote.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        // Read at once, before anything else can move errno.
        return if io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            Birth::Gone
        } else {
            Birth::Unreadable
        };
    }
    // SAFETY: the exact-size success above initialized the whole record.
    let info = unsafe { info.assume_init() };
    if u32::try_from(pid).ok() != Some(info.pbi_pid) {
        return Birth::Unreadable;
    }
    if info.pbi_status == libc::SZOMB {
        return Birth::Gone;
    }
    info.pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|us| us.checked_add(info.pbi_start_tvusec))
        .map_or(Birth::Unreadable, Birth::At)
}

#[cfg(target_os = "linux")]
fn process_birth_platform(pid: libc::pid_t) -> crate::platform::Birth {
    use crate::platform::Birth;
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Birth::Gone,
        Err(_) => return Birth::Unreadable,
    };
    // `comm` (field 2) is parenthesised and may hold spaces or `)`: the fields proper start
    // after the LAST `)`, field 3 (`state`) first and field 22 (`starttime`) twentieth.
    let Some(close) = stat.rfind(')') else {
        return Birth::Unreadable;
    };
    let mut fields = stat[close + 1..].split_ascii_whitespace();
    match fields.next() {
        Some("Z") => Birth::Gone,
        Some(_) => fields
            .nth(18)
            .and_then(|t| t.parse().ok())
            .map_or(Birth::Unreadable, Birth::At),
        None => Birth::Unreadable,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_birth_platform(_pid: libc::pid_t) -> crate::platform::Birth {
    crate::platform::Birth::Unreadable
}

/// This ACCOUNT's home directory, as the OS knows it — `getpwuid(getuid())->pw_dir` —
/// which is NOT `$HOME`.
///
/// The difference is load-bearing for the `[machine]` settings. `defaults` (cfprefsd)
/// resolves the per-host preference domain from the account, and IGNORES `$HOME`
/// (measured: `HOME=/tmp/x defaults -currentHost write com.aterm.probe …` landed in the
/// REAL `~/Library/Preferences/ByHost/`). So a process pointed at a synthetic home —
/// every integration test in this crate, a `sudo -H`, a CI job — would still write the
/// real account's Universal Control keys. Read through `/etc/passwd` this would be wrong
/// on macOS, where local accounts live in Open Directory and are absent from that file;
/// `getpwuid` asks the directory service.
///
/// `None` when the account cannot be resolved; the caller treats that as "cannot prove
/// this is the real home" and does not write.
#[must_use]
pub fn account_home() -> Option<PathBuf> {
    aterm_uds::account::home()
}

/// Mark `dir` as excluded from Time Machine and every backup tool that honours the
/// convention, by setting the `com.apple.metadata:com_apple_backup_excludeItem`
/// extended attribute.
///
/// The store is multiple GB of extracted toolchain that is re-downloadable and
/// signature-verifiable from the signed index; backing it up copies bytes the machine
/// can always reconstruct, and re-copies them on every update pass. It cannot simply
/// live in `~/Library/Caches` instead — a purge there would silently break an
/// installed toolchain, and the verified lane needs a stable path — so the exclusion
/// is set explicitly.
///
/// Best-effort and deliberately infallible: a volume or filesystem that refuses
/// extended attributes (a network home, an exFAT prefix) costs the user backup space
/// and nothing else. Never let a storage courtesy fail an install.
///
/// The value is the documented `bplist00` byte sequence for a `true` boolean, written
/// verbatim rather than via a plist encoder so this stays a dependency-free `setxattr`.
pub fn exclude_from_backup(dir: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    const ATTR: &[u8] = b"com.apple.metadata:com_apple_backup_excludeItem\0";
    // CFBoolean true, as a binary plist: header, the `true` marker (0x09), trailer.
    const TRUE_BPLIST: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, // "bplist00"
        0x09, // kCFBinaryPlistMarkerTrue
        0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x09,
    ];
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return; // an interior NUL cannot name a real path
    };
    // READ BEFORE WRITING. A `setxattr(2)` that stores the very value already there still
    // moves the directory's `st_ctime` (measured on APFS), and this runs on the prefix at
    // every store-lock acquisition — every `repair`, `gc`, install and update pass. tippy
    // snapshots the ctime of every ancestor of the executable it runs, so a re-write
    // aborts every tippy in flight ("ancestor … changed identity or contents").
    #[cfg(target_os = "macos")]
    {
        let mut have = [0u8; 64];
        // SAFETY: `path` and `ATTR` are NUL-terminated C strings that outlive the call, and
        // `have` is a writable buffer of the stated length; `getxattr` writes at most that
        // many bytes and returns how many it wrote, or -1.
        let n = unsafe {
            libc::getxattr(
                path.as_ptr(),
                ATTR.as_ptr().cast::<libc::c_char>(),
                have.as_mut_ptr().cast::<libc::c_void>(),
                have.len(),
                0,
                0,
            )
        };
        if usize::try_from(n).is_ok_and(|n| have.get(..n) == Some(TRUE_BPLIST)) {
            return;
        }
    }
    // SAFETY: `path` and `ATTR` are NUL-terminated C strings that outlive the call, the
    // value pointer/length describe a `'static` slice, and the return value is ignored
    // because failure is explicitly acceptable here.
    #[cfg(target_os = "macos")]
    unsafe {
        libc::setxattr(
            path.as_ptr(),
            ATTR.as_ptr().cast::<libc::c_char>(),
            TRUE_BPLIST.as_ptr().cast::<libc::c_void>(),
            TRUE_BPLIST.len(),
            0,
            0,
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Other Unixes have no Time Machine convention to honour.
        let _ = (path, ATTR, TRUE_BPLIST);
    }
}

/// The `defaults(1)` domain of macOS Universal Control, per-host (`-currentHost`).
#[cfg(target_os = "macos")]
pub const UNIVERSAL_CONTROL_DOMAIN: &str = "com.apple.universalcontrol";
/// The two keys that switch it off: the feature, and the screen-edge hand-off.
#[cfg(target_os = "macos")]
pub const UNIVERSAL_CONTROL_KEYS: [&str; 2] = ["Disable", "DisableMagicEdges"];

/// Universal Control's per-host switches, as `defaults -currentHost read` answers:
/// `[Disable, DisableMagicEdges]`, each `Some(true)` when set to a true value,
/// `Some(false)` when set false, `None` when the key is absent (the OS default —
/// `defaults` exits 1 with "does not exist") or could not be asked.
///
/// READ-ONLY. `/usr/bin/defaults` is named absolutely, never through `PATH`.
/// Non-macOS: `[None, None]`.
#[must_use]
pub fn universal_control_state() -> [crate::machine::Read<bool>; 2] {
    #[cfg(target_os = "macos")]
    {
        let read = |key: &str| {
            use crate::machine::Read;
            let mut cmd = aterm_uds::spawnfd::command("/usr/bin/defaults");
            cmd.arg("-currentHost")
                .arg("read")
                .arg(UNIVERSAL_CONTROL_DOMAIN)
                .arg(key);
            // ABSENT IS AN ANSWER; UNUSABLE IS NOT. `defaults read` of a key that is not
            // set exits 1 with EMPTY stdout — that is macOS saying "the OS default", and
            // it is the shape this feature is normally looking at. Anything else (the
            // binary would not run, the deadline killed it, a nonzero exit that still
            // printed something, or output this module cannot parse) taught us nothing,
            // and must not be folded into the same `None` the OS default arrives as.
            match bounded_exit_and_stdout(&mut cmd) {
                Some((true, out)) => match crate::machine::parse_defaults_bool(&out) {
                    Some(value) => Read::Value(value),
                    None => Read::Unusable(format!("`defaults read` printed no boolean for {key}")),
                },
                Some((false, out)) if out.is_empty() => Read::Absent,
                Some((false, _)) => Read::Unusable(format!(
                    "`defaults read` of {key} exited nonzero and printed something"
                )),
                None => Read::Unusable(format!(
                    "`defaults read` of {key} did not run or passed its deadline"
                )),
            }
        };
        [
            read(UNIVERSAL_CONTROL_KEYS[0]),
            read(UNIVERSAL_CONTROL_KEYS[1]),
        ]
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Not macOS: there is no domain to read, which is a measured absence of the
        // feature, not a failed read.
        [crate::machine::Read::Absent, crate::machine::Read::Absent]
    }
}

/// Write both Universal Control switches true for the current host (`defaults
/// -currentHost write com.apple.universalcontrol <key> -bool true`). `true` iff both
/// writes exited 0. No sudo: the domain is the user's. The revert is
/// [`crate::machine::UNIVERSAL_CONTROL_REVERT`]. Non-macOS: `false`, nothing run.
#[must_use]
pub fn universal_control_disable() -> bool {
    #[cfg(target_os = "macos")]
    {
        UNIVERSAL_CONTROL_KEYS.iter().all(|key| {
            let mut cmd = aterm_uds::spawnfd::command("/usr/bin/defaults");
            cmd.arg("-currentHost")
                .arg("write")
                .arg(UNIVERSAL_CONTROL_DOMAIN)
                .arg(key)
                .arg("-bool")
                .arg("true");
            bounded_stdout(&mut cmd).is_some()
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// [`bounded_stdout`], but keeping the child's SUCCESS bit instead of folding a nonzero
/// exit into `None`: `defaults read` uses exit 1 with empty output to mean "this key is
/// not set", a real answer the caller must tell apart from a read that did not happen.
#[cfg(target_os = "macos")]
fn bounded_exit_and_stdout(cmd: &mut aterm_uds::spawnfd::Command) -> Option<(bool, Vec<u8>)> {
    bounded_stdout_inner(cmd, BOUNDED_CEILING)
}

/// Run `cmd` with a bounded wall clock ([`BOUNDED_CEILING`]); its stdout on a zero exit,
/// `None` on a spawn failure, a non-zero exit or the deadline.
#[cfg(target_os = "macos")]
fn bounded_stdout(cmd: &mut aterm_uds::spawnfd::Command) -> Option<Vec<u8>> {
    match bounded_stdout_inner(cmd, BOUNDED_CEILING)? {
        (true, out) => Some(out),
        (false, _) => None,
    }
}

/// How long a production probe waits for its child before killing it and answering `None`.
#[cfg(target_os = "macos")]
const BOUNDED_CEILING: std::time::Duration = std::time::Duration::from_secs(5);

/// The deadline behind [`bounded_stdout`]: `crate::doctor::output_bounded_within`, which
/// kills and reaps the child at `ceiling` and reads its stdout while it runs, never waiting
/// on a process the child left behind holding the pipe.
#[cfg(target_os = "macos")]
fn bounded_stdout_inner(
    cmd: &mut aterm_uds::spawnfd::Command,
    ceiling: std::time::Duration,
) -> Option<(bool, Vec<u8>)> {
    crate::doctor::output_bounded_within(cmd, ceiling).map(|out| (out.status.success(), out.stdout))
}

/// The system-prefix counterpart of `ensure_private_dir`'s `0700`. A root-owned store
/// exists precisely so every user can execute out of it, so it must be readable and
/// traversable by all — while staying writable only by root, which is what the prefix
/// chain check enforces and what Trust's launcher predicate requires
/// (`mode & 0o022 == 0`). `0755` satisfies both; `0700` satisfies both checks too and
/// then denies every non-root user at exec time.
///
/// Fails closed on a symlink at the target, mirroring `ensure_private_dir`: a
/// pre-created link must never capture our writes.
pub fn ensure_shared_dir(dir: &Path) -> std::io::Result<()> {
    if let Ok(md) = std::fs::symlink_metadata(dir)
        && md.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{}: store directory is a symlink; refusing", dir.display()),
        ));
    }
    // Missing ancestors `0755` whatever the umask, as `ensure_private_dir` makes them: they
    // are on the chain `targo trust` refuses when any component is group/other-writable.
    aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o755)?;
    // Only a mode that differs is written: a same-mode `chmod(2)` still moves `st_ctime`,
    // which tippy's ancestor check reads (see `aterm_update_core::ensure_private_dir`).
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.mode() & 0o7777 == 0o755) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Whether a directory's metadata says it is SYSTEM-owned: owned by root (uid 0) and
/// not group/other-writable.
///
/// The second trusted chain shape, and the one a verified toolchain requires. The
/// `$HOME` chain check answers "no attacker-writable ancestor" by proving every
/// component is ours; a root-owned chain answers the same question at least as
/// strongly — nothing below root can swap a component. This shape is for a genuinely
/// shared multi-user store.
///
/// Trust's default `CallerOwned` mode admits a component owned by root OR by the invoking
/// identity (`process_authority.rs`; it REFUSES to authenticate at all when targo runs as
/// root), so a user-owned 0700 prefix proves fine too.
///
/// Deliberately NOT "root-owned OR ours": admitting our own uid here would let the
/// system prefix fall back to exactly the property the verified lane refuses.
#[must_use]
pub fn dir_meta_is_system(meta: &Metadata) -> bool {
    meta.uid() == 0 && meta.mode() & 0o022 == 0
}

/// Whether a directory's metadata says it is private-write-safe: owned by our uid and
/// not group/other-writable (the shared [`dir_safe_for_private_write`] predicate).
#[must_use]
pub fn dir_meta_is_private(meta: &Metadata) -> bool {
    dir_safe_for_private_write(our_uid(), meta.uid(), meta.mode())
}

/// Whether the CALLING user can write into `dir`.
///
/// This answers the question [`dir_meta_is_system`] does not. That a prefix chain is
/// *trusted* — every component root-owned and not group/other-writable — says nothing
/// about whether the caller can *use* it. A non-root user with a root-owned prefix
/// configured passes the trust check and then fails on `store.lock` for every verb,
/// forever.
///
/// Uses `access(2)` rather than re-deriving permission from the mode bits, so group
/// membership and ACLs are honoured by the kernel instead of approximated here. It
/// tests the REAL uid, which is what we want: atpkg is not setuid, and the question is
/// "can this user install", not "can this process momentarily write".
#[must_use]
pub fn dir_writable_by_caller(dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        // An interior NUL cannot name a real directory.
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated C string that outlives the call, and
    // `access` only reads it.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Whether `meta` (from `symlink_metadata`) is a link-like indirection that must NOT be
/// trusted as a real directory in the fail-closed prefix chain check. On Unix that is
/// exactly a symlink; the Windows backend also treats a directory **junction** (a reparse
/// point that reports `is_symlink() == false`) as disqualifying.
#[must_use]
pub fn is_reparse(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// Remove whatever indirection sits at `link`. On Unix a `store/<program>/current` link is a
/// symlink, so `remove_file` unlinks it (never following into the target). Best-effort.
pub fn remove_link(link: &Path) {
    let _ = fs::remove_file(link);
}

/// Force a file to `0600` (owner-only). The Unix hardening for the durable
/// floor/pin/links/cache state files.
pub fn harden_file(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(0o600))
}

/// Set a file's permission bits to `mode`.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(mode))
}

/// Set an OPEN file's permission bits to `mode`: `fchmod(2)` on the handle, which std
/// lowers `File::set_permissions` to.
///
/// The same bits as [`set_mode`], for a caller that still holds the file it just wrote
/// ([`crate::extract::write_capped`]): no second resolution of the path — which for a
/// staged toolchain is a deep `<prefix>/store/<program>/<build>.incoming-<pid>/lib/...`
/// walk paid once per file — and the mode lands on the inode we wrote by construction
/// rather than on whatever the name resolves to now.
pub fn set_mode_on(f: &File, mode: u32) -> io::Result<()> {
    f.set_permissions(Permissions::from_mode(mode))
}

/// The BSD file flags that stop an entry's OWNER changing or removing it — user and
/// system immutable and append-only (`chflags(1)`'s `uchg`, `uappnd`, `schg`, `sappnd`).
/// No archive lane lays a flag; `ditto` copies a bundle's with it (the `dmg` lane,
/// `install::vet_copied_tree`).
#[cfg(target_os = "macos")]
pub const OWNER_BINDING_FLAGS: u32 =
    libc::UF_IMMUTABLE | libc::UF_APPEND | libc::SF_IMMUTABLE | libc::SF_APPEND;

/// Set `path`'s BSD file flags to exactly `flags` (`chflags(2)`, which follows a link).
#[cfg(target_os = "macos")]
pub fn set_file_flags(path: &Path, flags: u32) -> io::Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `c` is a NUL-terminated C string that outlives the call, which reads it and
    // nothing else.
    if unsafe { libc::chflags(c.as_ptr(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Open `path` for a fresh (create+truncate) write with initial permission `mode`.
pub fn open_create_write(path: &Path, mode: u32) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)
}

/// Create `path` for writing with initial permission `mode`, failing
/// [`io::ErrorKind::AlreadyExists`] when the NAME already resolves to anything —
/// `O_CREAT|O_EXCL`, which follows no symlink and which the filesystem answers by its own
/// name equality (case-insensitive APFS, Unicode normalization), not by the bytes asked.
pub fn open_create_new_write(path: &Path, mode: u32) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
}

/// Open `path` the way [`sync_file_contents`] needs it: read-only — `fsync(2)` takes any
/// open descriptor, and a read-only open never fails on a file the store cannot write
/// (no lane lays one since AT3, 2026-10-10, when the `dmg` lane stopped keeping the
/// image's modes and owners; the read-only open asks for nothing it does not need).
pub fn open_for_sync(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// Push ONE open file's contents out of the page cache: a plain `fsync(2)`.
///
/// **Deliberately not [`File::sync_all`].** On Apple targets std lowers BOTH `sync_all`
/// and `sync_data` to `fcntl(F_FULLFSYNC)` — a full device-cache flush, a barrier of
/// milliseconds each however few bytes the file holds. The caller
/// ([`crate::store::sync_tree`]) flushes an entire staged toolchain, tens of thousands of
/// files for the ~3.4 GB `trust` member, so a drive flush per file would add minutes to
/// every install and buy only the difference between "the drive acknowledged the write"
/// and "the drive's cache is on platter". `fsync(2)` buys the property the store actually
/// needs and cannot do without: the tree's DATA reaches the filesystem before the renames
/// and the completeness marker that vouch for it become durable, so no crash can leave a
/// build marked installed over zero-length files.
///
/// `EINTR` is retried rather than reported, exactly as std's own `cvt_r` does: an
/// interrupted call is not a flush that failed.
pub fn sync_file_contents(f: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd as _;
    loop {
        // SAFETY: `f` is a live, open `File` borrowed for the whole call, so its raw fd is
        // valid; `fsync` takes that fd BY VALUE and dereferences no pointer.
        let rc = unsafe { libc::fsync(f.as_raw_fd()) };
        if rc == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// A file's permission bits (`st_mode`), as read by the tree-root hash and doctor.
#[must_use]
pub fn permission_mode(meta: &Metadata) -> u32 {
    meta.permissions().mode()
}

/// Whether this platform's filesystems store POSIX permission bits an `fstat` reads
/// back. Unix does, so the tree-root walk folds the inode's bits ([`permission_mode`])
/// and never a declared-mode record ([`crate::tree::tree_root_declared`]).
pub const HAS_POSIX_MODES: bool = true;

/// The mode slot the extraction-time fold records for a file just written with
/// `requested` bits: the bits READ BACK from the open handle (`meta`), masked to
/// `0o7777` — never the request — so a filesystem that stores something other than what
/// it was asked for moves the root and the stage fails closed. Unix stores them
/// verbatim (`fchmod`), so this equals `requested` on every filesystem atpkg has met.
#[must_use]
pub fn folded_mode(meta: &Metadata, requested: u32) -> u32 {
    let _ = requested;
    permission_mode(meta) & 0o7777
}

/// The raw OS bytes of an `OsStr` (no lossy conversion), for the tree-root path hash.
#[must_use]
pub fn os_str_bytes(s: &OsStr) -> &[u8] {
    s.as_bytes()
}

/// Free bytes on the volume holding `dir` (which must EXIST), or `None` on any
/// `statvfs` failure. Uses `f_bavail` — blocks free to an UNPRIVILEGED user (correct:
/// atpkg never runs as root) — times the fragment size, saturating so a pathological
/// filesystem can never wrap to a bogus "fits".
#[must_use]
pub fn volume_free_bytes(dir: &Path) -> Option<u64> {
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a NUL-terminated C string that outlives the call, and `&mut st` is a
    // valid, writable out-param of the exact `statvfs` type the libc call expects.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    let frsize = if st.f_frsize != 0 {
        st.f_frsize
    } else {
        st.f_bsize
    };
    Some((st.f_bavail as u64).saturating_mul(frsize as u64))
}

/// Atomically point `link` at `target`: create a sibling temp symlink and `rename(2)` it
/// over `link`. `rename` is atomic on POSIX, so the swap has no window where `link` is
/// missing or partially written — even if a previous `link` already existed. The
/// directory-indirection primitive behind `store/<program>/current` and the sysroot dir links.
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    let file_name = link
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "link has no file name"))?;
    let tmp = link.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
    // A leftover temp from a crashed run must not block us.
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    // Atomic replace. On failure, clean the temp so it can't accumulate.
    if let Err(e) = fs::rename(&tmp, link) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Install a bin shim at `shim` forwarding to `target`, exporting `env` first (design
/// S7; an empty `env` is the plain shim). The `(shim, target)` form without an
/// environment is [`super::install_shim_to`].
pub fn install_shim_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    crate::lay::write_in_process(&shim_executable_to_env(shim, target, env)?)
}

/// The shim [`install_shim_to_env`] lays, RENDERED but not written: an EXEC STUB, not a
/// symlink (`platform::sh_shim_content` says why). Written by [`crate::lay`] —
/// temp+rename, `0755` — so a shim on the user's PATH is never briefly absent or
/// half-written.
///
/// ROUTED WHEN AN EXEC ROOT STANDS. The body carries the guard line of
/// `platform::sh_shim_content_routed` exactly when [`crate::compat::route_for_shim`]
/// finds a complete exec root for the trust build `target` lies in — `stat` only, and
/// `None` for every other program, every build without a root and every target outside
/// this shim's prefix. Deciding it HERE, in the one function every shim writer renders
/// through (`install_tools_env`, the alias and agents reconciles, flow's restore,
/// linkmode's unlink), keeps a writer from laying a plain shim beside a root that the
/// next reconcile would have to route again; with no root the body is the plain shim.
pub fn shim_executable_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<crate::lay::Executable> {
    let route = crate::compat::route_for_shim(shim, target);
    Ok(crate::lay::Executable::new(
        shim,
        super::sh_shim_content_routed(target, env, route.as_deref()),
    ))
}

/// The `agents/` twin's shim: [`shim_executable_to_env`] with the twin's `prelude`
/// ([`super::twin_prelude`], the self-update block) ahead of its exports — the same
/// route decision, the same writer. `prelude` empty is byte for byte the `bin/` shim.
pub fn twin_executable_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> io::Result<crate::lay::Executable> {
    let route = crate::compat::route_for_shim(shim, target);
    Ok(crate::lay::Executable::new(
        shim,
        super::sh_shim_content_twin(target, env, route.as_deref(), prelude),
    ))
}

/// Lay the `agents/` twin [`twin_executable_to_env`] renders.
pub fn install_twin_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
    prelude: &str,
) -> io::Result<()> {
    crate::lay::write_in_process(&twin_executable_to_env(shim, target, env, prelude)?)
}

/// Install a **failing tombstone shim** at `shim` — a tiny `sh` script that prints
/// `message` to stderr and exits 70 (`EX_SOFTWARE`). Written atomically (temp `0755` +
/// `rename(2)`), replacing whatever shim (symlink or file) was there.
pub fn install_tombstone_shim(shim: &Path, message: &str) -> io::Result<()> {
    // The failing script. `printf '%s\n' <quoted>` keeps the message a fixed format with the
    // (quoted) tool-bearing text as a separate arg — no format-string or shell injection — and
    // exits 70 (EX_SOFTWARE), a clear nonzero.
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' {} 1>&2\nexit 70\n",
        crate::stub::sh_single_quote(message)
    );

    // Atomic install through the one executable writer (`crate::lay`): a sibling temp,
    // `0755`, then `rename(2)` over `shim` — atomic on POSIX and replacing the
    // destination regardless of its prior type (symlink or regular file), so the live
    // shim flips to the tombstone with no torn window.
    crate::lay::write_in_process(&crate::lay::Executable::new(shim, script.as_bytes()))
}

/// Resolve the store/checkout target a `bin/<tool>` shim execs, or `None` for no shim or
/// a TOMBSTONE (no `exec` line). `active_builds`, `prune_stale_shims` and gc's retention
/// all key on it. A symlink shim from an older atpkg still resolves until the next
/// activation rewrites it as a stub.
#[must_use]
pub fn resolve_shim(shim: &Path) -> Option<PathBuf> {
    if let Ok(target) = fs::read_link(shim) {
        return Some(target);
    }
    let content =
        crate::metadata_io::read_bounded_regular_utf8(shim, super::MAX_SHIM_BYTES).ok()?;
    super::parse_sh_shim_target(&content)
}

/// The environment the shim at `shim` exports before it execs — its `export` lines,
/// parsed back by [`super::parse_sh_shim_env`] (fail-closed: NONE for a symlink an older
/// atpkg left, a tombstone, a pending stub, or anything the rule refuses).
#[must_use]
pub fn shim_env_of(shim: &Path) -> crate::shim_env::ShimEnv {
    match crate::metadata_io::read_bounded_regular_utf8(shim, super::MAX_SHIM_BYTES) {
        Ok(content) => super::parse_sh_shim_env(&content),
        Err(_) => crate::shim_env::ShimEnv::NONE,
    }
}

/// Replace the current process image with `command` (`execve`); returns only on failure.
pub fn exec_or_run(command: &mut aterm_uds::spawnfd::StdCommand) -> io::Error {
    use std::os::unix::process::CommandExt as _;
    // `exec` replaces this process and never returns on success; the returned value is
    // the error that PREVENTED the exec.
    command.exec()
}

/// Wait for `child` while this process STANDS ASIDE from the terminal's interrupt and quit
/// keys — the rule `system(3)` keeps: a ^C typed at the terminal reaches the whole
/// foreground group, the child decides what it means, and this process lives on to collect
/// its status. What `aterm pkg lease` waits with ([`crate::lease::hold_for`]): a holder that
/// died of the ^C its child was still cleaning up after would drop the lease under it, and
/// hand the shell its prompt back over the child's last lines.
///
/// A SIGTERM or SIGHUP sent to this process alone is PASSED ON to the child, and this
/// process waits on. Those are how a supervisor ends a run — `kill
/// <pid>`, `timeout`, a closed tab — and the pid it holds is the holder's: a packer
/// `exec`s into `aterm pkg lease`. Died of it, the holder let the lease go while the
/// command ran on UNLEASED and unwatched (measured: holder exit 143, its `sh` child still
/// looping, the build no longer leased), and a pack its caller had stopped went on to its
/// upload. Passed on, the command ends the way it would have with no holder between, and
/// [`exit_code_like`] then leaves this process by the same signal. A signal this process
/// was started ignoring (`nohup`) stays ignored; ^C and quit are NOT passed on, since the
/// terminal already sent them to the whole group and a second would read as a second key.
///
/// The dispositions are set AFTER the spawn — an ignored signal is inherited across `exec`,
/// and the child must meet the key with its own — and put back before this returns. So a
/// SIGINT, SIGQUIT, SIGTERM or SIGHUP that arrives between the spawn and the ignore still
/// acts on this process; closing that needs the ignore set before the spawn, with the child
/// resetting the signals that were not ignored on entry (`POSIX_SPAWN_SETSIGDEF`) through
/// `aterm_uds::spawnfd`, which no spawn path of it offers yet (it resets SIGPIPE alone).
///
/// # Errors
/// The wait's.
pub fn wait_standing_aside(
    child: &mut aterm_uds::spawnfd::TransparentChild,
) -> io::Result<std::process::ExitStatus> {
    use std::sync::atomic::Ordering;
    let pid = libc::pid_t::try_from(child.id()).unwrap_or(0);
    FORWARD_TO.store(pid, Ordering::SeqCst);
    let forward = forward_to_child as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: `signal(2)` with `SIG_IGN`, or with a handler that makes only
    // async-signal-safe calls ([`forward_to_child`]); the previous dispositions are kept
    // to be restored below.
    let previous = unsafe {
        [
            (libc::SIGINT, libc::signal(libc::SIGINT, libc::SIG_IGN)),
            (libc::SIGQUIT, libc::signal(libc::SIGQUIT, libc::SIG_IGN)),
            (libc::SIGTERM, libc::signal(libc::SIGTERM, forward)),
            (libc::SIGHUP, libc::signal(libc::SIGHUP, forward)),
        ]
    };
    for (sig, was) in &previous[2..] {
        if *was == libc::SIG_IGN {
            // SAFETY: puts back the `SIG_IGN` this process was started with.
            unsafe { libc::signal(*sig, libc::SIG_IGN) };
        }
    }
    let status = child.wait();
    // The child is reaped: its pid is free for the kernel to hand out again, so nothing is
    // sent to it from here on.
    let _ = FORWARD_TO.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
    for (sig, was) in previous {
        if was != libc::SIG_ERR {
            // SAFETY: puts back the disposition this function replaced a moment ago.
            unsafe { libc::signal(sig, was) };
        }
    }
    status
}

/// The child [`wait_standing_aside`] passes SIGTERM and SIGHUP on to; `0` while it waits on
/// none.
static FORWARD_TO: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// [`wait_standing_aside`]'s handler: the signal, sent on to the child it waits on — or,
/// with none (a handler outliving its wait), the default action, as if never installed.
extern "C" fn forward_to_child(sig: libc::c_int) {
    let pid = FORWARD_TO.load(std::sync::atomic::Ordering::SeqCst);
    // SAFETY: `kill`, `signal` and `raise` are async-signal-safe.
    unsafe {
        if pid > 0 {
            libc::kill(pid, sig);
        } else {
            libc::signal(sig, libc::SIG_DFL);
            #[expect(
                clippy::disallowed_methods,
                reason = "re-raises SIGTERM or SIGHUP (the two it forwards) with the default action: neither dumps core"
            )]
            libc::raise(sig);
        }
    }
}

/// Leave this process the way `status` says its child left: the child's exit code, or — for
/// a child ended by SIGINT, SIGTERM, SIGHUP or SIGPIPE — the same signal, raised on this
/// process with its default action, so the shell that ran it sees what it would have seen
/// without a holder between them (bash stops a script whose foreground command died of
/// SIGINT; it carries on past one that exited 130). Any other signal is `128 + n`: its
/// default action may dump core, and a holder's core is nobody's evidence. Never returns
/// for a raised signal.
#[must_use]
pub fn exit_code_like(status: std::process::ExitStatus) -> std::process::ExitCode {
    use std::os::unix::process::ExitStatusExt as _;
    if let Some(code) = status.code() {
        return std::process::ExitCode::from(u8::try_from(code & 0xff).unwrap_or(1));
    }
    let Some(sig) = status.signal() else {
        return std::process::ExitCode::from(1);
    };
    if matches!(
        sig,
        libc::SIGINT | libc::SIGTERM | libc::SIGHUP | libc::SIGPIPE
    ) {
        // SAFETY: the default action for these ends the process without a core; `raise`
        // delivers it to this thread before it returns.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            #[expect(
                clippy::disallowed_methods,
                reason = "re-raises SIGINT, SIGTERM, SIGHUP or SIGPIPE: none dumps core"
            )]
            libc::raise(sig);
        }
    }
    std::process::ExitCode::from(u8::try_from(128 + sig).unwrap_or(255))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// A SHARED DIRECTORY ALREADY `0755` IS NOT REWRITTEN. `ensure_shared_dir` runs on
    /// every store-lock acquisition over a system prefix, and a same-mode `chmod(2)` still
    /// moves `st_ctime` — which tippy's ancestor check reads, so a `repair` or `gc` aborted
    /// every tippy running from that store. A second call moves no stamp; a drifted mode
    /// (too tight, group-writable, a sticky bit) is still forced to `0755`; a symlink at
    /// the name is refused and the directory it names left as it was.
    #[test]
    fn a_shared_dir_already_0755_is_not_rewritten_and_a_drifted_one_is_forced() {
        let root = crate::testdir::dir("atpkg-shared-dir-");
        let dir = root.join("prefix");
        let stamp = |p: &Path| {
            let m = fs::symlink_metadata(p).unwrap();
            (
                m.mode() & 0o7777,
                m.ctime(),
                m.ctime_nsec(),
                m.mtime(),
                m.mtime_nsec(),
            )
        };
        ensure_shared_dir(&dir).unwrap();
        let first = stamp(&dir);
        assert_eq!(first.0, 0o755, "created shared: {:o}", first.0);
        std::thread::sleep(std::time::Duration::from_millis(20));
        ensure_shared_dir(&dir).unwrap();
        assert_eq!(
            stamp(&dir),
            first,
            "an unchanged 0755 directory was written"
        );
        for drifted in [0o700, 0o775, 0o1755] {
            fs::set_permissions(&dir, Permissions::from_mode(drifted)).unwrap();
            ensure_shared_dir(&dir).unwrap();
            assert_eq!(stamp(&dir).0, 0o755, "{drifted:o} not forced to 0755");
        }
        let link = root.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
        let refused = ensure_shared_dir(&link);
        let named = stamp(&dir).0;
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            refused.map_err(|e| e.kind()),
            Err(io::ErrorKind::PermissionDenied)
        );
        assert_eq!(named, 0o700, "the refused link's target was chmodded");
    }

    /// THE FAIL-CLOSED KEYSTONE, MEASURED ON THE MACHINE RUNNING THE TEST.
    ///
    /// `account_home()` decides whether the `[machine]` settings apply at all: a
    /// `None` here means "the account could not be resolved", which the guard reads as
    /// a synthetic machine and refuses. So any regression in the FFI — a buffer too
    /// small for a long gecos (ERANGE), a changed passwd binding, an empty `pw_dir` —
    /// silently turns the default-on doctor OFF for every user, on every launch, with
    /// no failing test anywhere. Nothing else asserts it resolves.
    #[test]
    fn the_account_home_resolves_on_the_machine_running_this_test() {
        let account = account_home().expect(
            "getpwuid_r must resolve this account's home; a None here disables the \
             [machine] doctor everywhere",
        );
        assert!(
            account.is_absolute(),
            "a passwd home is an absolute path: {}",
            account.display()
        );
        assert!(
            !account.as_os_str().is_empty(),
            "an empty pw_dir is not a home"
        );
        assert!(
            account.is_dir(),
            "the account home must exist: {}",
            account.display()
        );
        // Called twice, it answers the same: the buffer is sized once per call and a
        // partial read would show up here as a truncated second answer.
        assert_eq!(account_home(), Some(account));
    }
}
