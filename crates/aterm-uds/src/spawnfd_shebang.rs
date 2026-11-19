// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Darwin spawn's `#!` file, interpreted here rather than by the kernel wherever this gives
//! the kernel's result exactly.
//!
//! XNU leaks one `data.kalloc.1024` element, never freed, on each exec of a `#!` file it
//! interprets itself (`posix_spawn` and `execve` alike, whatever the interpreter; measured on
//! macOS 26.6), and nothing on an exec of the interpreter that names the script. A test suite
//! that starts scripts leaks tens of thousands a run, and an exhausted zone map panics the Mac.
//! [`interpret`] reads the file as XNU's `exec_shell_imgact` reads it and answers the
//! interpreter and the argv the kernel would run, or the kernel's refusal (`ENOEXEC`) of a line
//! it will not run. A file whose exec this layer cannot reproduce exactly goes to the kernel as
//! it is: one it cannot read, one with a set-id bit, one another of the kernel's exec checks may
//! judge (a volume mounted `noexec`, a quarantined file, a sandboxed process), a line holding a
//! NUL, a line whose interpreter it cannot read, and one whose interpreter has a set-id bit,
//! which the kernel's interpretation ignores and an exec of the interpreter itself would apply.
//!
//! What the kernel's own interpretation shows the new image is what an exec of the interpreter
//! shows: `ps`'s command and arguments, `proc_pidpath` and `proc_name` read alike either way
//! (measured on macOS 26.6).

#[cfg(target_vendor = "apple")]
use std::ffi::{CStr, CString};
#[cfg(target_vendor = "apple")]
use std::path::Path;

/// How many bytes of a file's head the kernel reads for its `#!` line (`IMG_SHSIZE`).
pub(super) const HEADER: usize = 512;

/// A file's head, as the kernel reads its `#!` line ([`parse`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Line<'a> {
    /// The head does not start `#!`: no script, whichever image activator takes the file.
    NotScript,
    /// A `#!` line the kernel refuses with `ENOEXEC`: the line ends (`\n` or `#`) before any
    /// interpreter, or no end comes within the [`HEADER`].
    Malformed,
    /// A `#!` line holding a NUL before its end, whose words the kernel splits by rules this
    /// layer does not reproduce.
    Unsure,
    /// The interpreter, then each of its arguments.
    Words(Vec<&'a [u8]>),
}

/// The `#!` line of a file whose first [`HEADER`] bytes are `head` (NUL past the file's end),
/// read as XNU's `exec_shell_imgact` reads it: blanks (`' '` and `'\t'`) after the `#!` are
/// skipped, the line ends at the first `\n` or `#` at or past offset 2 and before offset
/// [`HEADER`], blanks before that end are trimmed, and the rest splits at each run of blanks
/// into separate words (where Linux hands the rest on as one argument). A `'\r'` is no blank and
/// stays in its word.
pub(super) fn parse(head: &[u8; HEADER]) -> Line<'_> {
    let blank = |b: u8| b == b' ' || b == b'\t';
    let end = |b: u8| b == b'\n' || b == b'#';
    if !head.starts_with(b"#!") {
        return Line::NotScript;
    }
    let Some(start) = (2..HEADER).find(|&at| !blank(head[at])) else {
        return Line::Malformed;
    };
    if end(head[start]) {
        return Line::Malformed;
    }
    let Some(stop) = (start..HEADER).find(|&at| end(head[at])) else {
        return Line::Malformed;
    };
    let line = &head[start..stop];
    if line.contains(&0) {
        return Line::Unsure;
    }
    Line::Words(
        line.split(|&b| blank(b))
            .filter(|word| !word.is_empty())
            .collect(),
    )
}

/// What a spawn hands `posix_spawn` for one file it would run ([`interpret`]).
#[cfg(target_vendor = "apple")]
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    /// The file and argv as they are: the kernel decides.
    Kernel,
    /// The kernel's error for this file, this errno, with nothing started.
    Refuse(i32),
    /// The interpreter, as its line names it, and the argv the kernel would give it.
    Run { path: CString, argv: Vec<CString> },
}

