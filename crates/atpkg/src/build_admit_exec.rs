// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A FRONT BECOMES ITS BUILD AS A SHELL'S EXEC WOULD HAVE STARTED IT (BUILD-ADMIT): the front's
//! exec hands the build this process's pid, descriptors, environment, limits, mask and signal
//! dispositions, and the front must change none of them. Two of those the front's own runtime
//! changed before its `main`: Rust's start ignores SIGPIPE (an exec keeps an ignored signal
//! ignored), and std's exec then sets SIGPIPE back to its default and empties the signal mask,
//! whatever the front was started with. So each signal's disposition and the mask are read at
//! load, before the runtime starts ([`INHERITED`]), and put back just before the exec, which is a
//! plain `execv` of this process's own environment: an inherited ignored SIGPIPE or SIGINT (a
//! background job's) and an inherited blocked signal reach the build as they reached the front.
//! The front's admission wait catches SIGINT, SIGTERM, SIGHUP and SIGQUIT while it waits
//! (`aterm_toolchain::interrupt`), so what it puts back must be what it was started with, not
//! the defaults. Measured on macOS 27.0.1 (2026-10-29) with C stand-ins: a front that installs
//! handlers for those and execs without putting the inherited dispositions back turns an
//! inherited ignored SIGINT and SIGQUIT into their defaults (and drops their `SA_RESTART`); one
//! that blocks signals and execs leaves them blocked in the build; one that puts both back is
//! byte-identical to a direct run (pid, parent, group, session, environment, mask, every
//! disposition, descriptors, limits, umask, directory).

use std::ffi::{CString, OsStr};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::Path;
use std::sync::OnceLock;

/// The signals whose dispositions are read and put back: every one a process can set.
const SIGNALS: std::ops::Range<libc::c_int> = 1..32;

/// This process's signal state as it was started with it.
struct Inherited {
    /// Each signal's disposition, read whole.
    actions: Vec<(libc::c_int, libc::sigaction)>,
    /// The signal mask.
    mask: libc::sigset_t,
}

/// What [`read_at_load`] read before `main`; unset where the load-time read did not run.
static INHERITED: OnceLock<Inherited> = OnceLock::new();

/// Read the signal state now: run at load, before the runtime starts and before any thread.
extern "C" fn read_at_load() {
    let mut actions = Vec::with_capacity(SIGNALS.len());
    for sig in SIGNALS {
        let mut old = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        // SAFETY: a null new action only reads `sig`'s disposition into `old`, zeroed and live.
        if unsafe { libc::sigaction(sig, std::ptr::null(), old.as_mut_ptr()) } == 0 {
            // SAFETY: written whole by the call that succeeded.
            actions.push((sig, unsafe { old.assume_init() }));
        }
    }
    let mut mask = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: a null set only reads the mask into `mask`, zeroed and live.
    if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), mask.as_mut_ptr()) } != 0 {
        return;
    }
    // SAFETY: written whole by the call that succeeded.
    let mask = unsafe { mask.assume_init() };
    let _ = INHERITED.set(Inherited { actions, mask });
}

/// The load-time read: a constructor the loader runs before `main`, so before Rust's runtime
/// ignores SIGPIPE.
#[used]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
)]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
static READ_AT_LOAD: extern "C" fn() = read_at_load;

/// Put back every disposition and the mask [`INHERITED`] read. `false` where the load-time read
/// did not run (a binary whose loader skipped it): nothing is put back.
fn put_back() -> bool {
    // The read's object is linked wherever this is: the loader runs only constructors it loads.
    // It is named by its function, never by the constructor's slot: a link with chained fixups
    // (aterm's, `-Wl,-fixup_chains`) turns `__mod_init_func` into `__init_offsets` and leaves
    // that slot no address, so code that takes it does not link.
    std::hint::black_box(read_at_load as extern "C" fn());
    let Some(inherited) = INHERITED.get() else {
        return false;
    };
    for (sig, action) in &inherited.actions {
        // SAFETY: puts back a disposition read whole for `sig` at load: an inherited one is the
        // default or ignored (an exec resets every handler), so no handler of this image runs.
        unsafe { libc::sigaction(*sig, action, std::ptr::null_mut()) };
    }
    // SAFETY: the mask read whole at load, live across the call.
    unsafe {
        libc::pthread_sigmask(
            libc::SIG_SETMASK,
            &raw const inherited.mask,
            std::ptr::null_mut(),
        )
    };
    true
}

/// BECOME `target` run on `args`, as this process was started: its signal state put back
/// ([`put_back`]), then `execv` with this process's environment and descriptors (each one this
/// process opened is close-on-exec), or `execve` with that environment and
/// [`aterm_capacity::COVER_ENV`] set to `marker` where one is given. Where the load-time read did
/// not run, std's exec does it, which hands the build SIGPIPE at its default and an empty mask.
/// Returns only when the exec failed: its error.
pub(crate) fn become_build<A: AsRef<OsStr>>(
    target: &Path,
    args: &[A],
    marker: Option<&str>,
) -> std::io::Error {
    let words: Option<Vec<CString>> = std::iter::once(target.as_os_str())
        .chain(args.iter().map(AsRef::as_ref))
        .map(|w| CString::new(w.as_bytes()).ok())
        .collect();
    let Some(words) = words else {
        return std::io::Error::new(std::io::ErrorKind::InvalidInput, "a word holds a NUL byte");
    };
    let env = marker.map(|marker| {
        let mut set = false;
        let mut env: Vec<CString> = std::env::vars_os()
            .filter_map(|(k, v)| {
                let v = if k == aterm_capacity::COVER_ENV {
                    set = true;
                    OsStr::new(marker).to_owned()
                } else {
                    v
                };
                let mut entry = k.into_vec();
                entry.push(b'=');
                entry.extend_from_slice(v.as_bytes());
                CString::new(entry).ok()
            })
            .collect();
        if !set && let Ok(entry) = CString::new(format!("{}={marker}", aterm_capacity::COVER_ENV)) {
            env.push(entry);
        }
        env
    });
    if !put_back() {
        #[expect(
            clippy::disallowed_types,
            reason = "a pass-through by exec: its caller's descriptors are the program's"
        )]
        let mut command = std::process::Command::new(target);
        aterm_uds::spawn::set_flags(&mut command, 0);
        command.args(args);
        if let Some(marker) = marker {
            command.env(aterm_capacity::COVER_ENV, marker);
        }
        return crate::platform::exec_or_run(&mut command);
    }
    let mut argv: Vec<*const libc::c_char> = words.iter().map(|w| w.as_ptr()).collect();
    argv.push(std::ptr::null());
    match &env {
        None => {
            // SAFETY: `words[0]` and `argv` are NUL-terminated and live across the call, `argv`
            // ending in a null pointer; `execv` returns only on failure.
            unsafe { libc::execv(words[0].as_ptr(), argv.as_ptr()) };
        }
        Some(env) => {
            let mut envp: Vec<*const libc::c_char> = env.iter().map(|e| e.as_ptr()).collect();
            envp.push(std::ptr::null());
            // SAFETY: as for `execv`, and `envp` likewise: NUL-terminated entries, live, ending
            // in a null pointer.
            unsafe { libc::execve(words[0].as_ptr(), argv.as_ptr(), envp.as_ptr()) };
        }
    }
    std::io::Error::last_os_error()
}

#[cfg(test)]
#[path = "build_admit_exec_tests.rs"]
mod tests;
