// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hand-written C macro bodies, `siginfo_t` accessors, and struct notes for regenerated libc shims.

use crate::cells::Os;

/// One C macro libc reimplements as a Rust function: its body, unindented.
pub struct MacroBody {
    pub name: &'static str,
    /// Whether the shim writes it `const fn` to match the reference.
    pub is_const: bool,
    pub body: &'static str,
}

const DARWIN_MACROS: &[MacroBody] = &[
    MacroBody {
        name: "CMSG_LEN",
        is_const: true,
        body: "(cmsg_align(::core::mem::size_of::<cmsghdr>()) + length as usize) as c_uint",
    },
    MacroBody {
        name: "CMSG_SPACE",
        is_const: true,
        body: "(cmsg_align(::core::mem::size_of::<cmsghdr>()) + cmsg_align(length as usize)) as c_uint",
    },
    MacroBody {
        name: "CMSG_FIRSTHDR",
        is_const: false,
        body: r"// SAFETY: the caller supplies a valid msghdr describing one control allocation.
unsafe {
    if (*mhdr).msg_controllen as usize >= ::core::mem::size_of::<cmsghdr>() {
        (*mhdr).msg_control.cast::<cmsghdr>()
    } else { ::core::ptr::null_mut() }
}",
    },
    MacroBody {
        name: "CMSG_DATA",
        is_const: false,
        body: r"// SAFETY: the caller supplies a header in its valid control-message allocation.
unsafe { (cmsg as *mut c_uchar).add(cmsg_align(::core::mem::size_of::<cmsghdr>())) }",
    },
    MacroBody {
        name: "CMSG_NXTHDR",
        is_const: false,
        body: r"// SAFETY: both pointers describe the caller's valid control allocation/header.
unsafe {
    if cmsg.is_null() { return CMSG_FIRSTHDR(mhdr); }
    let next = cmsg as usize + cmsg_align((*cmsg).cmsg_len as usize);
    let max = (*mhdr).msg_control as usize + (*mhdr).msg_controllen as usize;
    if next + cmsg_align(::core::mem::size_of::<cmsghdr>()) > max {
        ::core::ptr::null_mut()
    } else { next as *mut cmsghdr }
}",
    },
    MacroBody {
        name: "WSTOPSIG",
        is_const: true,
        body: "status >> 8",
    },
    MacroBody {
        name: "WIFSTOPPED",
        is_const: true,
        body: "(status & 0o177) == 0o177 && (status >> 8) != 0x13",
    },
    MacroBody {
        name: "WIFCONTINUED",
        is_const: true,
        body: "(status & 0o177) == 0o177 && (status >> 8) == 0x13",
    },
    MacroBody {
        name: "major",
        is_const: true,
        body: "(dev >> 24) & 0xff",
    },
    MacroBody {
        name: "minor",
        is_const: true,
        body: "dev & 0xff_ffff",
    },
    MacroBody {
        name: "FD_ZERO",
        is_const: false,
        body: "unsafe { (*set).__opaque.fill(0) }",
    },
    MacroBody {
        name: "WTERMSIG",
        is_const: true,
        body: "status & 0o177",
    },
    MacroBody {
        name: "WIFEXITED",
        is_const: true,
        body: "(status & 0o177) == 0",
    },
    MacroBody {
        name: "WEXITSTATUS",
        is_const: true,
        body: "(status >> 8) & 0x00ff",
    },
    MacroBody {
        name: "WIFSIGNALED",
        is_const: true,
        body: "(status & 0o177) != 0o177 && (status & 0o177) != 0",
    },
    MacroBody {
        name: "makedev",
        is_const: true,
        body: "(major << 24) | minor",
    },
    MacroBody {
        name: "FD_SET",
        is_const: false,
        body: r"let bits = ::core::mem::size_of::<fd_word>() * 8;
let fd = fd as usize;
unsafe { (*set).__opaque[fd / bits] |= 1 << (fd % bits) };",
    },
    MacroBody {
        name: "FD_ISSET",
        is_const: false,
        body: r"let bits = ::core::mem::size_of::<fd_word>() * 8;
let fd = fd as usize;
unsafe { ((*set).__opaque[fd / bits] & (1 << (fd % bits))) != 0 }",
    },
];