/// How the kernel would run `path`, a file a spawn hands `posix_spawn` with `argv` (its first
/// word is the caller's `argv[0]`, which an interpreted script's argv drops): its interpreter
/// and argv, its refusal, or [`Outcome::Kernel`] wherever this layer cannot answer exactly. A
/// relative path resolves where the exec resolves it: in this process's working directory, or,
/// when a file action moves the child's first (`moved`), in `base`, that directory, and
/// [`Outcome::Kernel`] where `base` is unknown. Every file it opens is closed when it returns.
///
/// The argv the kernel gives an interpreter: each word of the `#!` line ([`parse`]), the first
/// as written, then `path` exactly as the exec was given it, then `argv[1..]`; the environment
/// is unchanged. The interpreter is never searched for on `PATH`, and a relative one resolves in
/// the exec's working directory, so [`Outcome::Run`] names it as written. XNU interprets once:
/// an interpreter that is itself a `#!` file fails the exec with `ENOEXEC`. XNU takes an
/// interpreted exec's set-id from the script's attributes alone, so an interpreter with a set-id
/// bit runs without it there, where an exec of the interpreter itself would apply it: such an
/// interpreter is [`Outcome::Kernel`]'s.
#[cfg(target_vendor = "apple")]
pub(super) fn interpret(
    path: &CStr,
    argv: &[CString],
    moved: bool,
    base: Option<&Path>,
) -> Outcome {
    let Some(script) = darwin::probe_path(path.to_bytes(), moved, base) else {
        return Outcome::Kernel;
    };
    let mut head = [0; HEADER];
    let darwin::Head::Read(file) = darwin::head_of(&script, &mut head) else {
        return Outcome::Kernel;
    };
    let line = parse(&head);
    if line == Line::NotScript {
        return Outcome::Kernel;
    }
    let exact = darwin::exec_checks_pass(&file, &script) && !darwin::sandboxed();
    drop(file);
    let words = match line {
        _ if !exact => return Outcome::Kernel,
        Line::Malformed => return Outcome::Refuse(libc::ENOEXEC),
        Line::NotScript | Line::Unsure => return Outcome::Kernel,
        Line::Words(words) => words,
    };
    let Some(&interpreter) = words.first() else {
        return Outcome::Kernel;
    };
    let Some(probe) = darwin::probe_path(interpreter, moved, base) else {
        return Outcome::Kernel;
    };
    let mut interpreter_head = [0; HEADER];
    match darwin::head_of(&probe, &mut interpreter_head) {
        darwin::Head::Read(file) if interpreter_head.starts_with(b"#!") => {
            // The kernel fails it once its exec checks of the interpreter pass, as any image's.
            return if darwin::exec_checks_pass(&file, &probe) {
                Outcome::Refuse(libc::ENOEXEC)
            } else {
                Outcome::Kernel
            };
        }
        darwin::Head::Read(file) if darwin::set_id_free(&file) => {}
        darwin::Head::Read(_) | darwin::Head::NoFile | darwin::Head::Unread => {
            return Outcome::Kernel;
        }
    }
    let mut run = Vec::with_capacity(words.len() + argv.len());
    for word in &words {
        let Ok(word) = CString::new(*word) else {
            return Outcome::Kernel;
        };
        run.push(word);
    }
    run.push(path.to_owned());
    run.extend(argv.iter().skip(1).cloned());
    let Ok(interpreter) = CString::new(interpreter) else {
        return Outcome::Kernel;
    };
    Outcome::Run {
        path: interpreter,
        argv: run,
    }
}

/// Whether `path`, a file a spawn hands `posix_spawn` as it is, is or may be a `#!` script the
/// kernel then interprets or refuses: a regular file whose head starts `#!` or could not be read,
/// or a relative path whose directory is unknown (a file action moves the child's working
/// directory, `moved`, and `base` does not name it). No regular file at the path, which the
/// kernel never activates, is no script. It allocates only to join a relative path to `base`.
#[cfg(all(target_vendor = "apple", any(test, debug_assertions)))]
pub(super) fn handed_script(path: &CStr, moved: bool, base: Option<&Path>) -> bool {
    let joined;
    let probe = if path.to_bytes().first() == Some(&b'/') || !moved {
        path
    } else if let Some(full) = darwin::probe_path(path.to_bytes(), moved, base) {
        joined = full;
        &joined
    } else {
        return true;
    };
    let mut head = [0; HEADER];
    match darwin::head_of(probe, &mut head) {
        darwin::Head::NoFile => false,
        darwin::Head::Unread => true,
        darwin::Head::Read(_) => head.starts_with(b"#!"),
    }
}

