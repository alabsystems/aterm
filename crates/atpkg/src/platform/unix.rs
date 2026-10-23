// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The Unix backend of [`crate::platform`].

use std::ffi::OsStr;
use std::fs::{self, File, Metadata, OpenOptions, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use aterm_types::fs_restricted::dir_safe_for_private_write;

/// Appended to a tool name to form its executable name.
pub const EXE_SUFFIX: &str = "";
/// Appended to a tool name to form its `bin/` shim name.
pub const SHIM_SUFFIX: &str = "";

/// The default install prefix under `home`.
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

/// SIGKILL the process group `leader` leads (a child started with `process_group(0)`), so whatever it started goes with it.
pub fn kill_process_group(leader: u32) {
    if let Ok(pgid) = libc::pid_t::try_from(leader) {
        // SAFETY: `killpg` takes two integers and touches no memory of ours.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
}

/// The kernel's start-time stamp for `pid` ([`crate::platform::Birth`]), used with the pid to detect reuse within the current boot.
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
    // SAFETY: `info` points at `size` writable bytes of exactly the structure PROC_PIDTBSDINFO fills.
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

/// This ACCOUNT's home directory, as the OS knows it.
#[must_use]
pub fn account_home() -> Option<PathBuf> {
    aterm_uds::account::home()
}

/// Mark `dir` as excluded from Time Machine via `com.apple.metadata:com_apple_backup_excludeItem`.
pub fn exclude_from_backup(dir: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    const ATTR: &[u8] = b"com.apple.metadata:com_apple_backup_excludeItem\0";
    const TRUE_BPLIST: &[u8] = &[
        0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0x09, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09,
    ];
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        // an interior NUL cannot name a real path
        return;
    };
    #[cfg(target_os = "macos")]
    {
        let mut have = [0u8; 64];
        // SAFETY: `path` and `ATTR` are NUL-terminated C strings that outlive the call, and `have` is a writable buffer of the stated length.
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
    // SAFETY: `path` and `ATTR` are valid C strings and the value points at a `'static` slice.
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
/// The two keys that switch it off.
#[cfg(target_os = "macos")]
pub const UNIVERSAL_CONTROL_KEYS: [&str; 2] = ["Disable", "DisableMagicEdges"];

/// Universal Control's per-host switches, as `defaults -currentHost read` answers.
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
        // No domain to read: a known absence of the feature, not a failed read.
        [crate::machine::Read::Absent, crate::machine::Read::Absent]
    }
}

/// Write both Universal Control switches true for the current host (`defaults -currentHost write com.apple.universalcontrol <key> -bool true`).
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

/// Whether Rosetta 2's runtime file is at `runtime` ([`crate::machine::ROSETTA_RUNTIME`]).
pub fn rosetta_present(runtime: &Path) -> Result<bool, String> {
    match fs::metadata(runtime) {
        Ok(meta) => Ok(meta.is_file()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The one Rosetta 2 install, program and arguments: `softwareupdate`'s Rosetta verb, which
/// installs it as the user who runs it, with no administrator password and no dialog
/// (measured 2026-10-18 and 2026-10-21 on macOS 27.0.1: uid 501, `installAuth=NO` in
/// `/var/log/install.log`, exit 0).
#[cfg(target_os = "macos")]
const ROSETTA_INSTALL: [&str; 3] = [
    "/usr/sbin/softwareupdate",
    "--install-rosetta",
    "--agree-to-license",
];

/// Install Rosetta 2 ([`ROSETTA_INSTALL`]) as this user, never through `sudo`, and wait at most `within` (killed past it).
pub fn rosetta_install(within: std::time::Duration) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let [program, args @ ..] = ROSETTA_INSTALL;
        let mut cmd = aterm_uds::spawnfd::command(program);
        cmd.args(args);
        let Some(out) = crate::doctor::output_bounded_capturing(&mut cmd, within) else {
            return Err(format!(
                "`softwareupdate --install-rosetta` did not start, or ran past {} s and was \
                 stopped",
                within.as_secs()
            ));
        };
        if out.status.success() {
            return Ok(());
        }
        let last_line = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .map(|l| l.chars().take(200).collect::<String>())
        };
        let said = last_line(&out.stderr)
            .or_else(|| last_line(&out.stdout))
            .unwrap_or_default();
        let code = out
            .status
            .code()
            .map_or_else(|| "on a signal".to_string(), |c| c.to_string());
        Err(if said.is_empty() {
            format!("`softwareupdate --install-rosetta` exited {code}")
        } else {
            format!("`softwareupdate --install-rosetta` exited {code}: {said}")
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = within;
        Err("Rosetta 2 is a macOS install".to_string())
    }
}

/// [`bounded_stdout`], keeping the child's success bit: `defaults read` exits 1 with empty
/// output for "this key is not set", an answer to tell apart from a read that did not run.
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
/// kills the child at `ceiling` (`aterm_uds::reaper` collects it once it ends) and reads its
/// stdout while it runs, never waiting on a process the child left behind holding the pipe.
#[cfg(target_os = "macos")]
fn bounded_stdout_inner(
    cmd: &mut aterm_uds::spawnfd::Command,
    ceiling: std::time::Duration,
) -> Option<(bool, Vec<u8>)> {
    crate::doctor::output_bounded_within(cmd, ceiling).map(|out| (out.status.success(), out.stdout))
}

/// The system-prefix counterpart of `ensure_private_dir`'s `0700`.
pub fn ensure_shared_dir(dir: &Path) -> std::io::Result<()> {
    if let Ok(md) = std::fs::symlink_metadata(dir)
        && md.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{}: store directory is a symlink; refusing", dir.display()),
        ));
    }
    aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o755)?;
    if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.mode() & 0o7777 == 0o755) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Whether a directory's metadata says it is system-owned.
