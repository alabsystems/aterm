// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Cursor;

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}
fn header(machine: u16) -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    put16(&mut bytes, 16, 2);
    put16(&mut bytes, 18, machine);
    put32(&mut bytes, 20, 1);
    put16(&mut bytes, 52, 64);
    put16(&mut bytes, 54, 56);
    bytes
}
fn dynamic(version: &str) -> Vec<u8> {
    let mut bytes = header(62);
    bytes.resize(1024, 0);
    put16(&mut bytes, 16, 3);
    put64(&mut bytes, 32, 64);
    put16(&mut bytes, 56, 2);
    put32(&mut bytes, 64, 1); // PT_LOAD covers all runtime metadata.
    put64(&mut bytes, 80, 0x40_0000);
    put64(&mut bytes, 96, 1024);
    put64(&mut bytes, 104, 1024);
    put32(&mut bytes, 120, 2); // PT_DYNAMIC, with no section table at all.
    put64(&mut bytes, 128, 256);
    put64(&mut bytes, 136, 0x40_0100);
    put64(&mut bytes, 152, 80);
    let strings = format!("\0libc.so.6\0{version}\0").into_bytes();
    for (index, (tag, value)) in [
        (5, 0x40_0200),
        (10, strings.len() as u64),
        (0x6fff_fffe, 0x40_0280),
        (0x6fff_ffff, 1),
        (0, 0),
    ]
    .into_iter()
    .enumerate()
    {
        put64(&mut bytes, 256 + index * 16, tag);
        put64(&mut bytes, 264 + index * 16, value);
    }
    bytes[512..512 + strings.len()].copy_from_slice(&strings);
    put16(&mut bytes, 640, 1);
    put16(&mut bytes, 642, 1);
    put32(&mut bytes, 644, 1);
    put32(&mut bytes, 648, 16);
    put16(&mut bytes, 662, 2);
    put32(&mut bytes, 664, 11);
    bytes
}
fn parsed(bytes: &[u8]) -> Result<Option<Requirements>, Unadmitted> {
    inspect(&mut Cursor::new(bytes), bytes.len() as u64)
}
fn host(version: &str) -> Host {
    Host {
        machine: 62,
        glibc: Version::parse(version).unwrap(),
    }
}

#[test]
fn dynamic_needs_without_sections_refuse_an_older_host_and_accept_a_newer_one() {
    let mut bytes = dynamic("GLIBC_2.39");
    bytes[900..910].copy_from_slice(b"GLIBC_99.0");
    let requirements = parsed(&bytes).unwrap().unwrap();
    assert_eq!(requirements.glibc, Version::parse("2.39"));
    assert!(
        requirements
            .admit(host("2.35"))
            .unwrap_err()
            .contains("2.39")
    );
    assert_eq!(requirements.admit(host("2.39")), Ok(()));
    assert_eq!(requirements.admit(host("2.40")), Ok(()));
    assert!(
        requirements
            .admit(Host {
                machine: 183,
                ..host("2.40")
            })
            .is_err()
    );
    assert!(Version::parse("2.9") < Version::parse("2.35"));
}

#[test]
fn ordinary_data_static_programs_and_other_libraries_do_not_invent_libc_requirements() {
    assert_eq!(parsed(b"README contains GLIBC_99.0"), Ok(None));
    let static_elf = parsed(&header(62)).unwrap().unwrap();
    assert_eq!(static_elf.glibc, None);
    assert_eq!(static_elf.admit(host("2.17")), Ok(()));
    let mut with_null = dynamic("GLIBC_2.17");
    put16(&mut with_null, 56, 3);
    with_null[180..232].fill(0xff);
    assert_eq!(
        parsed(&with_null).unwrap().unwrap().admit(host("2.17")),
        Ok(())
    );
    assert_eq!(
        parsed(&dynamic("OPENSSL_3.0.0")).unwrap().unwrap().glibc,
        None
    );
    let mut object = header(183);
    put16(&mut object, 16, 1);
    assert_eq!(parsed(&object), Ok(None));
    let mut object32 = vec![0; 52];
    object32[..7].copy_from_slice(b"\x7fELF\x01\x02\x01");
    object32[16..18].copy_from_slice(&1_u16.to_be_bytes());
    object32[20..24].copy_from_slice(&1_u32.to_be_bytes());
    object32[40..42].copy_from_slice(&52_u16.to_be_bytes());
    assert_eq!(parsed(&object32), Ok(None));
    object32[16..18].copy_from_slice(&2_u16.to_be_bytes());
    assert!(
        parsed(&object32).is_err(),
        "foreign runtime ELF stays refused"
    );
    let mut optional = dynamic("GLIBC_2.39");
    put16(&mut optional, 660, 2);
    assert_eq!(
        parsed(&optional).unwrap().unwrap().admit(host("2.17")),
        Ok(())
    );
}

