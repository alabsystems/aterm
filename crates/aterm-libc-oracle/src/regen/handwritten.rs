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
pub fn safety_doc(name: &str) -> Option<&'static [&'static str]> {
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

const VM_STATISTICS64: &str = r"UPSTREAM-PARITY DIVERGENCE FROM THE PLATFORM SDK, inherited and deliberate.

libc 0.2.186 declares this struct at Mach **rev1** -- the 24 fields and 152
bytes measured here. The SDK on the machine that generated this file
(`MacOSX.sdk/usr/include/mach/vm_statistics.h`) is at **rev3**: 36 fields and
248 bytes, rev2 having added `swapped_count` and rev3 eleven more counters.
This crate reproduces UPSTREAM byte-for-byte, so the oracle is green on it and
the crates.io `libc` it replaces would behave identically.

That parity is the decision, not an oversight. It was declared because rustix
names it: `pub(super) use host_statistics64 as host_statistics, vm_statistics64_t
as vm_statistics_t` in `src/backend/libc/c.rs` (0.38.44 and 1.1.4, called by
neither). Since 2026-09-24 ONE caller exists: `aterm-sysprobe` reads `swapins`
and `swapouts` for the memory ceiling, and it passes upstream's own
`HOST_VM_INFO64_COUNT` -- THIS layout's size in words, 38 -- so the kernel
writes the rev1 prefix and nothing past it. Both fields it reads are rev1
fields. For a drop-in replacement, differing from the crate it replaces is
still the worse property.

This layout is NOT the one to hand `host_statistics64` alongside an SDK-derived
`HOST_VM_INFO64_COUNT` (62 words): the kernel would write 248 bytes into 152.
A caller that needs a rev2 or rev3 counter widens it here deliberately, and
states the divergence where the oracle can see it rather than leaving it
silent.";

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
mod tests {
    use super::*;

    #[test]
    fn the_emitted_accessors_are_read_off_the_impl_text() {
        assert_eq!(
            public_unsafe_fns(siginfo_impls(Os::Darwin)),
            ["si_addr", "si_pid", "si_uid", "si_status", "si_value"]
        );
        let linux = public_unsafe_fns(siginfo_impls(Os::GnuLinux));
        assert_eq!(
            linux.len(),
            7,
            "{linux:?}: the private `sigchld` is not one"
        );
        assert_eq!(
            public_unsafe_fns("pub unsafe fn (x)\npub  unsafe\tfn a_1 (y) pub unsafe fn b(z)"),
            ["a_1", "b"]
        );
        assert!(public_unsafe_fns("pub fn safe(&self)").is_empty());
    }
}