const GNU_LINUX_MACROS: &[MacroBody] = &[
    MacroBody {
        name: "CMSG_DATA",
        is_const: false,
        body: r"// SAFETY: the caller supplies a header in its valid control-message allocation.
unsafe { cmsg.add(1).cast_mut().cast::<c_uchar>() }",
    },
    MacroBody {
        name: "CMSG_NXTHDR",
        is_const: false,
        body: r"// SAFETY: both pointers describe the caller's valid control allocation/header.
unsafe {
    if (*cmsg).cmsg_len < ::core::mem::size_of::<cmsghdr>() {
        return ::core::ptr::null_mut();
    }
    let next = cmsg.cast::<u8>().wrapping_add(cmsg_align((*cmsg).cmsg_len)).cast::<cmsghdr>();
    if (next as usize) < (cmsg as usize) {
        return ::core::ptr::null_mut();
    }
    let max = (*mhdr).msg_control as usize + (*mhdr).msg_controllen;
    if next as usize + ::core::mem::size_of::<cmsghdr>() > max {
        ::core::ptr::null_mut()
    } else { next.cast_mut() }
}",
    },
    MacroBody {
        name: "WSTOPSIG",
        is_const: true,
        body: "(status >> 8) & 0xff",
    },
    MacroBody {
        name: "WIFSTOPPED",
        is_const: true,
        body: "(status & 0xff) == 0x7f",
    },
    MacroBody {
        name: "WIFCONTINUED",
        is_const: true,
        body: "status == 0xffff",
    },
    MacroBody {
        name: "major",
        is_const: true,
        body: r"let mut major = 0;
major |= (dev & 0x0000_0000_000f_ff00) >> 8;
major |= (dev & 0xffff_f000_0000_0000) >> 32;
major as c_uint",
    },
    MacroBody {
        name: "minor",
        is_const: true,
        body: r"let mut minor = 0;
minor |= dev & 0x0000_0000_0000_00ff;
minor |= (dev & 0x0000_0fff_fff0_0000) >> 12;
minor as c_uint",
    },
    MacroBody {
        name: "FD_ZERO",
        is_const: false,
        body: "unsafe { (*set).__opaque.fill(0) }",
    },
    MacroBody {
        name: "WTERMSIG",
        is_const: true,
        body: "status & 0x7f",
    },
    MacroBody {
        name: "WIFEXITED",
        is_const: true,
        body: "(status & 0x7f) == 0",
    },
    MacroBody {
        name: "WEXITSTATUS",
        is_const: true,
        body: "(status >> 8) & 0xff",
    },
    MacroBody {
        name: "WIFSIGNALED",
        is_const: true,
        body: "((status & 0x7f) + 1) as i8 >= 2",
    },
    MacroBody {
        name: "makedev",
        is_const: true,
        body: r"let major = major as dev_t;
let minor = minor as dev_t;
let mut dev = 0;
dev |= (major & 0x0000_0fff) << 8;
dev |= (major & 0xffff_f000) << 32;
dev |= minor & 0x0000_00ff;
dev |= (minor & 0xffff_ff00) << 12;
dev",
    },
    MacroBody {
        name: "FD_SET",
        is_const: false,
        body: r"let bits = ::core::mem::size_of::<fd_word>() * 8;
let fd = fd as usize;
unsafe { (*set).__opaque[fd / bits] |= 1 << (fd % bits) };",
    },
    MacroBody {
        name: "FD_ISSET",
        is_const: false,
        body: r"let bits = ::core::mem::size_of::<fd_word>() * 8;
let fd = fd as usize;
((unsafe { (*set).__opaque[fd / bits] }) & (1 << (fd % bits))) != 0",
    },
    MacroBody {
        name: "CMSG_LEN",
        is_const: true,
        body: "cmsg_align(::core::mem::size_of::<cmsghdr>()) as c_uint + length",
    },
    MacroBody {
        name: "CMSG_SPACE",
        is_const: true,
        body: r"(cmsg_align(length as usize) + cmsg_align(::core::mem::size_of::<cmsghdr>()))
    as c_uint",
    },
    MacroBody {
        name: "CMSG_FIRSTHDR",
        is_const: false,
        body: r"unsafe {
    if (*mhdr).msg_controllen >= ::core::mem::size_of::<cmsghdr>() {
        (*mhdr).msg_control.cast::<cmsghdr>()
    } else {
        ::core::ptr::null_mut::<cmsghdr>()
    }
}",
    },
];

