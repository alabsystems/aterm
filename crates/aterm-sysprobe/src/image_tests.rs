// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// One load command: its type, its size and its payload, padded to `size`.
fn command(cmd: u32, size: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&cmd.to_ne_bytes());
    out.extend_from_slice(&size.to_ne_bytes());
    out.extend_from_slice(payload);
    out.resize(size as usize, 0);
    out
}

/// The UUID is the one the `LC_UUID` command carries, reached by stepping over each command
/// before it by its own size, and the walk reads no more than the `ncmds` commands the header
/// names: a UUID past them is not this image's.
///
/// NEGATIVE CONTROL (2026-11-15): with `lc_uuid` walking on past its `ncmds` commands until a
/// header does not read, the second assertion failed: `the count bounds the walk`, `left:
/// Some([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16])`, `right: None`.
#[test]
fn the_uuid_is_found_after_the_commands_before_it() {
    let uuid: [u8; 16] = std::array::from_fn(|i| i as u8 + 1);
    let mut commands = command(0x19, 72, &[7; 64]);
    commands.extend(command(LC_UUID, 24, &uuid));
    commands.extend(command(0x32, 32, &[9; 24]));
    assert_eq!(lc_uuid(&commands, 3), Some(uuid));
    assert_eq!(lc_uuid(&commands, 1), None, "the count bounds the walk");
}

/// A command whose size is short of its own 8-byte header, or runs past the commands, stops the
/// walk, and a UUID cut short is not read: a malformed command is never stepped over into bytes
/// that only look like the next one. The short command says it is 4 bytes long; stepped over,
/// the walk would read its size field as the next command's type and the 8 after it as that
/// command's size, which lands on a whole `LC_UUID` command. The long one is an `LC_UUID` whose
/// UUID bytes are there but whose size runs past them.
///
/// NEGATIVE CONTROLS (2026-11-15), each run alone: with `lc_uuid`'s `size < 8` guard removed,
/// `a size under 8 cannot be stepped over` failed with `left: Some([5, 5, ..])`, `right: None`
/// (the fixture this replaced said its short command was 0 bytes long, which never moves the
/// walk on, and passed with the guard removed); with `lc_uuid` reading a command to the end of
/// the commands instead of to its size, `a size past the commands is not followed` failed with
/// `left: Some([5, 5, ..])`, `right: None`.
#[test]
fn a_command_short_of_its_header_or_past_the_end_stops_the_walk() {
    let uuid = [5u8; 16];
    let mut short = Vec::new();
    short.extend_from_slice(&0x19u32.to_ne_bytes());
    short.extend_from_slice(&4u32.to_ne_bytes());
    short.extend_from_slice(&8u32.to_ne_bytes());
    short.extend(command(LC_UUID, 24, &uuid));
    assert_eq!(
        lc_uuid(&short, 3),
        None,
        "a size under 8 cannot be stepped over"
    );
    let mut long = command(LC_UUID, 24, &uuid);
    long[4..8].copy_from_slice(&4096u32.to_ne_bytes());
    assert_eq!(
        lc_uuid(&long, 1),
        None,
        "a size past the commands is not followed"
    );
    let cut = command(LC_UUID, 24, &uuid);
    assert_eq!(lc_uuid(&cut[..20], 1), None, "a UUID cut short is not read");
}

/// One ELF note: its name, type and descriptor, each padded to four bytes.
fn note(name: &[u8], kind: u32, desc: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(name.len() as u32).to_ne_bytes());
    out.extend_from_slice(&(desc.len() as u32).to_ne_bytes());
    out.extend_from_slice(&kind.to_ne_bytes());
    for part in [name, desc] {
        out.extend_from_slice(part);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    out
}

/// The build id is the descriptor of the first note that is both named `GNU` and of type
/// `NT_GNU_BUILD_ID`: a GNU note of another type and another vendor's note of that type (the
/// fixture's `Go` one; a note's type means something only under its name) are stepped over,
/// each by its padded sizes, and a descriptor cut short is not read.
///
/// NEGATIVE CONTROL (2026-11-15): with `gnu_build_id` matching a note by its type alone, the
/// `Go` note's descriptor was read: `left: Some([110, 111, 116, 45, 103, 110, 117])` (`not-gnu`),
/// `right: Some([171, 205, 239, 1, 35])`.
#[test]
fn the_build_id_is_the_gnu_note_of_its_type() {
    let id = [0xab, 0xcd, 0xef, 0x01, 0x23];
    let mut notes = note(b"GNU\0", 1, &[0; 16]);
    notes.extend(note(b"Go\0", NT_GNU_BUILD_ID, b"not-gnu"));
    notes.extend(note(b"GNU\0", NT_GNU_BUILD_ID, &id));
    assert_eq!(gnu_build_id(&notes), Some(&id[..]));
    let cut = notes.len() - 4;
    assert_eq!(
        gnu_build_id(&notes[..cut]),
        None,
        "a descriptor cut short is not read"
    );
    assert_eq!(gnu_build_id(&note(b"GNU\0", 1, &id)), None);
}

