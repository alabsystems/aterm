// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Behaviour sweeps the oracle ([`super::oracle`]) runs over the shim's hand-written bodies.

use super::oracle::{REAL, SHIM};

/// The shim and reference crates substituted into a sweep.
#[must_use]
pub fn crates(template: &str) -> String {
    template.replace("@S@", SHIM).replace("@R@", REAL)
}

const STATUS: &str = r#"let mut s: i32 = -1; while s < 0x1_0000 { assert!(@S@::@N@(s) == @R@::@N@(s), "@N@ differs"); s += 1; } let edge = [i32::MIN, i32::MIN + 1, -0x1_0000, -0x8001, 0x1_0000, 0x1234_5678, i32::MAX]; let mut i = 0; while i < edge.len() { let s = edge[i]; assert!(@S@::@N@(s) == @R@::@N@(s), "@N@ differs"); i += 1; }"#;

/// Sweeps that run at compile time when the macro is a `const fn`, in emission order.
#[must_use]
pub fn evaluable() -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = [
        "WSTOPSIG",
        "WIFSTOPPED",
        "WIFCONTINUED",
        "WIFEXITED",
        "WEXITSTATUS",
        "WIFSIGNALED",
        "WTERMSIG",
    ]
    .into_iter()
    .map(|name| (name, STATUS.replace("@N@", name)))
    .collect();
    out.extend([
        ("makedev", r#"let mut i = 0u32; while i < 4096 { let hi = i.wrapping_mul(2654435761); let lo = i.wrapping_mul(40503); assert!(@S@::makedev(hi as _, lo as _) as u128 == @R@::makedev(hi as _, lo as _) as u128, "makedev differs"); i += 1; }"#.to_string()),
        ("major", r#"let mut i = 0u64; while i < 4096 { let dev = i.wrapping_mul(0x9e37_79b9_7f4a_7c15); assert!(@S@::major(dev as _) as u128 == @R@::major(dev as _) as u128, "major differs"); i += 1; }"#.to_string()),
        ("minor", r#"let mut i = 0u64; while i < 4096 { let dev = i.wrapping_mul(0x9e37_79b9_7f4a_7c15); assert!(@S@::minor(dev as _) as u128 == @R@::minor(dev as _) as u128, "minor differs"); i += 1; }"#.to_string()),
    ]);
    out
}

/// Sweeps that write through pointers and run only as native tests.
pub const NATIVE: [(&str, &str); 6] = [
    (
        "FD_ZERO",
        r#"unsafe { let mut a: @S@::fd_set = ::core::mem::zeroed(); let mut b: @R@::fd_set = ::core::mem::zeroed(); ::core::slice::from_raw_parts_mut((&mut a as *mut @S@::fd_set).cast::<u8>(), ::core::mem::size_of_val(&a)).fill(0xa5); ::core::slice::from_raw_parts_mut((&mut b as *mut @R@::fd_set).cast::<u8>(), ::core::mem::size_of_val(&b)).fill(0xa5); @S@::FD_ZERO(&mut a); @R@::FD_ZERO(&mut b); let ab = ::core::slice::from_raw_parts((&a as *const @S@::fd_set).cast::<u8>(), ::core::mem::size_of_val(&a)); let bb = ::core::slice::from_raw_parts((&b as *const @R@::fd_set).cast::<u8>(), ::core::mem::size_of_val(&b)); assert_eq!(ab, bb, "FD_ZERO writes a different bit pattern"); }"#,
    ),
    (
        "FD_SET",
        r#"unsafe { assert_eq!(::core::mem::size_of::<@S@::fd_set>(), ::core::mem::size_of::<@R@::fd_set>()); let mut accumulated_a: @S@::fd_set = ::core::mem::zeroed(); let mut accumulated_b: @R@::fd_set = ::core::mem::zeroed(); for fd in 0..@S@::FD_SETSIZE as i32 { let mut individual_a: @S@::fd_set = ::core::mem::zeroed(); let mut individual_b: @R@::fd_set = ::core::mem::zeroed(); @S@::FD_SET(fd, &mut individual_a); @R@::FD_SET(fd, &mut individual_b); let individual_ab = ::core::slice::from_raw_parts((&individual_a as *const @S@::fd_set).cast::<u8>(), ::core::mem::size_of_val(&individual_a)); let individual_bb = ::core::slice::from_raw_parts((&individual_b as *const @R@::fd_set).cast::<u8>(), ::core::mem::size_of_val(&individual_b)); assert_eq!(individual_ab, individual_bb, "FD_SET individual bit pattern differs at fd {}", fd); @S@::FD_SET(fd, &mut accumulated_a); @R@::FD_SET(fd, &mut accumulated_b); let accumulated_ab = ::core::slice::from_raw_parts((&accumulated_a as *const @S@::fd_set).cast::<u8>(), ::core::mem::size_of_val(&accumulated_a)); let accumulated_bb = ::core::slice::from_raw_parts((&accumulated_b as *const @R@::fd_set).cast::<u8>(), ::core::mem::size_of_val(&accumulated_b)); assert_eq!(accumulated_ab, accumulated_bb, "FD_SET accumulated bit pattern differs after fd {}", fd); } }"#,
    ),
    (
        "FD_ISSET",
        r#"unsafe { assert_eq!(::core::mem::size_of::<@S@::fd_set>(), ::core::mem::size_of::<@R@::fd_set>()); let bytes = ::core::mem::size_of::<@S@::fd_set>(); for seeded_fd in 0..@S@::FD_SETSIZE as i32 { let mut b: @R@::fd_set = ::core::mem::zeroed(); @R@::FD_SET(seeded_fd, &mut b); let mut a: @S@::fd_set = ::core::mem::zeroed(); ::core::ptr::copy_nonoverlapping((&b as *const @R@::fd_set).cast::<u8>(), (&mut a as *mut @S@::fd_set).cast::<u8>(), bytes); for fd in 0..@S@::FD_SETSIZE as i32 { assert_eq!(@S@::FD_ISSET(fd, &a), @R@::FD_ISSET(fd, &b), "FD_ISSET differs at queried fd {} for reference-seeded fd {}", fd, seeded_fd); } } }"#,
    ),
    (
        "CMSG_LEN",
        r#"for n in (0u32..4096).chain([65535u32, 65536, 1<<20]) { unsafe { assert_eq!(@S@::CMSG_LEN(n), @R@::CMSG_LEN(n), "CMSG_LEN differs at {}", n); } }"#,
    ),
    (
        "CMSG_SPACE",
        r#"for n in (0u32..4096).chain([65535u32, 65536, 1<<20]) { unsafe { assert_eq!(@S@::CMSG_SPACE(n), @R@::CMSG_SPACE(n), "CMSG_SPACE differs at {}", n); } }"#,
    ),
    (
        "CMSG_FIRSTHDR",
        r#"unsafe { let mut buf = [0u8; 256]; for controllen in [0usize, 1, 15, 16, 24, 64, 256] { let mut ma: @S@::msghdr = ::core::mem::zeroed(); ma.msg_control = buf.as_mut_ptr().cast(); ma.msg_controllen = controllen as _; let mb: @R@::msghdr = ::core::mem::transmute_copy(&ma); let pa = @S@::CMSG_FIRSTHDR(&ma) as usize; let pb = @R@::CMSG_FIRSTHDR(&mb) as usize; assert_eq!(pa, pb, "CMSG_FIRSTHDR differs at controllen {}", controllen); } }"#,
    ),
];

/// What each `siginfo_t` accessor's result is compared as (`@C@` the value).
#[must_use]
pub fn siginfo_projection(method: &str) -> Option<&'static str> {
    Some(match method {
        "si_addr" => "@C@.si_addr() as usize",
        "si_value" => "@C@.si_value().sival_ptr as usize",
        "si_pid" => "@C@.si_pid() as i128",
        "si_uid" => "@C@.si_uid() as i128",
        "si_status" => "@C@.si_status() as i128",
        "si_utime" => "@C@.si_utime() as i128",
        "si_stime" => "@C@.si_stime() as i128",
        _ => return None,
    })
}

/// Builds the byte-sweep comparison for a `siginfo_t` accessor.
#[must_use]
pub fn siginfo(method: &str, projection: &str) -> String {
    let template = r#"unsafe { assert_eq!(::core::mem::size_of::<@S@::siginfo_t>(), ::core::mem::size_of::<@R@::siginfo_t>()); for byte in 0..::core::mem::size_of::<@S@::siginfo_t>() { for value in [1u8, 0x5a, 0x80, 0xff] { let mut a: @S@::siginfo_t = ::core::mem::zeroed(); let mut b: @R@::siginfo_t = ::core::mem::zeroed(); ((&mut a as *mut @S@::siginfo_t).cast::<u8>().add(byte)).write(value); ((&mut b as *mut @R@::siginfo_t).cast::<u8>().add(byte)).write(value); assert_eq!(@A@, @B@, "siginfo_t::@M@ differs at byte {} value {:#x}", byte, value); } } }"#;
    crates(
        &template
            .replace("@A@", &projection.replace("@C@", "a"))
            .replace("@B@", &projection.replace("@C@", "b"))
            .replace("@M@", method),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweeps_name_both_crates_and_their_subject() {
        let (name, sweep) = &evaluable()[0];
        assert_eq!(*name, "WSTOPSIG");
        let sweep = crates(sweep);
        assert!(sweep.contains(
            "assert!(libc_shim::WSTOPSIG(s) == libc_real::WSTOPSIG(s), \"WSTOPSIG differs\")"
        ));
        assert!(!sweep.contains('@'));
        let si = siginfo("si_pid", siginfo_projection("si_pid").expect("known"));
        assert!(si.contains(
            "assert_eq!(a.si_pid() as i128, b.si_pid() as i128, \"siginfo_t::si_pid differs"
        ));
        assert!(!si.contains('@'));
        assert_eq!(siginfo_projection("si_band"), None);
    }
}
