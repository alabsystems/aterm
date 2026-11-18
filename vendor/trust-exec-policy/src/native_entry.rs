//! Local Darwin trusted-entry restriction, before untrusted native constructors.
//! This grants no Ready, runtime provenance, resource or provider authority.

use std::fs::File;
use std::io;
use std::mem::{MaybeUninit, size_of, size_of_val};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

use crate::{RestrictedNativeImage, readonly_image, restrict_current_to_readonly_image};

const CENSUS_CAPACITY: usize = 1024;

mod bootstrap_port;
mod registered_ports;

/// Revoke the current task's registered inheritance stash and bootstrap
/// special-port slot/cache, including current local SEND references to those
/// actual names. Overlap is retained until both kernel stashes are cleared.
/// Blockable signals remain blocked. This is a local capability restriction,
/// not a seal, owner, service-closure observation or execution permission.
/// Other special/exception ports and ways to reacquire authority
/// remain separate obligations; there is no general Mach capability claim.
///
/// # Safety
/// Call only in a dedicated trusted sole-main-thread entry before untrusted
/// code. Its bootstrap and registered references must be disposable, with no
/// concurrent Mach mutator or Rust owner of them. Every error, including partial revocation,
/// requires termination under the outside native owner; never retry, restore
/// signals/ports, or fall back to an ordinary payload after failure.
pub unsafe fn revoke_current_bootstrap_and_registered_ports() -> io::Result<()> {
    single_main_thread()?;
    block_signals()?;
    let bootstrap = bootstrap_port::Captured::capture()?;
    registered_ports::revoke_with_bootstrap(Some(&bootstrap))?;
    bootstrap.revoke()?;
    single_main_thread()
}

/// Retains exactly the one control endpoint and read-only loader image. This
/// local guard authenticates neither the image bytes nor a complete runtime.
pub struct RestrictedNativeEntry {
    control: UnixStream,
    image: RestrictedNativeImage,
}

impl RestrictedNativeEntry {
    pub fn control(&self) -> &UnixStream {
        &self.control
    }

    pub fn image(&self) -> &RestrictedNativeImage {
        &self.image
    }
}

