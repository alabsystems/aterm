// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::process::ExitStatusExt as _;

use super::*;

#[test]
fn version_and_actual_reference_execution_are_both_required() {
    for fault in ["none", "version", "runtime", "crash", "wrong-output"] {
        let run = |_: &Path, args: &[&str], _: &Path| {
            let version = args == ["--version"];
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(match fault {
                    "crash" if !version => 9,
                    "runtime" if !version => 1 << 8,
                    _ => 0,
                }),
                stdout: if version {
                    if fault == "version" {
                        b"v0.0.0\n".to_vec()
                    } else {
                        b"v22.23.4\n".to_vec()
                    }
                } else if fault == "wrong-output" {
                    b"\n".to_vec()
                } else {
                    b"atpkg Node runtime qualified\n".to_vec()
                },
                stderr: vec![],
            })
        };
        assert_eq!(
            qualify(
                Path::new("/owned/node"),
                "22.23.4",
                Path::new("/owned/work"),
                &run
            )
            .is_ok(),
            fault == "none",
            "{fault}"
        );
    }
}
