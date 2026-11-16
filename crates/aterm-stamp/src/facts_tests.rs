// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Cursor;

fn stamp(debug: bool) -> Stamp {
    Stamp {
        build_profile: "release",
        debug_assertions: Some(debug),
        target_arch: "aarch64",
        compiler_commit: "0123456789012345678901234567890123456789",
        compiler_host: "aarch64-apple-darwin",
        ..Stamp::UNSTAMPED
    }
}

fn image(record: &[u8; LEN]) -> Vec<u8> {
    let mut bytes = vec![0; 256 + LEN];
    for (at, word) in [
        (0, 0xfeed_facf_u32),
        (4, 0x0100_000c),
        (12, 2),
        (16, 1),
        (20, 152),
        (32, 0x19),
        (36, 152),
        (92, 5),
        (96, 1),
        (152, 256),
    ] {
        bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes[40..46].copy_from_slice(b"__TEXT");
    let len = bytes.len() as u64;
    bytes[80..88].copy_from_slice(&len.to_le_bytes());
    bytes[104..117].copy_from_slice(SECTION);
    bytes[120..126].copy_from_slice(b"__TEXT");
    bytes[144..152].copy_from_slice(&(LEN as u64).to_le_bytes());
    bytes[256..].copy_from_slice(record);
    bytes
}

#[test]
fn actual_assertions_survive_profile_names_and_product_identity_patches() {
    let compiled = stamp(true);
    let record = encode(compiled);
    let facts = decode(&record).unwrap();
    assert!(
        facts.debug_assertions,
        "a custom release-derived profile may retain debug assertions"
    );
    assert_eq!(facts.profile, "release");
    let mut bytes = image(&record);
    bytes.extend_from_slice(&crate::UNSTAMPED_RECORD);
    crate::rewrite(
        &mut bytes,
        &Stamp {
            app_version: "1.2.3",
            release_build: true,
            ..compiled
        },
    )
    .unwrap();
    assert_eq!(macho_records(&mut Cursor::new(bytes)).unwrap(), [record]);
    let patched = Box::leak(Box::new(
        crate::encode(&Stamp {
            release_build: true,
            ..Stamp::UNSTAMPED
        })
        .unwrap(),
    ));
    let identity = crate::decode(patched, compiled).unwrap();
    assert_eq!(identity.debug_assertions, Some(true));
    assert_eq!(identity.target_arch, "aarch64");
}

#[test]
fn unknown_versions_values_duplicate_fields_and_truncation_never_supply_facts() {
    let good = encode(stamp(false));
    let text = std::str::from_utf8(&good).unwrap().trim_end_matches('\0');
    for text in [
        text.replace("aterm-compile-facts 1", "aterm-compile-facts 2"),
        text.replace("debug_assertions=false", "debug_assertions=unknown"),
        text.replace("target_arch=aarch64", "profile=release"),
        text.replace("profile=release", "profile=unknown"),
        text.replace("compiler_flavor=r", "compiler_flavor=x"),
    ] {
        let mut malformed = [0; LEN];
        malformed[..text.len()].copy_from_slice(text.as_bytes());
        assert_eq!(malformed.len(), LEN);
        assert!(decode(&malformed).is_err());
    }
    assert!(decode(&good[..LEN - 1]).is_err());
    let mut padding = good;
    padding[LEN - 1] = b'x';
    assert!(decode(&padding).is_err());
    assert!(
        macho_records(&mut Cursor::new(good)).is_err(),
        "a free-standing matching string is no executable section"
    );
}

#[test]
fn section_reader_refuses_unbacked_duplicate_truncated_and_contradictory_records() {
    let good = image(&encode(stamp(false)));
    assert_eq!(macho_records(&mut Cursor::new(&good)).unwrap().len(), 1);
    for end in [0, 7, 31, 183, good.len() - 1] {
        assert!(macho_records(&mut Cursor::new(&good[..end])).is_err());
    }
    for (at, word) in [
        (152, 1_u32),
        (168, 1),
        (96, 2),
        (4, 0x0100_0007),
        (20, 1024 * 1024 + 1),
    ] {
        let mut bytes = good.clone();
        bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
        assert!(
            macho_records(&mut Cursor::new(bytes)).is_err(),
            "invalid field at {at}"
        );
    }
    let mut duplicate = vec![0; good.len() + 80];
    duplicate[..184].copy_from_slice(&good[..184]);
    duplicate[184..264].copy_from_slice(&good[104..184]);
    duplicate[336..].copy_from_slice(&good[256..]);
    duplicate[36..40].copy_from_slice(&232_u32.to_le_bytes());
    duplicate[20..24].copy_from_slice(&232_u32.to_le_bytes());
    duplicate[96..100].copy_from_slice(&2_u32.to_le_bytes());
    duplicate[152..156].copy_from_slice(&336_u32.to_le_bytes());
    duplicate[232..236].copy_from_slice(&336_u32.to_le_bytes());
    let len = duplicate.len() as u64;
    duplicate[80..88].copy_from_slice(&len.to_le_bytes());
    // Both section entries name the same record; even duplicate declarations refuse.
    assert!(macho_records(&mut Cursor::new(duplicate)).is_err());
}

#[test]
fn universal_tables_bind_each_record_to_its_architecture_and_refuse_overlap() {
    let arm = image(&encode(stamp(true)));
    let mut intel = image(&encode(Stamp {
        target_arch: "x86_64",
        ..stamp(true)
    }));
    intel[4..8].copy_from_slice(&0x0100_0007_u32.to_le_bytes());
    for stride in [20, 32] {
        let mut fat = vec![0; 8192 + intel.len()];
        fat[..4].copy_from_slice(
            &(if stride == 20 {
                0xcafe_babe_u32
            } else {
                0xcafe_babf
            })
            .to_be_bytes(),
        );
        fat[4..8].copy_from_slice(&2_u32.to_be_bytes());
        for (i, cpu, offset, bytes) in [
            (0, 0x0100_000c_u32, 4096_u64, &arm),
            (1, 0x0100_0007, 8192, &intel),
        ] {
            let at = 8 + i * stride;
            fat[at..at + 4].copy_from_slice(&cpu.to_be_bytes());
            if stride == 20 {
                fat[at + 8..at + 12].copy_from_slice(&(offset as u32).to_be_bytes());
                fat[at + 12..at + 16].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
            } else {
                fat[at + 8..at + 16].copy_from_slice(&offset.to_be_bytes());
                fat[at + 16..at + 24].copy_from_slice(&(bytes.len() as u64).to_be_bytes());
            }
            fat[offset as usize..offset as usize + bytes.len()].copy_from_slice(bytes);
        }
        let records = macho_records(&mut Cursor::new(&fat)).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(decode(&records[0]).unwrap().target_arch, "aarch64");
        assert_eq!(decode(&records[1]).unwrap().target_arch, "x86_64");
        let mut wrong_cpu = fat.clone();
        wrong_cpu[8 + stride..12 + stride].copy_from_slice(&0x0100_000c_u32.to_be_bytes());
        assert!(macho_records(&mut Cursor::new(wrong_cpu)).is_err());
        if stride == 20 {
            fat[16 + stride..20 + stride].copy_from_slice(&4096_u32.to_be_bytes());
        } else {
            fat[16 + stride..24 + stride].copy_from_slice(&4096_u64.to_be_bytes());
        }
        assert!(
            macho_records(&mut Cursor::new(fat)).is_err(),
            "two declared slices cannot reuse the same bytes"
        );
    }
}