/// Revoke ambient descriptors, then install the fixed read-only image policy.
/// The environment must already be empty. The control must be a connected Unix
/// stream whose kernel peer is the expected current parent of this euid. Every
/// other descriptor, including standard I/O and aliases, is closed. All blockable
/// signals remain blocked; the outside owner must retain SIGKILL/wait custody.
/// The task's registered inheritance stash and current local send-name copies
/// are revoked too. Special/exception/bootstrap and other Mach capabilities,
/// including possible routes to reacquire a port, remain unqualified.
///
/// The complete native runtime, immutable backing/mount and image bytes still
/// require independent authentication. This does not establish complete Mach
/// capability revocation or prove the absence of preexisting mappings, threads
/// created later, or other runtime capabilities. No readiness, loader,
/// process-lifetime or proof receipt follows.
///
/// # Safety
/// Call only in a dedicated trusted native entry, on its sole main thread before
/// untrusted constructors/code. No other live Rust owner may own a descriptor
/// except the two moved here. Registered Mach names must be disposable; no
/// concurrent Mach-port/descriptor/environment mutation or untrusted signal
/// handler may exist. The caller must terminate the process on
/// EVERY error, including a partial irreversible restriction; never retry, restore
/// descriptors/signals, or continue an ordinary payload after failure.
pub unsafe fn restrict_current_native_entry(
    image: File,
    control: UnixStream,
    expected_parent: u32,
) -> io::Result<RestrictedNativeEntry> {
    single_main_thread()?;
    if std::env::vars_os().next().is_some() {
        return Err(invalid("native entry requires an empty environment"));
    }
    readonly_image::validate_image(&image)?;
    validate_control(&control, expected_parent)?;
    // Establish this before the immutable descriptor snapshot and first fixed
    // profile. Worker I/O borrows this endpoint without dup/ioctl/shutdown.
    let flags = unsafe { libc::fcntl(control.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this sole trusted entry exclusively owns the live endpoint.
    if unsafe { libc::fcntl(control.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: read back flags on the same retained endpoint before census.
    if unsafe { libc::fcntl(control.as_raw_fd(), libc::F_GETFL) } != flags | libc::O_NONBLOCK {
        return Err(invalid(
            "native entry control nonblocking setup did not persist",
        ));
    }
    let retained = [control.as_raw_fd(), image.as_raw_fd()];
    if retained[0] == retained[1] {
        return Err(invalid("native entry roles must have distinct descriptors"));
    }
    let identities = [
        Identity::capture(retained[0])?,
        Identity::capture(retained[1])?,
    ];
    block_signals()?;
    registered_ports::revoke()?;
    close_ambient(retained, identities)?;
    validate_control(&control, expected_parent)?;
    single_main_thread()?;
    // This is the first and only sandbox installation; native qualification
    // refuses attempts to tighten an earlier no-fork profile on this platform.
    let image = restrict_current_to_readonly_image(image)?;
    // Native profile setup can leave a new socket in a freed low FD slot.
    // Revoke it under the unchanged profile before any untrusted constructor;
    // the original before-profile census is not the final capability boundary.
    validate_control(&control, expected_parent)?;
    single_main_thread()?;
    close_ambient(retained, identities)?;
    Ok(RestrictedNativeEntry { control, image })
}

fn close_ambient(retained: [RawFd; 2], identities: [Identity; 2]) -> io::Result<()> {
    let before = census()?;
    for fd in retained {
        if !before.as_slice().contains(&fd) {
            return Err(invalid(
                "native entry retained descriptor absent from kernel census",
            ));
        }
    }
    for &fd in before.as_slice() {
        if !retained.contains(&fd) {
            // SAFETY: the trusted sole-thread entry owns the descriptor table;
            // caller guarantees no other Rust owner, signal or concurrent user.
            // Never retry EINTR against a potentially recycled descriptor number.
            if unsafe { libc::close(fd) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    let after = census()?;
    if after.len != retained.len() || after.as_slice().iter().any(|fd| !retained.contains(fd)) {
        return Err(invalid("native entry retained descriptor set changed"));
    }
    if [
        Identity::capture(retained[0])?,
        Identity::capture(retained[1])?,
    ] != identities
    {
        return Err(invalid("native entry retained descriptor identity changed"));
    }
    Ok(())
}

pub(super) fn single_main_thread() -> io::Result<()> {
    let mut info = MaybeUninit::<libc::proc_taskinfo>::uninit();
    // SAFETY: query only our own task into complete native writable storage.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size_of::<libc::proc_taskinfo>() as libc::c_int,
        )
    };
    if written != size_of::<libc::proc_taskinfo>() as libc::c_int {
        return Err(io::Error::other("native entry thread census unavailable"));
    }
    // SAFETY: complete successful native query initialized info; pthread query
    // changes no state. A libtest worker thread is deliberately not an entry.
    if unsafe { info.assume_init() }.pti_threadnum != 1 || unsafe { libc::pthread_main_np() } != 1 {
        return Err(invalid("native entry requires its sole main thread"));
    }
    Ok(())
}

fn validate_control(control: &UnixStream, expected_parent: u32) -> io::Result<()> {
    let expected = i32::try_from(expected_parent)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| invalid("native entry parent identity is invalid"))?;
    let fd = control.as_raw_fd();
    let identity = Identity::capture(fd)?;
    if identity.mode & libc::S_IFMT != libc::S_IFSOCK
        || identity.flags & libc::O_ACCMODE != libc::O_RDWR
    {
        return Err(invalid("native entry control must be a read/write socket"));
    }
    let mut kind = 0_i32;
    let mut length = size_of::<i32>() as libc::socklen_t;
    // SAFETY: complete writable native integer and length, retained descriptor.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &mut length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != size_of::<i32>() || kind != libc::SOCK_STREAM {
        return Err(invalid(
            "native entry control must be a connected Unix stream",
        ));
    }
    let mut address = MaybeUninit::<libc::sockaddr_un>::zeroed();
    let mut length = size_of::<libc::sockaddr_un>() as libc::socklen_t;
    // SAFETY: complete native address storage; the successful query initializes
    // its returned prefix. The family is inspected only when that prefix fits.
    if unsafe { libc::getpeername(fd, address.as_mut_ptr().cast(), &mut length) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if (length as usize) < size_of::<libc::sa_family_t>() + 1
        || length as usize > size_of::<libc::sockaddr_un>()
        || unsafe { address.assume_init() }.sun_family != libc::AF_UNIX as libc::sa_family_t
    {
        return Err(invalid(
            "native entry control must be a connected Unix stream",
        ));
    }
    let mut peer = 0_i32;
    let mut length = size_of::<i32>() as libc::socklen_t;
    // SAFETY: Darwin sys/un.h defines SOL_LOCAL0, LOCAL_PEERPID2 and pid_t32.
    if unsafe { libc::getsockopt(fd, 0, 2, (&raw mut peer).cast(), &mut length) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: writable credential storage and retained socket; queries only.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != size_of::<i32>()
        || peer != expected
        || unsafe { libc::getppid() } != expected
        || uid != unsafe { libc::geteuid() }
    {
        return Err(invalid(
            "native entry control does not belong to its expected parent/euid",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: libc::dev_t,
    inode: libc::ino_t,
    mode: libc::mode_t,
    uid: libc::uid_t,
    gid: libc::gid_t,
    flags: libc::c_int,
}

impl Identity {
    fn capture(fd: RawFd) -> io::Result<Self> {
        let mut metadata = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: retained descriptor and complete native writable storage.
        if unsafe { libc::fstat(fd, metadata.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful native fstat initialized complete metadata.
        let metadata = unsafe { metadata.assume_init() };
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            device: metadata.st_dev,
            inode: metadata.st_ino,
            mode: metadata.st_mode,
            uid: metadata.st_uid,
            gid: metadata.st_gid,
            flags,
        })
    }
}

struct Census {
    descriptors: [RawFd; CENSUS_CAPACITY],
    len: usize,
}

impl Census {
    fn as_slice(&self) -> &[RawFd] {
        &self.descriptors[..self.len]
    }
}

fn census() -> io::Result<Census> {
    let mut entries = [libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    }; CENSUS_CAPACITY];
    // SAFETY: bounded complete storage, own-process query only. A full or
    // malformed response is refused rather than interpreted as complete.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            entries.as_mut_ptr().cast(),
            size_of_val(&entries) as libc::c_int,
        )
    };
    let written = usize::try_from(written)
        .ok()
        .filter(|n| {
            *n > 0 && *n < size_of_val(&entries) && *n % size_of::<libc::proc_fdinfo>() == 0
        })
        .ok_or_else(|| {
            io::Error::other("native entry descriptor census unavailable or truncated")
        })?;
    let mut census = Census {
        descriptors: [0; CENSUS_CAPACITY],
        len: 0,
    };
    for entry in &entries[..written / size_of::<libc::proc_fdinfo>()] {
        if entry.proc_fd < 0 || census.as_slice().contains(&entry.proc_fd) {
            return Err(io::Error::other("native entry descriptor census malformed"));
        }
        census.descriptors[census.len] = entry.proc_fd;
        census.len += 1;
    }
    Ok(census)
}

fn block_signals() -> io::Result<()> {
    let mut all = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: complete native writable mask; sigfillset initializes it.
    if unsafe { libc::sigfillset(all.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: sole trusted thread, initialized complete mask. The mask is never
    // restored, including on failure; caller must terminate every partial entry.
    let result =
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, all.as_ptr(), std::ptr::null_mut()) };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