/// A PE32+ image as the loader maps it, with one debug directory entry of `kind` whose data is
/// `record`.
fn image(kind: u32, record: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 0x400];
    out[..2].copy_from_slice(b"MZ");
    let nt = 0x80usize;
    out[0x3c..0x40].copy_from_slice(&(nt as u32).to_le_bytes());
    out[nt..nt + 4].copy_from_slice(b"PE\0\0");
    let optional = nt + 24;
    out[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
    out[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes());
    let (debug, data) = (0x300usize, 0x340usize);
    let directory = optional + 112 + 6 * 8;
    out[directory..directory + 4].copy_from_slice(&(debug as u32).to_le_bytes());
    out[directory + 4..directory + 8].copy_from_slice(&28u32.to_le_bytes());
    out[debug + 12..debug + 16].copy_from_slice(&kind.to_le_bytes());
    out[debug + 16..debug + 20].copy_from_slice(&(record.len() as u32).to_le_bytes());
    out[debug + 20..debug + 24].copy_from_slice(&(data as u32).to_le_bytes());
    out[data..data + record.len()].copy_from_slice(record);
    out
}

/// The identity is the 20 bytes (GUID, then age) after the `RSDS` magic of the debug directory's
/// CodeView entry of a PE32+ image: an entry of another type, a CodeView record of another
/// format (`NB10`), a 32-bit optional header and an image cut short before the record each
/// answer `None`.
///
/// NEGATIVE CONTROL (2026-11-15): with `codeview` not checking the `RSDS` magic, `a record of
/// another format` failed: the `NB10` record's bytes were read, `left: Some([64, 65, .., 83])`,
/// `right: None`.
#[test]
fn the_codeview_record_is_its_guid_and_age() {
    let mut record = b"RSDS".to_vec();
    let identity: [u8; 20] = std::array::from_fn(|i| 0x40 + i as u8);
    record.extend_from_slice(&identity);
    record.extend_from_slice(b"aterm.pdb\0");
    assert_eq!(codeview(&image(DEBUG_CODEVIEW, &record)), Some(identity));
    assert_eq!(
        codeview(&image(1, &record)),
        None,
        "a debug entry of another kind"
    );
    let mut other = record.clone();
    other[..4].copy_from_slice(b"NB10");
    assert_eq!(
        codeview(&image(DEBUG_CODEVIEW, &other)),
        None,
        "a record of another format"
    );
    let mut pe32 = image(DEBUG_CODEVIEW, &record);
    pe32[0x80 + 24..0x80 + 26].copy_from_slice(&0x10bu16.to_le_bytes());
    assert_eq!(codeview(&pe32), None, "a 32-bit optional header");
    assert_eq!(
        codeview(&image(DEBUG_CODEVIEW, &record)[..0x350]),
        None,
        "an image cut short"
    );
}

/// The UUID read from this test binary's mapping is the one its file on disk carries: the
/// running image's own, read without the file. NEGATIVE CONTROL: `mapped` reading the bytes at
/// `base + 16` as the command bytes (the header's own fields) found no UUID there, and this test
/// failed with `left: None`.
#[cfg(target_os = "macos")]
#[test]
fn the_mapped_uuid_is_the_one_this_binary_was_linked_with() {
    let exe = std::env::current_exe().expect("this test binary's path");
    let file = std::fs::read(&exe).expect("this test binary's bytes");
    assert_eq!(
        u32::from_ne_bytes(file[..4].try_into().unwrap()),
        0xfeed_facf,
        "a test binary is a thin 64-bit Mach-O"
    );
    let ncmds = u32::from_ne_bytes(file[16..20].try_into().unwrap());
    let size = u32::from_ne_bytes(file[20..24].try_into().unwrap()) as usize;
    let on_disk = lc_uuid(&file[32..32 + size], ncmds).expect("the linker stamps an LC_UUID");
    let mapped = mapped().expect("dladdr knows this image");
    assert_eq!(mapped.uuid, Some(on_disk));
    assert_ne!(mapped.base, 0);
    assert_eq!(identity(), Some(&on_disk[..]));
}
