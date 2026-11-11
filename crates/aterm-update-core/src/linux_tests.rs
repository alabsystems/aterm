// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn executable_identity_binds_every_signed_field_and_rejects_dirty_sources() {
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let manifest = crate::Manifest::parse(&format!(
        "schema=1\nversion='0.86.0'\nbuild_number=123\ncommit='{commit}'\ndmg='aterm-0.86.0.dmg'\nsha256='x'\n"
    )).unwrap();
    let identity = BinaryIdentity {
        schema: 1,
        version: manifest.version.clone(),
        build_number: 123,
        commit: commit.into(),
        target: LinuxTarget::Aarch64.triple().into(),
        dirty: false,
    };
    identity
        .verify_against(&manifest, LinuxTarget::Aarch64)
        .unwrap();
    let json = aterm_json::to_vec(&identity).unwrap();
    let back: BinaryIdentity = aterm_json::from_slice(&json).unwrap();
    assert_eq!(identity, back);
    for index in 0..8 {
        let mut wrong = identity.clone();
        match index {
            0 => wrong.schema = 2,
            1 => wrong.version = "0.87.0".into(),
            2 => wrong.build_number = 124,
            3 => wrong.commit = "f".repeat(40),
            4 => wrong.commit.truncate(12),
            5 => wrong.target = LinuxTarget::X86_64.triple().into(),
            6 => wrong.dirty = true,
            _ => wrong.build_number = 0,
        }
        assert!(
            wrong
                .verify_against(&manifest, LinuxTarget::Aarch64)
                .is_err(),
            "mutant {index}"
        );
    }
}

fn header(target: LinuxTarget) -> [u8; 64] {
    let mut bytes = [0; 64];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4..7].copy_from_slice(&[2, 1, 1]);
    bytes[16] = 3;
    bytes[18] = match target {
        LinuxTarget::X86_64 => 62,
        LinuxTarget::Aarch64 => 183,
    };
    bytes[20] = 1;
    bytes[52] = 64;
    bytes
}

#[test]
fn exact_native_and_cross_architecture_headers() {
    for target in [LinuxTarget::X86_64, LinuxTarget::Aarch64] {
        let bytes = header(target);
        target.validate_elf_header(&bytes).unwrap();
        let other = if target == LinuxTarget::X86_64 {
            LinuxTarget::Aarch64
        } else {
            LinuxTarget::X86_64
        };
        assert!(other.validate_elf_header(&bytes).is_err());
    }
}

#[test]
fn elf_compatibility_is_fail_closed() {
    let target = LinuxTarget::Aarch64;
    let bytes = header(target);
    for length in 0..64 {
        assert!(target.validate_elf_header(&bytes[..length]).is_err());
    }
    for (index, wrong) in [(0, 0), (4, 1), (5, 2), (6, 2), (16, 1), (20, 2), (52, 0)] {
        let mut bad = bytes;
        bad[index] = wrong;
        assert!(target.validate_elf_header(&bad).is_err(), "byte {index}");
    }
    let mut executable = bytes;
    executable[16] = 2;
    target.validate_elf_header(&executable).unwrap();
}

#[test]
fn canonical_names_do_not_alias_targets_or_tarballs() {
    assert_eq!(LinuxTarget::Aarch64.triple(), "aarch64-unknown-linux-gnu");
    assert_eq!(
        LinuxTarget::X86_64.asset_name("0.86.0"),
        "aterm-0.86.0-linux-x86_64"
    );
    assert_eq!(
        LinuxTarget::Aarch64.asset_name("0.86.0"),
        "aterm-0.86.0-linux-aarch64"
    );
}
