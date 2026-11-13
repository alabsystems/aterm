// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const LEN: usize = 2 * PAGE;

#[test]
fn a_region_maps_back_read_only_from_its_custody_descriptor() {
    // SAFETY: custody stays local, never reopened for writing or resizing; every write uses `region` and finishes before either read-only mapping.
    let (region, custody) = unsafe { Region::create(LEN) }.expect("create");
    assert!(region.is_writable());
    assert_eq!(region.len(), LEN);
    region.write(100, b"written by the window").expect("write");
    region
        .store_u64_release(8, 0x0102_0304_0506_0708)
        .expect("store");
    assert_eq!(is_read_only(custody.as_fd()).ok(), Some(true));
    assert_eq!(fd_size(custody.as_fd()).ok(), Some(LEN as u64));
    // SAFETY: `F_GETFD` reads descriptor flags.
    let fd_flags = unsafe { libc::fcntl(custody.as_raw_fd(), libc::F_GETFD) };
    assert_eq!(
        fd_flags & libc::FD_CLOEXEC,
        libc::FD_CLOEXEC,
        "close-on-exec"
    );

    // SAFETY: all writes above are complete and the writer stays inert; the read-only custody keeps the object stable until both reader mappings drop.
    let reader = unsafe { Region::map_readonly(custody.as_fd(), LEN) }.expect("map read-only");
    assert!(!reader.is_writable());
    let mut back = [0u8; 21];
    reader.read_into(100, &mut back).expect("read");
    assert_eq!(&back, b"written by the window");
    assert_eq!(reader.load_u64_acquire(8), Ok(0x0102_0304_0506_0708));
    let mut le = [0u8; 8];
    reader.read_into(8, &mut le).expect("read");
    assert_eq!(le, 0x0102_0304_0506_0708_u64.to_le_bytes(), "little-endian");
    assert_eq!(reader.write(0, b"x"), Err(AccessError::ReadOnly));
    assert_eq!(reader.store_u64_release(0, 1), Err(AccessError::ReadOnly));

    drop(region);
    assert_eq!(reader.load_u64_acquire(8), Ok(0x0102_0304_0506_0708));
    drop(reader);
    // SAFETY: writer and prior reader were dropped; only read-only custody remains.
    let again = unsafe { Region::map_readonly(custody.as_fd(), LEN) }.expect("map again");
    assert_eq!(again.load_u64_acquire(8), Ok(0x0102_0304_0506_0708));
}

#[test]
fn map_readonly_refuses_a_writable_descriptor_and_a_wrong_size() {
    // SAFETY: this private writable descriptor is only inspected and refused below; neither it nor an alias writes, resizes or maps the object while `_region` lives.
    let (_region, writable) =
        unsafe { testing::create_with_writable_custody(LEN) }.expect("create");
    assert_eq!(is_read_only(writable.as_fd()).ok(), Some(false));
    // SAFETY: the object is unchanged during this refused call.
    let e = unsafe { Region::map_readonly(writable.as_fd(), LEN) }.expect_err("writable");
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");

    // SAFETY: this private custody is only used for the refusal checks below; it and its aliases never modify or resize the object while `_region` lives.
    let (_region, custody) = unsafe { Region::create(LEN) }.expect("create");
    // SAFETY: private object is unchanged during this refused call.
    let e = unsafe { Region::map_readonly(custody.as_fd(), 2 * LEN) }.expect_err("wrong size");
    assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
    // SAFETY: private object is unchanged during this refused call.
    let e =
        unsafe { Region::map_readonly(custody.as_fd(), LEN + 1) }.expect_err("not a region length");
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
    // SAFETY: private object is unchanged during this refused call.
    let e = unsafe { Region::map_readonly(custody.as_fd(), MAX_LEN + PAGE) }.expect_err("too long");
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");

    for len in [0, PAGE - 1, PAGE + 1, MAX_LEN + PAGE] {
        // SAFETY: invalid lengths return an error without mapping.
        let e = unsafe { Region::create(len) }.expect_err("not a region length");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{len}: {e}");
    }
}

#[test]
fn accesses_are_bounds_and_alignment_checked() {
    // SAFETY: custody stays local and unused; only `region` accesses the private object, and nothing changes its size.
    let (region, _custody) = unsafe { Region::create(PAGE) }.expect("create");
    assert_eq!(
        region.load_u64_acquire(12),
        Err(AccessError::Misaligned { at: 12 })
    );
    assert_eq!(
        region.load_u64_acquire(PAGE),
        Err(AccessError::OutOfBounds { at: PAGE, len: 8 })
    );
    assert_eq!(
        region.write(PAGE - 4, &[0; 5]),
        Err(AccessError::OutOfBounds {
            at: PAGE - 4,
            len: 5
        })
    );
    assert_eq!(
        region.read_into(usize::MAX, &mut [0; 2]),
        Err(AccessError::OutOfBounds {
            at: usize::MAX,
            len: 2
        })
    );
    region.write(PAGE - 5, &[7; 5]).expect("the last bytes");
    let mut last = [0u8; 5];
    region.read_into(PAGE - 5, &mut last).expect("read");
    assert_eq!(last, [7; 5]);
}

#[test]
fn copies_across_word_edges_are_plain_memory() {
    // SAFETY: custody is never reopened or used to change the object; every write goes through `region` and finishes before the read-only mapping.
    let (region, custody) = unsafe { Region::create(PAGE) }.expect("create");
    let mut model = vec![0u8; PAGE];
    let mut seed = 0x9e37_79b9_u32;
    for at in 0..3 * WORD {
        for len in 0..3 * WORD {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    seed.to_be_bytes()[0]
                })
                .collect();
            region.write(at, &bytes).expect("write");
            model[at..at + len].copy_from_slice(&bytes);
        }
    }
    // SAFETY: every write above is finished; the writer is only read from here on, and no other descriptor or mapping can modify the private object.
    let reader = unsafe { Region::map_readonly(custody.as_fd(), PAGE) }.expect("map read-only");
    for at in 0..4 * WORD {
        for len in 0..4 * WORD {
            let mut back = vec![0u8; len];
            reader.read_into(at, &mut back).expect("read");
            assert_eq!(back, model[at..at + len], "at={at} len={len}");
            region.read_into(at, &mut back).expect("read");
            assert_eq!(
                back,
                model[at..at + len],
                "the writer's own, at={at} len={len}"
            );
        }
    }
    for at in (0..4 * WORD).step_by(WORD) {
        let word = <[u8; WORD]>::try_from(&model[at..at + WORD]).expect("a word");
        assert_eq!(reader.load_u64_acquire(at), Ok(u64::from_le_bytes(word)));
    }
}

#[test]
fn a_region_name_is_31_bytes_of_base32() {
    for _ in 0..16 {
        let name = fresh_name().expect("name");
        let bytes = name.as_bytes();
        assert_eq!(bytes.len(), 31, "{name:?}");
        assert!(bytes.starts_with(NAME_HEAD.as_bytes()));
        assert!(
            bytes[NAME_HEAD.len()..]
                .iter()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(b)),
            "{name:?}"
        );
    }
}