/// Whether this process's spawns interpret a `#!` file themselves where that is exact: not in a
/// sandboxed process, whose scripts are the kernel's ([`interpret`]).
#[cfg(all(target_vendor = "apple", test))]
pub(super) fn interprets_here() -> bool {
    !darwin::sandboxed()
}

#[cfg(target_vendor = "apple")]
mod darwin {
    use super::HEADER;
    use std::ffi::{CStr, CString};
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    /// `<sys/mount.h>`'s `MNT_NOEXEC`.
    const MNT_NOEXEC: u32 = 0x0000_0004;
    /// `<sandbox.h>`'s `SANDBOX_FILTER_NONE`: a check that names no operation asks whether the
    /// process is sandboxed at all.
    const SANDBOX_FILTER_NONE: libc::c_int = 0;
    /// The extended attribute Gatekeeper judges a downloaded file by.
    const QUARANTINE: &CStr = c"com.apple.quarantine";

    unsafe extern "C" {
        fn sandbox_check(
            pid: libc::pid_t,
            operation: *const libc::c_char,
            filter: libc::c_int,
            ...
        ) -> libc::c_int;
    }

    /// What reading a path's head found ([`head_of`]).
    pub(super) enum Head {
        /// No regular file there: the kernel's exec fails before it reads any head.
        NoFile,
        /// A regular file this process could not open or read.
        Unread,
        /// The file, open for reading, whose head was read.
        Read(OwnedFd),
    }

    /// `path` where this process can name the file an exec would resolve it to: as it is when
    /// absolute, or when relative and the exec runs in this process's working directory, else
    /// taken in `base` (the child's working directory) when that is known.
    pub(super) fn probe_path(path: &[u8], moved: bool, base: Option<&Path>) -> Option<CString> {
        if path.first() == Some(&b'/') || !moved {
            return CString::new(path).ok();
        }
        let base = base?.as_os_str().as_bytes();
        let mut full = Vec::with_capacity(base.len() + path.len() + 1);
        full.extend_from_slice(base);
        full.push(b'/');
        full.extend_from_slice(path);
        CString::new(full).ok()
    }

    fn stat_of(path: &CStr) -> Option<libc::stat> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: a NUL-terminated path and one writable `stat`, which a successful call fills;
        // neither is retained.
        if unsafe { libc::stat(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: the successful `stat` initialized it whole.
        Some(unsafe { stat.assume_init() })
    }

    fn fstat_of(file: &OwnedFd) -> Option<libc::stat> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: an open descriptor this call borrows and one writable `stat`.
        if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: the successful `fstat` initialized it whole.
        Some(unsafe { stat.assume_init() })
    }

