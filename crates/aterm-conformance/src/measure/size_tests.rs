// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn thin(cputype: u32) -> Vec<u8> {
    let mut h = MH_MAGIC_64.to_le_bytes().to_vec();
    h.extend(cputype.to_le_bytes());
    h.resize(64, 0);
    h
}

fn fat(entries: &[(u32, u32, u32)]) -> Vec<u8> {
    let mut h = FAT_MAGIC.to_be_bytes().to_vec();
    h.extend(u32::try_from(entries.len()).unwrap().to_be_bytes());
    for &(cputype, offset, size) in entries {
        for word in [cputype, 0, offset, size, 14] {
            h.extend(word.to_be_bytes());
        }
    }
    h
}

#[test]
fn a_thin_image_is_one_slice_the_whole_file() {
    assert_eq!(
        slices(&thin(CPU_TYPE_ARM64), 80_000_000),
        Ok(vec![Slice {
            cputype: CPU_TYPE_ARM64,
            bytes: 80_000_000
        }])
    );
}

#[test]
fn a_universal_image_is_judged_by_this_hosts_slice_alone() {
    let image = fat(&[
        (CPU_TYPE_X86_64, 16_384, 90_000_000),
        (CPU_TYPE_ARM64, 90_013_696, 80_000_000),
    ]);
    let got = slices(&image, 170_013_696).expect("a universal image");
    assert_eq!(
        got,
        vec![
            Slice {
                cputype: CPU_TYPE_X86_64,
                bytes: 90_000_000
            },
            Slice {
                cputype: CPU_TYPE_ARM64,
                bytes: 80_000_000
            }
        ]
    );
    let row = SizeRow {
        max_bytes: 80_000_000,
    };
    assert_eq!(
        judge(&row, &got, CPU_TYPE_ARM64),
        ShapeVerdict::Pass,
        "the arm64 slice at its ceiling, the larger x86_64 slice beside it unjudged"
    );
    assert_eq!(
        judge(&row, &got, CPU_TYPE_X86_64),
        ShapeVerdict::Fail,
        "proven red: a slice one byte or more past the ceiling"
    );
    assert!(
        slices(&image, 170_000_000).is_err(),
        "a slice past the file's end is no measurement"
    );
}

/// The red: an executable one byte past its ceiling fails, and an image with no slice of
/// this host's type, or that is not Mach-O, is never a pass.
#[test]
fn a_slice_past_its_ceiling_or_missing_fails_and_a_foreign_file_is_refused() {
    let row = SizeRow { max_bytes: 100 };
    let one = |bytes| {
        vec![Slice {
            cputype: CPU_TYPE_ARM64,
            bytes,
        }]
    };
    assert_eq!(judge(&row, &one(100), CPU_TYPE_ARM64), ShapeVerdict::Pass);
    assert_eq!(judge(&row, &one(101), CPU_TYPE_ARM64), ShapeVerdict::Fail);
    assert_eq!(judge(&row, &one(1), CPU_TYPE_X86_64), ShapeVerdict::Fail);
    assert_eq!(judge(&row, &[], CPU_TYPE_ARM64), ShapeVerdict::Fail);
    assert!(slices(b"\x7fELF\x02\x01\x01\0", 8).is_err());
    assert!(slices(b"\xca\xfe", 2).is_err(), "a header cut short");
}

#[cfg(target_os = "macos")]
#[test]
fn this_process_reads_as_its_own_hosts_slice() {
    let exe = std::env::current_exe().expect("the test binary");
    let report = measure(
        &exe,
        &SizeRow {
            max_bytes: u64::MAX,
        },
    )
    .expect("a Mach-O test binary");
    assert_eq!(Some(report.cputype), host_cputype());
    assert!(
        report.bytes > 0 && report.bytes <= report.file_bytes,
        "{report}"
    );
    assert_eq!(report.verdict, ShapeVerdict::Pass);
    let tight = measure(&exe, &SizeRow { max_bytes: 1 }).expect("measured");
    assert_eq!(tight.verdict, ShapeVerdict::Fail, "{tight}");
}
