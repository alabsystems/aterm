//! Revoke the task's registered stash and its current local send-name copies.
//! No special/exception/bootstrap or arbitrary Mach capability closure follows.
use std::io;

// Primary installed SDK: mach/task.defs uses _kernelrpc_mach_ports_lookup3 /
// register3; mach/mach_port.h defines get_refs/mod_refs/type; mach/port.h
// defines SEND=0. No foreign-task query, service lookup or deprecated destroy.
const SLOTS: usize = 3;
const INVALID_NAME: i32 = 15;
const SEND: i32 = 0;

unsafe extern "C" {
    static mach_task_self_: u32;
    fn mach_ports_lookup(task: u32, ports: *mut *mut u32, count: *mut u32) -> i32;
    fn mach_ports_register(task: u32, ports: *const u32, count: u32) -> i32;
    fn mach_port_get_refs(task: u32, name: u32, right: i32, refs: *mut u32) -> i32;
    fn mach_port_mod_refs(task: u32, name: u32, right: i32, delta: i32) -> i32;
    fn mach_port_type(task: u32, name: u32, kind: *mut u32) -> i32;
    fn vm_deallocate(task: u32, address: usize, size: usize) -> i32;
}

fn checked(result: i32) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "native registered-port revocation refused ({result})"
        )))
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn lookup(task: u32) -> io::Result<[u32; SLOTS]> {
    let mut pointer = std::ptr::null_mut();
    let mut count = 0;
    // SAFETY: query only this task; the kernel/libSystem returns this owned
    // out-of-line array and send-name references, not a foreign memory range.
    checked(unsafe { mach_ports_lookup(task, &mut pointer, &mut count) })?;
    let mut ports = [0; SLOTS];
    let valid = count as usize <= SLOTS && (count == 0 || !pointer.is_null());
    if valid {
        for (index, slot) in ports.iter_mut().enumerate().take(count as usize) {
            // SAFETY: complete successful native array, bounded by SDK slots.
            *slot = unsafe { *pointer.add(index) };
        }
    }
    if count != 0 && !pointer.is_null() {
        // SAFETY: release only the exact array returned above. VM removal
        // does not revoke its separate local Mach send-name references.
        checked(unsafe {
            vm_deallocate(task, pointer as usize, count as usize * size_of::<u32>())
        })?;
    }
    if !valid {
        return Err(invalid(
            "native registered-port census unavailable or exceeds SDK bound",
        ));
    }
    Ok(ports)
}

/// Sole trusted entry, with async signals blocked and no concurrent Mach port
/// mutator/owner. Every error is terminal, including partial irreversible work.
pub(super) fn revoke() -> io::Result<()> {
    revoke_with_bootstrap(None)
}

/// A bootstrap overlap is preserved only by a genuine private own-task capture,
/// never by a caller-supplied name. That same entry must revoke it immediately
/// afterward, including ALL refs this lookup added; every error is terminal.
pub(super) fn revoke_with_bootstrap(
    bootstrap: Option<&super::bootstrap_port::Captured>,
) -> io::Result<()> {
    // SAFETY: borrowed own-task runtime name, no new right or foreign task.
    let task = unsafe { mach_task_self_ };
    let ports = lookup(task)?;
    let thread = unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) };
    if ports
        .iter()
        .any(|name| *name == u32::MAX || (*name != 0 && (*name == task || *name == thread)))
    {
        return Err(invalid(
            "native registered port aliases a required self handle or dead name",
        ));
    }
    // SAFETY: remove only this task's registered inheritance stash. A saved
    // name may still carry existing local user refs, which must be revoked too.
    checked(unsafe { mach_ports_register(task, std::ptr::null(), 0) })?;
    for (index, name) in ports.iter().copied().enumerate() {
        if name == 0
            || bootstrap.is_some_and(|original| original.local_name() == name)
            || ports.iter().take(index).any(|previous| *previous == name)
        {
            continue;
        }
        let mut references = 0;
        // SAFETY: this current local name was returned by our own-task lookup;
        // no concurrent local mutation is allowed by the trusted entry contract.
        checked(unsafe { mach_port_get_refs(task, name, SEND, &mut references) })?;
        let references = i32::try_from(references)
            .map_err(|_| invalid("native registered-port reference bound"))?;
        if references <= 0 {
            return Err(invalid("native registered-port send references absent"));
        }
        // SAFETY: remove ALL current SEND user refs, including cached and
        // duplicate lookup aliases. Never destroy a receive right or port object.
        checked(unsafe { mach_port_mod_refs(task, name, SEND, -references) })?;
        let mut kind = 0;
        // Require complete disappearance of this local name. Remote death or a
        // remaining receive/dead/other role is uncertain, terminal refusal.
        if unsafe { mach_port_type(task, name, &mut kind) } != INVALID_NAME {
            return Err(invalid(
                "native registered-port name remains or cannot be observed",
            ));
        }
    }
    if lookup(task)?.iter().any(|name| *name != 0) {
        return Err(invalid("native registered-port stash remains"));
    }
    Ok(())
}
