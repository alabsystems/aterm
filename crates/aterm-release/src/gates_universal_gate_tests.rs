// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::process::ExitStatusExt as _;

use super::*;

fn exited(code: i32) -> std::process::Output {
    std::process::Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: b"bad CPU type in executable".to_vec(),
    }
}

#[test]
fn a_missing_target_stops_before_rosetta_is_asked() {
    universal_gate(
        || Err(Error::new("x86_64-apple-darwin target missing")),
        || unreachable!("a missing target stops before Rosetta is asked"),
    )
    .expect_err("a missing target refuses");
    assert!(universal_gate(|| Ok(()), || Ok(())).is_ok());
}

#[test]
fn the_rosetta_probe_fails_closed() {
    let mut seen = Vec::new();
    let err = rosetta_runs(&mut |command| {
        seen.push(
            std::iter::once(command.get_program())
                .chain(command.get_args())
                .map(|s| s.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
        );
        Ok(exited(1))
    })
    .unwrap_err();
    assert_eq!(err, ROSETTA_REFUSAL);
    assert_eq!(
        seen,
        vec![vec!["/usr/bin/arch", "-x86_64", "/usr/bin/true"]]
    );
    assert_eq!(
        rosetta_runs(&mut |_| Err(std::io::Error::other("no arch"))).unwrap_err(),
        ROSETTA_REFUSAL
    );
    assert_eq!(rosetta_runs(&mut |_| Ok(exited(0))), Ok(()));
}
