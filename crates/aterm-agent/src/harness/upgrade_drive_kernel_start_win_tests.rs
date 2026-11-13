// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#[test]
fn the_start_is_the_filetime_decimal_read_by_syscall() {
    let me = std::process::id();
    let start = super::kernel_start(me).expect("this process's start");
    assert!(
        start.chars().all(|c| c.is_ascii_digit()),
        "a FILETIME decimal, not a `ps` lstart: {start:?}"
    );
    let ticks = aterm_uds::process::read_creation_filetime(i32::try_from(me).unwrap())
        .expect("the raw creation time");
    assert_eq!(start, ticks.to_string(), "the syscall's exact decimal");
    assert_eq!(super::kernel_start(0x7fff_fff0), None, "no such process");
    assert_eq!(super::kernel_start(0), None, "never pid 0");
}
