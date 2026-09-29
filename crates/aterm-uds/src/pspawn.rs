// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Start a program that inherits NO descriptor but stdio: `posix_spawn(2)` with
//! Darwin's `POSIX_SPAWN_CLOEXEC_DEFAULT`, which marks every descriptor the
//! caller did not name close-on-exec INSIDE the spawn, atomically.
//!
//! The PTY keeper holds a custody copy of every terminal's master. The one
//! program it ever starts is `/usr/bin/open -a <its own bundle>`
//! (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.1), and a master leaking into that
//! child — or into the app `open` hands off to — would be a second holder no
//! holder scan was asked about. A `fork` + strip in `pre_exec` has a window a
//! concurrently opened descriptor can slip through; this flag has none.
//!
//! stdin/stdout/stderr are replaced by `/dev/null` in the child, so nothing the
//! keeper's own stdio is attached to reaches the program either.
//!
//! Darwin only; elsewhere [`spawn_cloexec_default`] answers `Unsupported`
//! (Linux has no such flag, and no keeper runs there yet).

use std::io;

/// Spawn `program` with `args` (argv[1..]) and the caller's environment,
/// inheriting no descriptor but a `/dev/null` stdio. Returns the child's pid;
/// the CALLER reaps it (`waitpid`), as a single-threaded loop does on its next
/// turn.
///
/// # Errors
/// `InvalidInput` for an argument holding a NUL; anything `posix_spawn`
/// reports; `Unsupported` off Darwin.
#[cfg(target_vendor = "apple")]
// Skip: bottoms out at the `posix_spawn` family of FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
pub fn spawn_cloexec_default(program: &str, args: &[&str]) -> io::Result<u32> {
    use std::ffi::CString;
    type Attr = *mut core::ffi::c_void;
    type Actions = *mut core::ffi::c_void;
    // spawn.h, measured with the SDK 2026-09-28.
    const POSIX_SPAWN_CLOEXEC_DEFAULT: i16 = 0x4000;
    const POSIX_SPAWN_SETSIGDEF: i16 = 0x4;
    const POSIX_SPAWN_SETSIGMASK: i16 = 0x8;
    const O_RDWR: i32 = 2;
    unsafe extern "C" {
        fn posix_spawnattr_init(attr: *mut Attr) -> i32;
        fn posix_spawnattr_destroy(attr: *mut Attr) -> i32;
        fn posix_spawnattr_setflags(attr: *mut Attr, flags: i16) -> i32;
        fn posix_spawnattr_setsigmask(attr: *mut Attr, mask: *const u32) -> i32;
        fn posix_spawnattr_setsigdefault(attr: *mut Attr, mask: *const u32) -> i32;
        fn posix_spawn_file_actions_init(actions: *mut Actions) -> i32;
        fn posix_spawn_file_actions_destroy(actions: *mut Actions) -> i32;
        fn posix_spawn_file_actions_addopen(
            actions: *mut Actions,
            fd: i32,
            path: *const core::ffi::c_char,
            oflag: i32,
            mode: u16,
        ) -> i32;
        fn posix_spawn(
            pid: *mut i32,
            path: *const core::ffi::c_char,
            actions: *const Actions,
            attr: *const Attr,
            argv: *const *const core::ffi::c_char,
            envp: *const *const core::ffi::c_char,
        ) -> i32;
        static environ: *const *const core::ffi::c_char;
    }
    let nul = |_| io::Error::new(io::ErrorKind::InvalidInput, "argument holds a NUL");
    let program_c = CString::new(program).map_err(nul)?;
    let mut owned: Vec<CString> = vec![program_c.clone()];
    for arg in args {
        owned.push(CString::new(*arg).map_err(nul)?);
    }
    let mut argv: Vec<*const core::ffi::c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    argv.push(std::ptr::null());
    let dev_null = CString::new("/dev/null").map_err(nul)?;

    let mut attr: Attr = std::ptr::null_mut();
    let mut actions: Actions = std::ptr::null_mut();
    // SAFETY: init writes a fresh attribute object into `attr`.
    let rc = unsafe { posix_spawnattr_init(&mut attr) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    // SAFETY: init writes a fresh file-actions object into `actions`.
    let rc = unsafe { posix_spawn_file_actions_init(&mut actions) };
    if rc != 0 {
        // SAFETY: `attr` was initialised above.
        unsafe { posix_spawnattr_destroy(&mut attr) };
        return Err(io::Error::from_raw_os_error(rc));
    }
    let empty_mask: u32 = 0;
    // Every catchable signal to its default in the child: a disposition this
    // process set (SIGPIPE ignored, say) is not the program's to inherit.
    let all_signals: u32 = !0;
    let mut result = Ok(());
    let flags = POSIX_SPAWN_CLOEXEC_DEFAULT | POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK;
    // SAFETY: every call takes the objects initialised above and live locals.
    unsafe {
        for rc in [
            posix_spawnattr_setflags(&mut attr, flags),
            posix_spawnattr_setsigmask(&mut attr, &empty_mask),
            posix_spawnattr_setsigdefault(&mut attr, &all_signals),
            posix_spawn_file_actions_addopen(&mut actions, 0, dev_null.as_ptr(), O_RDWR, 0),
            posix_spawn_file_actions_addopen(&mut actions, 1, dev_null.as_ptr(), O_RDWR, 0),
            posix_spawn_file_actions_addopen(&mut actions, 2, dev_null.as_ptr(), O_RDWR, 0),
        ] {
            if rc != 0 && result.is_ok() {
                result = Err(io::Error::from_raw_os_error(rc));
            }
        }
    }
    let mut pid: i32 = 0;
    if result.is_ok() {
        // SAFETY: `argv` is NUL-terminated and its strings outlive the call;
        // `environ` is the process environment block.
        let rc = unsafe {
            posix_spawn(
                &mut pid,
                program_c.as_ptr(),
                &actions,
                &attr,
                argv.as_ptr(),
                environ,
            )
        };
        if rc != 0 {
            result = Err(io::Error::from_raw_os_error(rc));
        }
    }
    // SAFETY: both objects were initialised above and are destroyed once.
    unsafe {
        posix_spawn_file_actions_destroy(&mut actions);
        posix_spawnattr_destroy(&mut attr);
    }
    result?;
    u32::try_from(pid).map_err(|_| io::Error::other("posix_spawn returned a negative pid"))
}

/// See the Darwin twin.
///
/// # Errors
/// Always `Unsupported`.
#[cfg(not(target_vendor = "apple"))]
pub fn spawn_cloexec_default(_program: &str, _args: &[&str]) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "POSIX_SPAWN_CLOEXEC_DEFAULT is Darwin's",
    ))
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    /// A descriptor this process holds WITHOUT close-on-exec, at a number no
    /// shell touches (200), does not reach the child. NEGATIVE CONTROL: an
    /// ordinary `Command` spawn of the same probe sees it — so the probe can
    /// see a leak, and the flag is what stops it.
    #[test]
    fn the_child_inherits_nothing_but_stdio() {
        unsafe extern "C" {
            fn fcntl(fd: i32, cmd: i32, ...) -> i32;
            fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
        }
        const F_DUPFD: i32 = 0;
        const F_GETFD: i32 = 1;
        let (held, _other) = crate::CtlStream::pair().expect("socketpair");
        // SAFETY: F_DUPFD on a descriptor we own; the result is ours to own.
        let raw = unsafe { fcntl(held.as_raw_fd(), F_DUPFD, 200) };
        assert!(raw >= 200, "a high duplicate: {raw}");
        // SAFETY: a fresh descriptor this test exclusively owns.
        let leak = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: flag read on a descriptor we own.
        assert_eq!(
            unsafe { fcntl(leak.as_raw_fd(), F_GETFD) } & 1,
            0,
            "inheritable here"
        );
        let probe = format!("[ -e /dev/fd/{raw} ] && exit 3; exit 0");

        let control = std::process::Command::new("/bin/sh")
            .args(["-c", &probe])
            .status()
            .expect("control spawn");
        assert_eq!(
            control.code(),
            Some(3),
            "an ordinary spawn carries fd {raw}"
        );

        let pid = super::spawn_cloexec_default("/bin/sh", &["-c", &probe]).expect("spawn");
        let mut status = -1;
        // SAFETY: our own child; blocking reap.
        let reaped = unsafe { waitpid(pid as i32, &mut status, 0) };
        assert_eq!(reaped, pid as i32);
        assert_eq!(
            status, 0,
            "fd {raw} did not reach the child (wait status {status:#x})"
        );
        drop(leak);
    }
}