/// The family's macro bodies.
#[must_use]
pub const fn macros(os: Os) -> &'static [MacroBody] {
    match os {
        Os::Darwin => DARWIN_MACROS,
        Os::GnuLinux => GNU_LINUX_MACROS,
    }
}

/// The body written for the macro `name`, if there is one.
#[must_use]
pub fn macro_body(os: Os, name: &str) -> Option<&'static MacroBody> {
    macros(os).iter().find(|m| m.name == name)
}

/// `# Safety` doc lines for a public unsafe macro.
#[must_use]
pub fn safety_doc(os: Os, name: &str) -> Option<&'static [&'static str]> {
    if os == Os::GnuLinux && name == "CMSG_NXTHDR" {
        return Some(&[
            "`mhdr` must describe one valid control allocation; `cmsg` must point",
            "to a valid header within it whose length can be read.",
        ]);
    }
    const ARITHMETIC: &[&str] = &[
        "Always sound: this is arithmetic only. It is `unsafe` because",
        "upstream declares it so, and the signature is part of the contract.",
    ];
    Some(match name {
        "FD_SET" => &[
            "`set` must point to a valid, writable `fd_set`, and `fd` must be in",
            "`0..FD_SETSIZE` — out of range indexes past the end of the bitmap.",
        ],
        "FD_ISSET" => &[
            "`set` must point to a valid, initialised `fd_set`, and `fd` must be in",
            "`0..FD_SETSIZE` — out of range reads past the end of the bitmap.",
        ],
        "FD_ZERO" => &["`set` must point to a valid, writable `fd_set`."],
        "CMSG_FIRSTHDR" => &[
            "`mhdr` must point to a valid `msghdr` whose `msg_control` /",
            "`msg_controllen` describe one allocation.",
        ],
        "CMSG_LEN" | "CMSG_SPACE" => ARITHMETIC,
        "CMSG_DATA" => &["`cmsg` must point to a header in a valid control-message allocation."],
        "CMSG_NXTHDR" => &[
            "`mhdr` must describe one valid control allocation; `cmsg` is null or points",
            "to a valid header within it whose length can be read.",
        ],
        _ => return None,
    })
}

/// The integer word type of the `fd_set` bitmap.
#[must_use]
pub const fn fd_word(os: Os) -> &'static str {
    match os {
        Os::Darwin => "u32",
        Os::GnuLinux => "u64",
    }
}

/// Inherent `siginfo_t` accessor impl block for `os`.
#[must_use]
pub const fn siginfo_impls(os: Os) -> &'static str {
    match os {
        Os::Darwin => DARWIN_SIGINFO,
        Os::GnuLinux => GNU_LINUX_SIGINFO,
    }
}

/// Extracts `pub unsafe fn` names emitted by `source`.
#[must_use]
pub fn public_unsafe_fns(source: &str) -> Vec<String> {
    let words: Vec<&str> = source.split_whitespace().collect();
    (0..words.len())
        .filter(|&i| words[i..].starts_with(&["pub", "unsafe", "fn"]))
        .filter_map(|i| {
            let word = words.get(i + 3)?;
            let name: String = word
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let rest = &word[name.len()..];
            let opens = rest.starts_with('(')
                || (rest.is_empty() && words.get(i + 4).is_some_and(|w| w.starts_with('(')));
            (!name.is_empty() && opens).then_some(name)
        })
        .collect()
}

/// Extra doc note emitted on regenerated struct `name`, if any.
#[must_use]
pub fn struct_note(os: Os, name: &str) -> Option<&'static str> {
    match (os, name) {
        (Os::Darwin, "vm_statistics64") => Some(VM_STATISTICS64),
        _ => None,
    }
}

const VM_STATISTICS64: &str = r"UPSTREAM-PARITY LAYOUT.

This declaration follows the measured reference libc 0.2.191, including its
extended counters. HOST_VM_INFO64_COUNT is measured from the same reference
layout; callers must keep that count paired with this buffer, rather than
substituting an independently obtained SDK count. aterm-sysprobe passes this
paired count and reads only the original swapins/swapouts fields. The kernel
may return fewer words on older systems; the caller zero-initializes the
complete buffer before the query.";

