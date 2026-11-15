//! Process-local group-change restrictions for an independently owned producer.
//!
//! This grants no execution, CPU, SDK, descendant-retirement or returned-owner
//! authority. The actual outside owner must retain the original unreaped group
//! leader and its complete native cohort through settlement. Mach services and
//! other external process creators need separate restrictions and qualification.

use std::io;
use std::marker::PhantomData;
use std::os::unix::process::CommandExt as _;
use std::process::Command;
use std::rc::Rc;

#[link(name = "sandbox")]
unsafe extern "C" {
    fn sandbox_init(profile: *const libc::c_char, flags: u64, error: *mut *mut libc::c_char)
    -> i32;
    fn sandbox_free_error(error: *mut libc::c_char);
}

/// One fixed restriction installed by the original trusted group-leader entry.
///
/// Unix setsid, setpgid and posix_spawn are denied. The last is necessary:
/// spawn attributes can create another session/group without either setter.
/// A fixed fork/exec mechanism can still be selected before a first attempt.
/// Existing sandbox restrictions continue to apply; installation alone does
/// not establish that any workload or descendant ran successfully.
///
/// This non-Clone, non-Send marker belongs to its installing process/thread.
/// Dropping it never removes the kernel restriction or returns resources.
#[must_use]
pub struct ForkingGroupRestriction {
    leader: libc::pid_t,
    _local: PhantomData<Rc<()>>,
}

impl ForkingGroupRestriction {
    /// Install the fixed irreversible restriction on this actual session leader.
    ///
    /// An error is not a rollback. The caller must terminate the failed role;
    /// its outside owner retains all original custody until native settlement.
    /// There is no fallback to an ordinary producer or NoFork target.
    ///
    /// # Safety
    /// This must be the original trusted, single-threaded native entry before
    /// creating any descendant or calling untrusted code. The actual outside
    /// launch owner must still retain this same unreaped child (which must be
    /// its session/group leader for successful installation),
    /// original resources and endpoint. No PID observation, environment flag,
    /// local wait or successful installation supplies those owners. This does
    /// not retrospectively constrain a descendant created before installation.
    pub unsafe fn install_current() -> io::Result<Self> {
        // Apple's BSD syscall ABI: SDK sys/syscall.h and XNU syscalls.master.
        // libc does not export Darwin's SYS_* constants; the fixed profile and
        // native raw-syscall/posix_spawn controls qualify these exact numbers.
        let restriction = Self { leader: unsafe { libc::getpid() }, _local: PhantomData };
        restriction.check_current()?;
        let mut error = std::ptr::null_mut();
        // SAFETY: one static profile, initialized error storage, current process
        // only. The caller supplies the trusted original-entry preconditions.
        let result = unsafe {
            sandbox_init(
                c"(version 3)(allow default)(deny syscall-unix (syscall-number 82 147 244))"
                    .as_ptr(),
                0,
                &mut error,
            )
        };
        if !error.is_null() {
            // SAFETY: the initializer alone supplied this allocation.
            unsafe { sandbox_free_error(error) };
        }
        if result != 0 {
            return Err(io::Error::other("fixed forking-group restriction refused"));
        }
        restriction.check_current()?;
        Ok(restriction)
    }

    /// Negative check of the SAME installing leader; no other-process policy
    /// attestation, descendant census or resource/custody receipt is produced.
    pub fn check_current(&self) -> io::Result<()> {
        // SAFETY: query-only native identity calls with no borrowed pointers.
        if self.leader <= 0
            || unsafe { libc::getpid() } != self.leader
            || unsafe { libc::getpgrp() } != self.leader
            || unsafe { libc::getsid(0) } != self.leader
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "forking-group restriction requires its original session leader",
            ));
        }
        Ok(())
    }

    /// Select fork/exec before the command's FIRST attempt, then recheck the
    /// inherited group in its child. This is a mechanism restriction only;
    /// actual program permission, funding and native return remain independent.
    /// It never retries a refused posix_spawn or installs a new execution owner.
    pub fn configure_fork_exec(&self, command: &mut Command) -> io::Result<()> {
        self.check_current()?;
        let leader = self.leader;
        // SAFETY: the callback calls only native identity syscalls and constructs
        // an inline OS error. It neither allocates nor touches inherited locks.
        // Its presence selects std's fork path before any posix_spawn attempt.
        unsafe {
            command.pre_exec(move || {
                if libc::getpgrp() != leader || libc::getsid(0) != leader {
                    Err(io::Error::from_raw_os_error(libc::EPERM))
                } else {
                    Ok(())
                }
            });
        }
        Ok(())
    }
}