#[test]
fn incomplete_ambiguous_or_unrecognized_runtime_metadata_never_means_compatible() {
    let good = dynamic("GLIBC_2.39");
    for cut in [4, 63, 120, 256, 645, 1023] {
        assert!(parsed(&good[..cut]).is_err(), "truncated at {cut}");
    }
    let mut variants = Vec::new();
    let mut duplicate = good.clone();
    put64(&mut duplicate, 288, 5);
    variants.push(duplicate);
    let mut early = good.clone();
    put16(&mut early, 642, 2);
    variants.push(early);
    let mut past = good.clone();
    put32(&mut past, 664, 65_535);
    variants.push(past);
    let mut incomplete = good.clone();
    put64(&mut incomplete, 304, 0);
    variants.push(incomplete);
    let mut overlap = good.clone();
    overlap.resize(2048, 0);
    put16(&mut overlap, 56, 3);
    let load = overlap[64..120].to_vec();
    overlap[176..232].copy_from_slice(&load);
    put64(&mut overlap, 184, 1024);
    variants.push(overlap);
    let mut misleading_dynamic = good.clone();
    put64(&mut misleading_dynamic, 136, 0x40_0300);
    variants.push(misleading_dynamic);
    variants.push(dynamic("GLIBC_PRIVATE"));
    variants.push(dynamic("GLIBC_2.39.invalid"));
    variants.push(dynamic("GLIBC_02.39"));
    variants.push(dynamic("GLIBC_2.39.0"));
    for bytes in variants {
        assert!(parsed(&bytes).is_err());
    }
}

#[test]
fn even_a_partial_load_overlap_must_preserve_metadata_byte_authority() {
    let mut matching = dynamic("GLIBC_2.39");
    matching.resize(2048, 0);
    put16(&mut matching, 56, 3);
    put32(&mut matching, 176, 1);
    put64(&mut matching, 184, 525);
    put64(&mut matching, 192, 0x40_020d);
    put64(&mut matching, 208, 2);
    put64(&mut matching, 216, 2);
    assert_eq!(
        parsed(&matching).unwrap().unwrap().glibc,
        Version::parse("2.39")
    );
    let mut conflicting = matching.clone();
    put64(&mut conflicting, 184, 1024);
    assert!(
        parsed(&conflicting).is_err(),
        "partial different-file alias"
    );
    let mut zero_fill = matching;
    put64(&mut zero_fill, 208, 0);
    assert!(parsed(&zero_fill).is_err(), "partial zero-filled alias");
}

#[test]
fn shared_libraries_without_execute_bits_are_admitted_with_the_whole_tree() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    std::fs::create_dir(scratch.path().join("lib")).unwrap();
    let library = scratch.path().join("lib/runtime.so");
    std::fs::write(&library, dynamic("GLIBC_2.39")).unwrap();
    assert!(admit_tree_on(scratch.path(), host("2.35")).is_err());
    assert_eq!(admit_tree_on(scratch.path(), host("2.39")), Ok(()));
    assert_eq!(inspect_tree(scratch.path(), 62).unwrap().objects, 1);
    let mut file = std::fs::File::open(&library).unwrap();
    file.seek(SeekFrom::Start(5)).unwrap();
    assert!(admit_open_file(&file, host("2.35")).is_err());
    assert_eq!(admit_open_file(&file, host("2.39")), Ok(()));
    assert_eq!(
        file.stream_position().unwrap(),
        5,
        "inspection preserves the retained descriptor's cursor"
    );
}

