// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Linux architecture claims exercise the shipping shared contract,
//! not a retired tar publisher's duplicate ELF reader. Names, manifest fields,
//! and ELF machine identity must agree; authenticity additionally requires the
//! appcast/roster signatures and full digest checked by the worker/installer.
//! Historical manual tar downloads remain covered by test-check-release-shape.sh.

use aterm_update_core::Manifest;
use aterm_update_core::linux::{LINUX_BINARY_MAX_BYTES, LinuxTarget};

const TARGETS: [LinuxTarget; 2] = [LinuxTarget::X86_64, LinuxTarget::Aarch64];

fn elf(target: LinuxTarget) -> [u8; 64] {
    let mut bytes = [0; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
    let machine: u16 = match target {
        LinuxTarget::X86_64 => 62,
        LinuxTarget::Aarch64 => 183,
    };
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes
}

fn manifest(extra: &str) -> Result<Manifest, String> {
    Manifest::parse(&format!(
        "schema = 1\nversion = \"0.100.0\"\nbuild_number = 100\ndmg = \"aterm-0.100.0.dmg\"\nsha256 = \"{}\"\n{extra}",
        "0".repeat(64)
    ))
}

fn claim(target: LinuxTarget, name: &str, digest: &str, size: u64) -> String {
    let arch = target.arch();
    format!(
        "linux_{arch} = \"{name}\"\nlinux_{arch}_sha256 = \"{digest}\"\nlinux_{arch}_size = {size}\n"
    )
}

#[test]
fn raw_asset_names_and_target_triples_are_exact_and_distinct() {
    assert_eq!(
        LinuxTarget::X86_64.asset_name("0.100.0"),
        "aterm-0.100.0-linux-x86_64"
    );
    assert_eq!(
        LinuxTarget::Aarch64.asset_name("0.100.0"),
        "aterm-0.100.0-linux-aarch64"
    );
    assert_eq!(LinuxTarget::X86_64.triple(), "x86_64-unknown-linux-gnu");
    assert_eq!(LinuxTarget::Aarch64.triple(), "aarch64-unknown-linux-gnu");
}

#[test]
fn each_elf_is_accepted_only_for_its_claimed_machine() {
    for actual in TARGETS {
        for claimed in TARGETS {
            assert_eq!(
                claimed.validate_elf_header(&elf(actual)).is_ok(),
                claimed == actual,
                "ELF {actual:?}, claim {claimed:?}"
            );
        }
        let mut executable = elf(actual);
        executable[16..18].copy_from_slice(&2u16.to_le_bytes());
        assert!(actual.validate_elf_header(&executable).is_ok());
    }
}

#[test]
fn malformed_foreign_truncated_and_non_executable_headers_are_refused() {
    for target in TARGETS {
        assert!(target.validate_elf_header(&elf(target)[..63]).is_err());
        for (offset, byte) in [
            (0, b'M'), // PE/non-ELF
            (4, 1),    // ELF32
            (5, 2),    // big endian
            (6, 0),    // wrong identification version
            (16, 1),   // relocatable, not executable
            (18, 40),  // ARM32/foreign e_machine
            (20, 0),   // wrong ELF version
            (52, 0),   // wrong header size
        ] {
            let mut bytes = elf(target);
            bytes[offset] = byte;
            assert!(
                target.validate_elf_header(&bytes).is_err(),
                "{target:?} accepted malformed byte {offset}"
            );
        }
    }
}

#[test]
fn complete_native_manifest_claims_roundtrip_for_both_targets() {
    let digest = "a".repeat(64);
    let text: String = TARGETS
        .into_iter()
        .map(|target| claim(target, &target.asset_name("0.100.0"), &digest, 80))
        .collect();
    let parsed = manifest(&text).expect("complete native manifest");
    for target in TARGETS {
        let artifact = parsed.linux_artifact(target).unwrap().unwrap();
        assert_eq!(artifact.name, target.asset_name("0.100.0"));
        assert_eq!(artifact.sha256, digest);
        assert_eq!(artifact.size, 80);
    }
    assert_eq!(Manifest::parse(&parsed.to_toml().unwrap()).unwrap(), parsed);
}

#[test]
fn missing_native_target_is_absent_and_partial_claim_is_never_absent() {
    let empty = manifest("").unwrap();
    for target in TARGETS {
        assert!(empty.linux_artifact(target).unwrap().is_none());
        let full = claim(target, &target.asset_name("0.100.0"), &"a".repeat(64), 80);
        let fields: Vec<_> = full.lines().collect();
        for removed in 0..3 {
            let incomplete = fields
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != removed)
                .map(|(_, line)| *line)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(manifest(&incomplete).is_err(), "partial {target:?} claim");
        }
    }
}

#[test]
fn legacy_archives_wrong_version_foreign_target_and_bad_digests_are_refused() {
    for target in TARGETS {
        let canonical = target.asset_name("0.100.0");
        for name in [
            format!("{canonical}.tar.gz"),
            target.asset_name("0.99.0"),
            "../candidate".to_owned(),
            "aterm-0.100.0-linux-arm64".to_owned(),
        ] {
            assert!(manifest(&claim(target, &name, &"a".repeat(64), 80)).is_err());
        }
        for digest in ["a".repeat(63), "A".repeat(64), "z".repeat(64)] {
            assert!(manifest(&claim(target, &canonical, &digest, 80)).is_err());
        }
        for size in [0, 63, LINUX_BINARY_MAX_BYTES + 1] {
            assert!(manifest(&claim(target, &canonical, &"a".repeat(64), size)).is_err());
        }
    }
}

#[cfg(unix)]
#[test]
fn retired_unsigned_uploader_refuses_without_side_effects() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::process::Command::new("bash")
        .arg(root.join("tools/test-linux-auto-release.sh"))
        .current_dir(&root)
        .output()
        .expect("run hermetic retirement guard");
    assert!(
        out.status.success()
            && String::from_utf8_lossy(&out.stdout).contains("test-linux-auto-release: PASS"),
        "retirement guard failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
