//! Revoke this task's bootstrap slot and its matching local send-name copies.
//! Other special/exception ports and routes to reacquire authority are separate.

use std::io;

// Installed SDK mach/task_special_ports.h, mach/task.h, mach/port.h and
// servers/bootstrap.h. All operations below address only this current task.
const TASK_BOOTSTRAP_PORT: i32 = 4;
const SEND: i32 = 0;
const INVALID_NAME: i32 = 15;

unsafe extern "C" {
    static mach_task_self_: u32;
    static mut bootstrap_port: u32;
    fn task_get_special_port(task: u32, which: i32, port: *mut u32) -> i32;
    fn task_set_special_port(task: u32, which: i32, port: u32) -> i32;
    fn mach_port_get_refs(task: u32, name: u32, right: i32, refs: *mut u32) -> i32;
    fn mach_port_mod_refs(task: u32, name: u32, right: i32, delta: i32) -> i32;
    fn mach_port_type(task: u32, name: u32, kind: *mut u32) -> i32;
}

fn checked(result: i32) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "native bootstrap-port revocation refused ({result})"
        )))
    }
}

/// The actual own-task query's send reference. Private fields prevent a caller
/// DATA name from exempting a port in the registered-stash revoker. This value
/// is used only within the same sole-main, signal-blocked native entry; every
/// error is terminal and process teardown retires any partially held rights.
pub(super) struct Captured {
    task: u32,
    cached: u32,
    original: u32,
}

impl Captured {
    pub(super) fn capture() -> io::Result<Self> {
        // SAFETY: current process runtime globals, read only on its sole thread.
        let (task, cached) = unsafe { (mach_task_self_, bootstrap_port) };
        let mut original = 0;
        // SAFETY: own-task query into complete native writable storage. A nonzero
        // result is a newly obtained send reference, retained through revocation.
        checked(unsafe { task_get_special_port(task, TASK_BOOTSTRAP_PORT, &mut original) })?;
        if original == 0 {
            return if cached == 0 {
                Ok(Self {
                    task,
                    cached,
                    original,
                })
            } else {
                Err(io::Error::other(
                    "native bootstrap cache has no matching task slot",
                ))
            };
        }
        // Never revoke required self handles or interpret a distinct cached port
        // as the actual queried bootstrap authority. Uncertainty ends this entry.
        // SAFETY: current sole pthread's own borrowed native thread name.
        let thread = unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) };
        if original == u32::MAX
            || original == task
            || original == thread
            || cached != 0 && cached != original
        {
            return Err(io::Error::other(
                "native bootstrap slot/cache identity is unsupported",
            ));
        }
        Ok(Self {
            task,
            cached,
            original,
        })
    }

    pub(super) fn local_name(&self) -> u32 {
        self.original
    }

    pub(super) fn revoke(self) -> io::Result<()> {
        let Self {
            task,
            cached,
            original,
        } = self;
        // SAFETY: same sole-main entry's original globals. The registered-stash
        // revoker may add SEND refs to this actual name, but must not replace it.
        if unsafe { mach_task_self_ } != task || unsafe { bootstrap_port } != cached {
            return Err(io::Error::other(
                "native bootstrap original changed before revocation",
            ));
        }
        let mut current = 0;
        // SAFETY: own-task query; its additional ref joins the final actual count.
        checked(unsafe { task_get_special_port(task, TASK_BOOTSTRAP_PORT, &mut current) })?;
        if current != original {
            return Err(io::Error::other(
                "native bootstrap task slot changed before revocation",
            ));
        }
        if original == 0 {
            return Ok(());
        }
        let mut references = 0;
        // SAFETY: the actual own-task query supplied this live local send name.
        checked(unsafe { mach_port_get_refs(task, original, SEND, &mut references) })?;
        let references = i32::try_from(references)
            .ok()
            .filter(|count| *count > 0)
            .ok_or_else(|| {
                io::Error::other("native bootstrap send-reference count is unavailable")
            })?;
        // SAFETY: clear only this task's actual bootstrap slot. All later errors
        // remain irreversible and the caller must terminate instead of restoring.
        checked(unsafe { task_set_special_port(task, TASK_BOOTSTRAP_PORT, 0) })?;
        // SAFETY: the sole trusted entry owns this matching process-local cache.
        // Clearing the kernel slot alone does not clear libSystem's cached name.
        unsafe { bootstrap_port = 0 };
        // SAFETY: remove ALL current SEND user refs, including both native queries,
        // cached aliases and any overlapping registered-stash lookup copies. The
        // count is taken AFTER that lookup. Destroy no receive or remote object.
        checked(unsafe { mach_port_mod_refs(task, original, SEND, -references) })?;
        let mut kind = 0;
        // SAFETY: read-only observation of exactly the original local name. A dead
        // name, remaining receive right, or any uncertain response is a refusal.
        if unsafe { mach_port_type(task, original, &mut kind) } != INVALID_NAME {
            return Err(io::Error::other("native bootstrap send name remains"));
        }
        let mut remaining = 0;
        // SAFETY: own-task bookend into complete writable storage.
        checked(unsafe { task_get_special_port(task, TASK_BOOTSTRAP_PORT, &mut remaining) })?;
        // SAFETY: sole-thread read of the same local cache cleared above.
        if remaining != 0 || unsafe { bootstrap_port } != 0 {
            return Err(io::Error::other("native bootstrap slot/cache remains"));
        }
        Ok(())
    }
}