    /// Read the head of the regular file at `path`, following links, into `head`: its first
    /// [`HEADER`] bytes, NUL past its end (`head` is all NUL unless [`Head::Read`] answers).
    /// Only a regular file is opened (a FIFO's or a device's open acts on it), read-only, never
    /// as a controlling terminal, without waiting, and close-on-exec; the kernel's exec refuses
    /// every other kind.
    pub(super) fn head_of(path: &CStr, head: &mut [u8; HEADER]) -> Head {
        head.fill(0);
        match stat_of(path) {
            Some(stat) if stat.st_mode & libc::S_IFMT == libc::S_IFREG => {}
            _ => return Head::NoFile,
        }
        let flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY;
        // SAFETY: a NUL-terminated path; no creation flag needs a mode argument.
        let fd = unsafe { libc::open(path.as_ptr(), flags) };
        if fd < 0 {
            return Head::Unread;
        }
        // SAFETY: the successful open transferred its fresh descriptor to this call alone.
        let file = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut filled = 0;
        while filled < HEADER {
            let rest = &mut head[filled..];
            let Ok(offset) = libc::off_t::try_from(filled) else {
                head.fill(0);
                return Head::Unread;
            };
            // SAFETY: `rest` is `rest.len()` writable bytes the call fills and does not retain.
            let got = unsafe {
                libc::pread(
                    file.as_raw_fd(),
                    rest.as_mut_ptr().cast(),
                    rest.len(),
                    offset,
                )
            };
            match usize::try_from(got) {
                Ok(0) => break,
                Ok(got) => filled += got,
                Err(_) if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) => {}
                Err(_) => {
                    head.fill(0);
                    return Head::Unread;
                }
            }
        }
        Head::Read(file)
    }

    /// Whether `file`'s mode, read now, has neither set-id bit; one unread answers no.
    pub(super) fn set_id_free(file: &OwnedFd) -> bool {
        fstat_of(file).is_some_and(|stat| stat.st_mode & (libc::S_ISUID | libc::S_ISGID) == 0)
    }

    /// What the kernel's checks before it activates a file read of it ([`exec_checks_pass`]).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) struct ExecFacts {
        /// Its type and permission bits (`st_mode`).
        pub(super) mode: libc::mode_t,
        /// Whether this process's effective identity may execute it (`faccessat`, `X_OK`,
        /// `AT_EACCESS`).
        pub(super) executable: bool,
        /// Whether its volume is mounted `noexec` (`MNT_NOEXEC` in `fstatfs`'s `f_flags`).
        pub(super) noexec: bool,
        /// Whether it has a quarantine for Gatekeeper to judge (`com.apple.quarantine`).
        pub(super) quarantined: bool,
    }

    impl ExecFacts {
        /// The facts of `file`, opened from `path`, each read now; `None` when any could not be
        /// read.
        pub(super) fn read(file: &OwnedFd, path: &CStr) -> Option<Self> {
            let mode = fstat_of(file)?.st_mode;
            // SAFETY: a NUL-terminated path; `faccessat` reads it and retains nothing. AT_FDCWD
            // resolves a relative path in this process, and AT_EACCESS selects its effective
            // IDs.
            let executable = unsafe {
                libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS)
            } == 0;
            let mut volume = MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: an open descriptor this call borrows and one writable `statfs`.
            if unsafe { libc::fstatfs(file.as_raw_fd(), volume.as_mut_ptr()) } != 0 {
                return None;
            }
            // SAFETY: the successful `fstatfs` initialized it whole.
            let noexec = unsafe { volume.assume_init() }.f_flags & MNT_NOEXEC != 0;
            // SAFETY: a NUL-terminated name and a null buffer of size 0, which asks for the
            // value's size alone; nothing is written.
            let size = unsafe {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    QUARANTINE.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                )
            };
            let quarantined = if size >= 0 {
                true
            } else if io::Error::last_os_error().raw_os_error() == Some(libc::ENOATTR) {
                false
            } else {
                return None;
            };
            Some(Self {
                mode,
                executable,
                noexec,
                quarantined,
            })
        }

        /// Whether these are checks this layer reproduces, and pass: a regular file with an
        /// execute bit and no set-id bit, which this process's effective identity may execute,
        /// on a volume not mounted `noexec`, with no quarantine for Gatekeeper to judge.
        pub(super) const fn pass(self) -> bool {
            self.mode & libc::S_IFMT == libc::S_IFREG
                && self.mode & (libc::S_IXUSR | libc::S_IXGRP | libc::S_IXOTH) != 0
                && self.mode & (libc::S_ISUID | libc::S_ISGID) == 0
                && self.executable
                && !self.noexec
                && !self.quarantined
        }
    }

    /// Whether the kernel's checks before it activates `file` (opened from `path`) are ones this
    /// layer reproduces, and pass ([`ExecFacts::pass`]). Anything unread answers no.
    pub(super) fn exec_checks_pass(file: &OwnedFd, path: &CStr) -> bool {
        ExecFacts::read(file, path).is_some_and(ExecFacts::pass)
    }

    /// Whether this process runs under a sandbox, whose exec rules for the script the kernel's
    /// interpretation applies; an unanswered check counts as one.
    pub(super) fn sandboxed() -> bool {
        // SAFETY: `getpid` takes nothing; with no operation and SANDBOX_FILTER_NONE the check
        // takes no variadic argument and only reads this process's sandbox state.
        unsafe { sandbox_check(libc::getpid(), std::ptr::null(), SANDBOX_FILTER_NONE) != 0 }
    }
}

#[cfg(test)]
#[path = "spawnfd_shebang_tests.rs"]
mod tests;
