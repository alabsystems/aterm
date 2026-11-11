// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::ffi::OsString;
use std::process::ExitCode;

#[test]
fn tick_ms_rejects_non_canonical_unsigned_values() {
    let args: Vec<OsString> = [
        "--listen",
        "/tmp/l.sock",
        "--watch",
        "/tmp/w.sock",
        "--key",
        "/tmp/k.img",
        "--tick-ms",
        "+20",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    assert_eq!(super::main(&args), ExitCode::from(2));
}
