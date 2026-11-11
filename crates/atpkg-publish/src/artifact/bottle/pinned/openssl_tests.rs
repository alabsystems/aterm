// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::Cell;
use std::os::unix::process::ExitStatusExt;

use super::*;

fn probe(fault: &str) -> Result<()> {
    let work = aterm_tempfile::tempdir().expect("private fixture");
    let verifies = Cell::new(0);
    qualify(
        Path::new("/fixture/openssl-signing"),
        "3.5.9",
        work.path(),
        &|_, args, cwd| {
            let mut code = 0;
            let mut stdout = Vec::new();
            if args == ["version"] {
                stdout = if fault == "version" {
                    b"OpenSSL 3.5.8 fixture\n"
                } else {
                    b"OpenSSL 3.5.9 fixture\n"
                }
                .to_vec();
            } else if args.contains(&"-sign") {
                let key = std::fs::read(cwd.join("fixture-key.der")).expect("generated key");
                let message = std::fs::read(cwd.join("message")).expect("message");
                let mut signature = atpkg_keys::sign(&key, &message).expect("reference signing");
                if fault == "signature" {
                    signature[0] ^= 1;
                }
                if fault == "short" {
                    signature.pop();
                }
                std::fs::write(cwd.join("openssl.sig"), signature).expect("signature");
            } else {
                let n = verifies.get() + 1;
                verifies.set(n);
                code = match n {
                    1 => i32::from(fault == "genuine"),
                    2 if fault == "tamper" => 0,
                    3 if fault == "key" => 0,
                    _ if fault == "crash" => {
                        return Ok(std::process::Output {
                            status: std::process::ExitStatus::from_raw(9),
                            stdout,
                            stderr: Vec::new(),
                        });
                    }
                    _ => 1,
                };
            }
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout,
                stderr: Vec::new(),
            })
        },
    )
}

#[test]
fn source_signer_requires_matching_signatures_and_normal_negative_verdicts() {
    probe("").expect("signatures agree, invalid inputs are refused");
    for fault in [
        "version",
        "signature",
        "short",
        "genuine",
        "tamper",
        "key",
        "crash",
    ] {
        assert!(probe(fault).is_err(), "{fault} escaped qualification");
    }
}