const DARWIN_SIGINFO: &str = r"impl siginfo_t {
    /// # Safety
    ///
    /// The caller must know the signal was one that sets `si_addr`
    /// (SIGSEGV, SIGBUS, SIGILL, SIGFPE); the field is a union arm and reading
    /// the wrong arm reads uninitialised bytes.
    pub unsafe fn si_addr(&self) -> *mut c_void {
        self.si_addr
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_pid(&self) -> pid_t {
        self.si_pid
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_uid(&self) -> uid_t {
        self.si_uid
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_status(&self) -> c_int {
        self.si_status
    }

    /// # Safety
    ///
    /// The caller must know the signal was queued with a value
    /// (`sigqueue`, or a POSIX timer); this reads a union arm.
    pub unsafe fn si_value(&self) -> sigval {
        #[repr(C)]
        struct siginfo_timer {
            _si_signo: c_int,
            _si_errno: c_int,
            _si_code: c_int,
            _si_pid: pid_t,
            _si_uid: uid_t,
            _si_status: c_int,
            _si_addr: *mut c_void,
            si_value: sigval,
        }
        unsafe { (*(self as *const siginfo_t).cast::<siginfo_timer>()).si_value }
    }
}";

const GNU_LINUX_SIGINFO: &str = r"impl siginfo_t {
    // The supported GNU cells are 64-bit x86_64/aarch64. These views follow
    // libc 0.2.191 new/glibc/sysdeps/unix/linux/bits/types/siginfo_t.rs.

    /// # Safety
    /// The caller must know the signal initializes the timer union arm.
    pub unsafe fn si_timerid(&self) -> c_int {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            timer: c_int, _overrun: c_int,
        }
        // SAFETY: The caller selects the initialized arm; this repr(C) view
        // follows the reference's four-word prefix and that arm's field order.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).timer }
    }

    /// # Safety
    /// The caller must know the signal initializes the timer union arm.
    pub unsafe fn si_overrun(&self) -> c_int {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            _timer: c_int, overrun: c_int,
        }
        // SAFETY: The caller selects the initialized arm; this repr(C) view
        // follows the reference's four-word prefix and that arm's field order.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).overrun }
    }

    /// # Safety
    /// The caller must know the signal initializes the fault union arm.
    pub unsafe fn si_addr_lsb(&self) -> c_short {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            _address: *mut c_void, lsb: c_short,
        }
        // SAFETY: The caller selects the initialized arm; this repr(C) view
        // follows the reference's four-word prefix and that arm's field order.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).lsb }
    }

    /// # Safety
    /// The caller must know the signal initializes the bounds union arm.
    pub unsafe fn si_lower(&self) -> *mut c_void {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            _address: *mut c_void, _lsb: c_short, lower: *mut c_void, _upper: *mut c_void,
        }
        // SAFETY: The caller selects the initialized arm; this repr(C) view
        // follows the reference's four-word prefix and that arm's field order.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).lower }
    }

    /// # Safety
    /// The caller must know the signal initializes the bounds union arm.
    pub unsafe fn si_upper(&self) -> *mut c_void {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            _address: *mut c_void, _lsb: c_short, _lower: *mut c_void, upper: *mut c_void,
        }
        // SAFETY: The caller selects the initialized arm; this repr(C) view
        // follows the reference's four-word prefix and that arm's field order.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).upper }
    }

    /// # Safety
    /// The caller must know the signal initializes the fault pkey union arm.
    pub unsafe fn si_pkey(&self) -> u32 {
        #[repr(C)]
        union Bounds {
            _alignment: *mut c_void,
            pkey: u32,
        }
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            _address: *mut c_void, _lsb: c_short, bounds: Bounds,
        }
        // SAFETY: The caller selects the initialized pkey arm. Bounds retains
        // the reference union's pointer alignment, but reads only its u32 arm,
        // never the potentially uninitialized bytes following that scalar.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).bounds.pkey }
    }

    /// # Safety
    /// The caller must know the signal initializes the poll union arm.
    pub unsafe fn si_band(&self) -> c_long {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            band: c_long, _fd: c_int,
        }
        // SAFETY: The caller selects the initialized arm; this repr(C) view
        // follows the reference's four-word prefix and that arm's field order.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).band }
    }

    /// # Safety
    /// The caller must know the signal initializes the poll union arm.
    pub unsafe fn si_fd(&self) -> c_int {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            _band: c_long, fd: c_int,
        }
        // SAFETY: The caller selects the initialized arm; this repr(C) view
        // follows the reference's four-word prefix and that arm's field order.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).fd }
    }

    /// # Safety
    /// The caller must know the signal initializes the syscall union arm.
    pub unsafe fn si_call_addr(&self) -> *mut c_void {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            address: *mut c_void, _syscall: c_int, _arch: c_uint,
        }
        // SAFETY: The caller selects the initialized arm; this repr(C) view
        // follows the reference's four-word prefix and that arm's field order.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).address }
    }

    /// # Safety
    /// The caller must know the signal initializes the syscall union arm.
    pub unsafe fn si_syscall(&self) -> c_int {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            _address: *mut c_void, syscall: c_int, _arch: c_uint,
        }
        // SAFETY: The caller selects the initialized arm; this repr(C) view
        // follows the reference's four-word prefix and that arm's field order.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).syscall }
    }

    /// # Safety
    /// The caller must know the signal initializes the syscall union arm.
    pub unsafe fn si_arch(&self) -> c_uint {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            _address: *mut c_void, _syscall: c_int, arch: c_uint,
        }
        // SAFETY: The caller selects the initialized arm; this repr(C) view
        // follows the reference's four-word prefix and that arm's field order.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).arch }
    }

    /// # Safety
    /// The caller must know the queued signal initializes its integer value.
    pub unsafe fn si_int(&self) -> c_int {
        #[repr(C)]
        struct View {
            _header: [c_int; 4],
            _pid: pid_t,
            _uid: uid_t,
            value: c_int,
        }
        // SAFETY: The caller establishes the queued integer field. This reads
        // that c_int alone, not the padding of the pointer-sized value union.
        unsafe { (*(self as *const siginfo_t).cast::<View>()).value }
    }

    /// # Safety
    /// The caller must know the queued signal initializes its value arm.
    pub unsafe fn si_ptr(&self) -> *mut c_void {
        // SAFETY: The caller establishes the queued-value arm.
        unsafe { self.si_value().sival_ptr }
    }

    /// # Safety
    ///
    /// The caller must know the signal was one that sets `si_addr`
    /// (SIGSEGV, SIGBUS, SIGILL, SIGFPE); the field is a union arm and reading
    /// the wrong arm reads uninitialised bytes.
    pub unsafe fn si_addr(&self) -> *mut c_void {
        #[repr(C)]
        struct siginfo_sigfault {
            _si_signo: c_int,
            _si_errno: c_int,
            _si_code: c_int,
            si_addr: *mut c_void,
        }
        unsafe { (*(self as *const siginfo_t).cast::<siginfo_sigfault>()).si_addr }
    }

    /// # Safety
    ///
    /// The caller must know the signal was queued with a value
    /// (`sigqueue`, or a POSIX timer); this reads a union arm.
    pub unsafe fn si_value(&self) -> sigval {
        #[repr(C)]
        struct siginfo_timer {
            _si_signo: c_int,
            _si_errno: c_int,
            _si_code: c_int,
            _si_tid: c_int,
            _si_overrun: c_int,
            si_sigval: sigval,
        }
        unsafe { (*(self as *const siginfo_t).cast::<siginfo_timer>()).si_sigval }
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_pid(&self) -> pid_t {
        unsafe { self.sigchld().si_pid }
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_uid(&self) -> uid_t {
        unsafe { self.sigchld().si_uid }
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_status(&self) -> c_int {
        unsafe { self.sigchld().si_status }
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_utime(&self) -> c_long {
        unsafe { self.sigchld().si_utime }
    }

    /// # Safety
    ///
    /// The caller must know the signal was a SIGCHLD; this reads a union arm.
    pub unsafe fn si_stime(&self) -> c_long {
        unsafe { self.sigchld().si_stime }
    }

    /// The `sigchld` arm of the private `_sifields` union. Note the pointer in
    /// the alignment slot: some arms of that union start with one, which is
    /// what fixes where the arm begins.
    unsafe fn sigchld(&self) -> &sifields_sigchld {
        #[repr(C)]
        struct sifields_sigchld_arm {
            _align_pointer: *mut c_void,
        }
        #[repr(C)]
        struct siginfo_f {
            _siginfo_base: [c_int; 3],
            _align: sifields_sigchld_arm,
        }
        let base = (self as *const siginfo_t).cast::<siginfo_f>();
        unsafe { &*(&raw const (*base)._align).cast::<sifields_sigchld>() }
    }
}

#[repr(C)]
struct sifields_sigchld {
    si_pid: pid_t,
    si_uid: uid_t,
    si_status: c_int,
    si_utime: c_long,
    si_stime: c_long,
}";

#[cfg(test)]
#[path = "handwritten_tests.rs"]
mod tests;
