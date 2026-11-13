// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A sectionless static AArch64 executable, like the pinned Linux gitleaks payload.
fn static_elf() -> Vec<u8> {
    let mut bytes = vec![0; 120];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&183_u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
    bytes[64..68].copy_from_slice(&1_u32.to_le_bytes());
    bytes[96..104].copy_from_slice(&120_u64.to_le_bytes());
    bytes[104..112].copy_from_slice(&120_u64.to_le_bytes());
    bytes
}

#[test]
fn static_elf_needs_no_dynamic_relocation_tool() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("gitleaks");
    std::fs::write(&path, static_elf()).unwrap();
    // A large payload needs only its headers, with no new whole-file size limit.
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(128 * 1024 * 1024)
        .unwrap();
    let calls = std::cell::Cell::new(0);
    let refs = read_refs_with(&path, &|_, _| {
        calls.set(calls.get() + 1);
        Err("patchelf is absent".into())
    });
    assert_eq!(refs, Ok(ObjectRefs::default()));
    assert_eq!(calls.get(), 0);
}

fn with_segment(tag: u32, payload: &[u8]) -> Vec<u8> {
    let mut bytes = static_elf();
    bytes.resize(176, 0);
    bytes[56..58].copy_from_slice(&2_u16.to_le_bytes());
    bytes[120..124].copy_from_slice(&tag.to_le_bytes());
    bytes[128..136].copy_from_slice(&176_u64.to_le_bytes());
    bytes[152..160].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(payload);
    let size = bytes.len() as u64;
    bytes[96..104].copy_from_slice(&size.to_le_bytes());
    bytes[104..112].copy_from_slice(&size.to_le_bytes());
    bytes
}

#[test]
fn either_dynamic_segment_or_interpreter_keeps_the_tool_path() {
    for bytes in [
        with_segment(2, &[0; 16]),
        with_segment(3, b"/lib/ld-linux-aarch64.so.1\0"),
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("dynamic");
        std::fs::write(&path, bytes).unwrap();
        let calls = std::cell::RefCell::new(Vec::new());
        let refs = read_refs_with(&path, &|args, _| {
            calls.borrow_mut().push(args[0].to_string());
            Ok(match args[0] {
                "--print-soname" => "libfixture.so\n",
                "--print-needed" => "libc.so.6\n",
                "--print-rpath" => "$ORIGIN/../lib\n",
                _ => panic!("unexpected dynamic operation"),
            }
            .into())
        })
        .unwrap();
        assert_eq!(refs.id.as_deref(), Some("libfixture.so"));
        assert_eq!(refs.needed, ["libc.so.6"]);
        assert_eq!(refs.rpaths, ["$ORIGIN/../lib"]);
        assert_eq!(calls.borrow().len(), 3);
    }
}

#[test]
fn malformed_static_headers_never_bypass_validation() {
    for fault in [
        "truncated",
        "table-overflow",
        "file-range",
        "memory-range",
        "no-load",
    ] {
        let mut bytes = static_elf();
        match fault {
            "truncated" => bytes.truncate(63),
            "table-overflow" => bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes()),
            "file-range" => bytes[96..104].copy_from_slice(&121_u64.to_le_bytes()),
            "memory-range" => bytes[80..88].copy_from_slice(&u64::MAX.to_le_bytes()),
            "no-load" => bytes[64..68].copy_from_slice(&4_u32.to_le_bytes()),
            _ => unreachable!(),
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join(fault);
        std::fs::write(&path, bytes).unwrap();
        let calls = std::cell::Cell::new(0);
        assert!(
            read_refs_with(&path, &|_, _| {
                calls.set(calls.get() + 1);
                Ok(String::new())
            })
            .is_err(),
            "{fault}"
        );
        assert_eq!(calls.get(), 0, "{fault}");
    }
}