#[must_use]
pub fn dir_meta_is_system(meta: &Metadata) -> bool {
    meta.uid() == 0 && meta.mode() & 0o022 == 0
}

/// Whether a directory's metadata says it is private-write-safe.
#[must_use]
pub fn dir_meta_is_private(meta: &Metadata) -> bool {
    dir_safe_for_private_write(our_uid(), meta.uid(), meta.mode())
}

/// Whether the CALLING user can write into `dir`.
#[must_use]
pub fn dir_writable_by_caller(dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        // An interior NUL cannot name a real directory.
        return false;
    };
    // SAFETY: `c` is a NUL-terminated C string that outlives the call.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Whether `meta` (from `symlink_metadata`) is a link-like indirection the fail-closed prefix chain check must not trust as a directory.
#[must_use]
pub fn is_reparse(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// Remove whatever indirection sits at `link`.
pub fn remove_link(link: &Path) {
    let _ = fs::remove_file(link);
}

/// Force a file to `0600`.
pub fn harden_file(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(0o600))
}

/// Set a file's permission bits to `mode`.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, Permissions::from_mode(mode))
}

/// Set an open file's permission bits to `mode` (`fchmod(2)`) for [`crate::extract::write_capped`].
pub fn set_mode_on(f: &File, mode: u32) -> io::Result<()> {
    f.set_permissions(Permissions::from_mode(mode))
}

/// The BSD file flags that stop an entry's owner changing or removing it.
#[cfg(target_os = "macos")]
pub const OWNER_BINDING_FLAGS: u32 =
    libc::UF_IMMUTABLE | libc::UF_APPEND | libc::SF_IMMUTABLE | libc::SF_APPEND;

/// Set `path`'s BSD file flags to exactly `flags` (`chflags(2)`, which follows a link).
#[cfg(target_os = "macos")]
pub fn set_file_flags(path: &Path, flags: u32) -> io::Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `c` is a NUL-terminated C string that outlives the call, which reads it and nothing else.
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

/// Create `path` exclusively (`O_CREAT|O_EXCL`) with mode `mode`, failing [`io::ErrorKind::AlreadyExists`] if it exists.
pub fn open_create_new_write(path: &Path, mode: u32) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
}