/// NEGATIVE CONTROL: stop `walk` or `inspect_files` at the first entry it cannot judge and
/// the refusals behind it read as no verdict.
#[cfg(unix)]
#[test]
fn an_entry_it_cannot_read_is_no_verdict_and_never_hides_a_refusal() {
    use std::os::unix::fs::PermissionsExt as _;
    let unjudged = |judged: Result<(), Unadmitted>, what: &str| {
        assert!(
            matches!(&judged, Err(Unadmitted::CannotJudge(why)) if why.contains(what)),
            "{judged:?}"
        );
    };
    let refused = |judged: Result<(), Unadmitted>| {
        assert!(matches!(judged, Err(Unadmitted::Refused(_))), "{judged:?}");
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    std::fs::create_dir(root.join("lib")).unwrap();
    std::fs::write(root.join("lib/runtime.so"), dynamic("GLIBC_2.39")).unwrap();
    let fifo = root.join("a-pipe");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `c_fifo` is a valid NUL-terminated path in our scratch directory.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    unjudged(admit_tree_on(root, host("2.39")), "special file");
    refused(admit_tree_on(root, host("2.35")));
    refused(admit_tree_on(
        root,
        Host {
            machine: 183,
            ..host("2.39")
        },
    ));
    assert!(!inspect_tree(root, 62).unwrap_err().is_refusal());
    std::fs::remove_file(&fifo).unwrap();
    assert_eq!(admit_tree_on(root, host("2.39")), Ok(()));
    // SAFETY: geteuid reads this process's effective uid and has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return; // root reads a mode-0 file and folder alike.
    }
    let sealed = root.join("lib/a-sealed.so");
    std::fs::write(&sealed, dynamic("GLIBC_2.17")).unwrap();
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
    unjudged(admit_tree_on(root, host("2.39")), "a-sealed.so");
    refused(admit_tree_on(root, host("2.35")));
    std::fs::remove_file(&sealed).unwrap();
    let shut = root.join("a-shut");
    std::fs::create_dir(&shut).unwrap();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
    unjudged(admit_tree_on(root, host("2.39")), "a-shut");
    refused(admit_tree_on(root, host("2.35")));
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(admit_tree_on(root, host("2.39")), Ok(()));
}

/// A root that is a link or no directory is a refusal, a verdict on what the walk read, and
/// a tree past the entry bound is not judged, the inspector's own limit and no verdict (a
/// marked build's completion stands on its marker there, and is never swept for it); no
/// recalled admission answers for either. CONTROL: the same tree at its own folder, within
/// the bound, is admitted, settled and recalled. NEGATIVE CONTROL: class a root
/// [`Unadmitted::CannotJudge`] and its `refused` fails, or the bound a refusal and its
/// `unjudged` does.
#[cfg(unix)]
#[test]
fn a_root_that_is_no_folder_is_refused_and_a_tree_past_the_bound_is_not_judged() {
    let refused = |judged: Result<(), Unadmitted>, what: &str| {
        assert!(
            matches!(&judged, Err(Unadmitted::Refused(why)) if why.contains(what)),
            "{judged:?}"
        );
    };
    let unjudged = |judged: Result<(), Unadmitted>, what: &str| {
        assert!(
            matches!(&judged, Err(Unadmitted::CannotJudge(why)) if why.contains(what)),
            "{judged:?}"
        );
    };
    // A clock long past every status the walk records: each read has settled.
    let settled = Limits {
        now: || Some(i128::MAX / 2),
        ..LIMITS
    };
    let recalled = |root: &Path| {
        admissions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|cached| cached.root == root)
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let build = scratch.path().join("build");
    std::fs::create_dir_all(build.join("lib")).unwrap();
    std::fs::write(build.join("lib/runtime.so"), dynamic("GLIBC_2.17")).unwrap();
    assert_eq!(admit_tree_within(&build, host("2.39"), &settled), Ok(()));
    assert!(recalled(&build), "a settled admission is recalled");
    let aside = scratch.path().join("aside");
    std::fs::rename(&build, &aside).unwrap();
    std::os::unix::fs::symlink(&aside, &build).unwrap();
    refused(admit_tree_within(&build, host("2.39"), &settled), "symlink");
    refused(admit_tree_on(&build, host("2.39")), "symlink");
    assert!(inspect_tree(&build, 62).unwrap_err().is_refusal());
    assert_eq!(admit_tree_on(&aside, host("2.39")), Ok(()));
    std::fs::remove_file(&build).unwrap();
    std::fs::write(&build, dynamic("GLIBC_2.17")).unwrap();
    refused(admit_tree_on(&build, host("2.39")), "no directory");
    assert!(inspect_tree(&build, 62).unwrap_err().is_refusal());
    std::fs::remove_file(&build).unwrap();
    let c_fifo = std::ffi::CString::new(build.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `c_fifo` is a valid NUL-terminated path in our scratch directory.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    refused(admit_tree_on(&build, host("2.39")), "no directory");
    // `aside`, `lib` and `lib/runtime.so`: three entries.
    assert_eq!(admit_tree_within(&aside, host("2.39"), &settled), Ok(()));
    assert!(recalled(&aside));
    let bounded = Limits {
        entries: 3,
        ..settled
    };
    assert_eq!(admit_tree_within(&aside, host("2.39"), &bounded), Ok(()));
    let bounded = Limits {
        entries: 2,
        ..settled
    };
    unjudged(
        admit_tree_within(&aside, host("2.39"), &bounded),
        "more than 2 entries",
    );
}
