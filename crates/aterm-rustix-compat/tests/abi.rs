// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Every value this crate names, against rustix 1.1.4's of the same name.
//!
//! The two come from different headers: this crate's from aterm-libc (glibc's
//! values, through libc 0.2.186's per-triple declarations), rustix 1.1.4's
//! from linux-raw-sys (the kernel's UAPI headers). Each comparison is a
//! `const` assertion, so ANY compile of this file for a Linux triple decides
//! it: `xtask gate cells` type-checks every target of every workspace member
//! on x86_64-unknown-linux-gnu and aarch64-unknown-linux-gnu, where several of
//! these differ (O_DIRECT, O_DIRECTORY, O_NOFOLLOW). A wrong value fails the
//! build with the name of the item in the message.

#![cfg(target_os = "linux")]

use core::mem::size_of;

/// `rustix::<module>::<Type>::<FLAG>` and rustix 1.1.4's carry the same bits.
macro_rules! same_bits {
    ($module:ident :: $ty:ident { $($flag:ident),* $(,)? }) => {
        $(
            const _: () = assert!(
                rustix::$module::$ty::$flag.bits() as u64
                    == rustix1::$module::$ty::$flag.bits() as u64,
                concat!(
                    "rustix::", stringify!($module), "::", stringify!($ty), "::",
                    stringify!($flag), " differs from rustix 1.1.4's"
                ),
            );
        )*
    };
}

same_bits!(fs::OFlags {
    RDONLY,
    WRONLY,
    RDWR,
    ACCMODE,
    APPEND,
    CREATE,
    EXCL,
    TRUNC,
    NOCTTY,
    NONBLOCK,
    CLOEXEC,
    DIRECTORY,
    NOFOLLOW,
    NOATIME,
    DIRECT,
    PATH,
});
same_bits!(fs::Mode {
    RWXU,
    RUSR,
    WUSR,
    XUSR,
    RWXG,
    RGRP,
    WGRP,
    XGRP,
    RWXO,
    ROTH,
    WOTH,
    XOTH,
    SUID,
    SGID,
    SVTX,
});
same_bits!(fs::MemfdFlags {
    CLOEXEC,
    ALLOW_SEALING,
    HUGETLB
});
same_bits!(fs::SealFlags {
    SEAL,
    SHRINK,
    GROW,
    WRITE,
    FUTURE_WRITE
});
same_bits!(pipe::PipeFlags {
    CLOEXEC,
    DIRECT,
    NONBLOCK
});
same_bits!(event::EventfdFlags {
    CLOEXEC,
    NONBLOCK,
    SEMAPHORE
});
same_bits!(event::PollFlags {
    IN,
    PRI,
    OUT,
    RDNORM,
    WRNORM,
    RDBAND,
    WRBAND,
    ERR,
    HUP,
    NVAL
});
same_bits!(shm::OFlags {
    CREATE,
    EXCL,
    RDONLY,
    RDWR,
    TRUNC
});

/// `rustix::io::Errno::<NAME>` and rustix 1.1.4's are the same errno.
macro_rules! same_errno {
    ($($name:ident),* $(,)?) => {
        $(
            const _: () = assert!(
                rustix::io::Errno::$name.raw_os_error()
                    == rustix1::io::Errno::$name.raw_os_error(),
                concat!("rustix::io::Errno::", stringify!($name), " differs from rustix 1.1.4's"),
            );
        )*
    };
}

same_errno!(
    AGAIN,
    WOULDBLOCK,
    EXIST,
    INTR,
    INVAL,
    NAMETOOLONG,
    NOSYS,
    PROTO
);

// The shapes a consumer can observe: a `PollFd` array is a `struct pollfd`
// array to both, and a `Pid` (and `Option<Pid>`) is a `pid_t`.
const _: () = assert!(
    size_of::<rustix::event::PollFd<'static>>() == size_of::<rustix1::event::PollFd<'static>>()
);
const _: () = assert!(size_of::<rustix::process::Pid>() == size_of::<rustix1::process::Pid>());
const _: () = assert!(
    size_of::<Option<rustix::process::Pid>>() == size_of::<Option<rustix1::process::Pid>>()
);
const _: () =
    assert!(size_of::<Option<rustix::process::Pid>>() == size_of::<rustix1::process::RawPid>());