/// Open `path` for [`sync_file_contents`].
pub fn open_for_sync(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// Push one open file's contents out of the page cache.
pub fn sync_file_contents(f: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd as _;
    loop {
        // SAFETY: `f` is a live, open `File` borrowed for the whole call, so its raw fd is valid.
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

/// Whether this platform's filesystems store POSIX permission bits an `fstat` reads back.
pub const HAS_POSIX_MODES: bool = true;

/// The mode the extraction-time fold records for a file just written with `requested` bits.
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

/// Free bytes on the volume holding `dir` (which must exist), or `None` on any `statvfs` failure.
#[must_use]
pub fn volume_free_bytes(dir: &Path) -> Option<u64> {
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a valid C string and `&mut st` is a writable `statvfs` out-parameter.
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

/// Atomically point `link` at `target`.
pub fn atomic_symlink(target: &Path, link: &Path) -> io::Result<()> {
    let file_name = link
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "link has no file name"))?;
    let tmp = link.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    if let Err(e) = fs::rename(&tmp, link) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Install a bin shim at `shim` forwarding to `target`, exporting `env` first (empty: the plain shim).
pub fn install_shim_to_env(
    shim: &Path,
    target: &Path,
    env: &crate::shim_env::ShimEnv,
) -> io::Result<()> {
    crate::lay::write_in_process(&shim_executable_to_env(shim, target, env)?)
}

/// The shim [`install_shim_to_env`] lays, rendered but not written.
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

/// The `agents/` twin's shim.
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

/// Install a failing tombstone shim at `shim`.
pub fn install_tombstone_shim(shim: &Path, message: &str) -> io::Result<()> {
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' {} 1>&2\nexit 70\n",
        crate::stub::sh_single_quote(message)
    );

    crate::lay::write_in_process(&crate::lay::Executable::new(shim, script.as_bytes()))
}

/// Resolve the store/checkout target a `bin/<tool>` shim execs, or `None` for no shim or a tombstone (no `exec` line).
#[must_use]
pub fn resolve_shim(shim: &Path) -> Option<PathBuf> {
    if let Ok(target) = fs::read_link(shim) {
        return Some(target);
    }
    let content =
        crate::metadata_io::read_bounded_regular_utf8(shim, super::MAX_SHIM_BYTES).ok()?;
    super::parse_sh_shim_target(&content)
}

/// The environment the shim at `shim` exports before it execs.
#[must_use]
pub fn shim_env_of(shim: &Path) -> crate::shim_env::ShimEnv {
    match crate::metadata_io::read_bounded_regular_utf8(shim, super::MAX_SHIM_BYTES) {
        Ok(content) => super::parse_sh_shim_env(&content),
        Err(_) => crate::shim_env::ShimEnv::NONE,
    }
}

/// Replace the current process image with `command` (`execve`).
pub fn exec_or_run(command: &mut aterm_uds::spawnfd::StdCommand) -> io::Error {
    use std::os::unix::process::CommandExt as _;
    command.exec()
}

/// Wait for `child` while this process stands aside from the terminal's interrupt and quit keys, as `system(3)` does.
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
    // SAFETY: `signal(2)` with `SIG_IGN`, or with a handler that makes only async-signal-safe calls ([`forward_to_child`]).
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

/// [`wait_standing_aside`]'s handler: the signal sent on to the child it waits on, or, with
/// none (a handler outliving its wait), the default action.
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

/// Leave this process the way `status` says its child left.
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
        // SAFETY: the default action for these ends the process without a core.
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

    /// The Rosetta 2 probe is a `stat`: a file is present; nothing there, a missing
    /// parent, a parent that is a file, or a directory at the name is missing; a parent
    /// the probe may not search is no answer at all.
    #[test]
    fn the_rosetta_probe_reads_a_file_and_never_guesses_an_unsearchable_one() {
        let root = crate::testdir::dir("atpkg-rosetta-probe-");
        let runtime = root.join("oah").join("libRosettaRuntime");
        assert_eq!(rosetta_present(&runtime), Ok(false), "no parent");
        fs::create_dir_all(runtime.parent().unwrap()).unwrap();
        assert_eq!(rosetta_present(&runtime), Ok(false), "nothing there");
        fs::create_dir(&runtime).unwrap();
        assert_eq!(rosetta_present(&runtime), Ok(false), "a directory");
        fs::remove_dir(&runtime).unwrap();
        fs::write(&runtime, b"\xcf\xfa\xed\xfe").unwrap();
        assert_eq!(rosetta_present(&runtime), Ok(true));
        let under_a_file = runtime.join("libRosettaRuntime");
        assert_eq!(rosetta_present(&under_a_file), Ok(false), "ENOTDIR");
        let oah = runtime.parent().unwrap();
        fs::set_permissions(oah, Permissions::from_mode(0o000)).unwrap();
        if our_uid() != 0 {
            assert!(rosetta_present(&runtime).is_err(), "EACCES is no answer");
        }
        fs::set_permissions(oah, Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    /// A shared directory already `0755` is not rewritten (a same-mode `chmod(2)` moves
    /// `st_ctime`, which tippy's ancestor check reads); a drifted mode is forced to `0755`;
    /// a symlink at the name is refused and its target left alone.
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

    /// `account_home()` resolves on the machine running the test.
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
        assert_eq!(account_home(), Some(account));
    }
}
