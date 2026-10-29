// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every thread pays for the image's whole thread-local template. dyld allocates the image's
//! `__thread_data` and `__thread_bss` together, with malloc, the first time a thread touches any
//! thread-local in the image. A large value held in one `thread_local!` therefore costs each of
//! aterm's 45 idle threads its size. Three Settings preview memos held inline made the template
//! 46 KB, 2 MB of an idle instance's heap (docs/measured/idle-memory-2026-10-27.md).
//!
//! The template is read from the `aterm-gui` binary, which is the app's `main_entry` built without
//! the library's test-only thread-locals.

#![cfg(target_os = "macos")]

/// The most a thread's copy of the template may hold. The shipped binary's is 3.6 KB.
const TEMPLATE_BUDGET_BYTES: u64 = 8 << 10;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const LC_SEGMENT_64: u32 = 0x19;
/// `struct mach_header_64`.
const HEADER_LEN: usize = 32;
/// `struct segment_command_64` up to its sections.
const SEGMENT_LEN: usize = 72;
/// `struct section_64`.
const SECTION_LEN: usize = 80;

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let word = bytes
        .get(at..at + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .unwrap_or_else(|| panic!("the image ends inside a load command at {at}"));
    u32::from_le_bytes(word)
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let word = bytes
        .get(at..at + 8)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())
        .unwrap_or_else(|| panic!("the image ends inside a section header at {at}"));
    u64::from_le_bytes(word)
}

/// A 16-byte, NUL-padded Mach-O name.
fn name_at(bytes: &[u8], at: usize) -> &[u8] {
    let raw = bytes.get(at..at + 16).unwrap_or_default();
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    raw.get(..end).unwrap_or_default()
}

/// The sizes of `__DATA,__thread_data` and `__DATA,__thread_bss` in a thin 64-bit Mach-O image.
fn thread_local_sections(image: &[u8]) -> (u64, u64) {
    assert_eq!(u32_at(image, 0), MH_MAGIC_64, "a thin 64-bit Mach-O image");
    let ncmds = u32_at(image, 16) as usize;
    let (mut data, mut bss) = (0, 0);
    let mut at = HEADER_LEN;
    for _ in 0..ncmds {
        let (cmd, cmdsize) = (u32_at(image, at), u32_at(image, at + 4) as usize);
        assert!(cmdsize >= 8, "a load command at {at} names its size");
        if cmd == LC_SEGMENT_64 {
            let nsects = u32_at(image, at + 64) as usize;
            for i in 0..nsects {
                let section = at + SEGMENT_LEN + i * SECTION_LEN;
                if name_at(image, section + 16) != b"__DATA" {
                    continue;
                }
                let size = u64_at(image, section + 40);
                match name_at(image, section) {
                    b"__thread_data" => data += size,
                    b"__thread_bss" => bss += size,
                    _ => {}
                }
            }
        }
        at += cmdsize;
    }
    (data, bss)
}

#[test]
fn every_threads_copy_of_the_thread_local_template_stays_small() {
    let path = env!("CARGO_BIN_EXE_aterm-gui");
    let image = std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let (data, bss) = thread_local_sections(&image);
    assert!(data + bss > 0, "fixture: {path} has thread-locals");
    assert!(
        data + bss <= TEMPLATE_BUDGET_BYTES,
        "a thread's copy of {path}'s thread-locals is {} bytes (__thread_data {data}, \
         __thread_bss {bss}), over {TEMPLATE_BUDGET_BYTES}: a large value in a thread_local! \
         belongs behind a Box",
        data + bss
    );
}
