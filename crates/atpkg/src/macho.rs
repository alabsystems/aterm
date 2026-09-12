// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Two Mach-O executables compared MODULO their code signatures — the pure half of
//! `doctor`'s check that the Trust bundle's `rustc` is the compiler `trustc` it stands in
//! for.
//!
//! An ad-hoc signature embeds the file's own name (`rustc-<hash>` beside
//! `trustc-<hash>`), so two copies of one program, each signed under its own name, never
//! match byte for byte, and anything that asks "is this sibling the same compiler?" by
//! comparing the files refuses them. Measured 2026-09-12 on Trust bundles 8571 and
//! 8589: `rustc` and `trustc` are both 336 192 bytes, both `Signature=adhoc`,
//! identifiers `rustc-<hash>` and `trustc-<hash>`, and their first difference is at
//! offset 317 303 (`cmp`'s byte 317 304) — inside the signature blob, which runs from
//! offset 317 296 to the end.
//!
//! What one program under two signatures may differ in is also measured, not assumed:
//! one signature-stripped copy of 8589's `trustc`, signed by `codesign` twice — under
//! trustc's own identifier and under one 3 001 bytes longer — came out 3 008 bytes apart
//! in length, and outside the blob only two fields differed: `LC_CODE_SIGNATURE`'s
//! `datasize` and `__LINKEDIT`'s `filesize`. The header's `sizeofcmds`, the command's
//! `cmdsize` and its `dataoff` were equal. `__LINKEDIT`'s `vmsize` was equal too, but it
//! is `filesize` rounded up to a page, so a blob that pushes the segment across one
//! moves it. Those three size fields and the blob are the whole discount; every other
//! byte must match.
//!
//! Pure Rust over two byte slices: no `codesign` spawn, no I/O. Only a THIN 64-bit
//! little-endian Mach-O is read — every Apple platform the bundles serve. A universal
//! (fat) file, a 32-bit one, an ELF on Linux and anything truncated or malformed is
//! [`Modulo::Unparseable`], and the caller makes no claim about it.

use std::ops::Range;

/// `MH_MAGIC_64`, as the little-endian word a thin 64-bit Mach-O opens with.
const MH_MAGIC_64: u32 = 0xfeed_facf;
/// `sizeof(struct mach_header_64)`.
const HEADER_64: usize = 32;
/// Where `mach_header_64.ncmds` sits.
const NCMDS_AT: usize = 16;
/// Where `mach_header_64.sizeofcmds` sits — the header's total of load-command bytes.
const SIZEOFCMDS_AT: usize = 20;
/// `LC_SEGMENT_64`.
const LC_SEGMENT_64: u32 = 0x19;
/// `LC_CODE_SIGNATURE`.
const LC_CODE_SIGNATURE: u32 = 0x1d;
/// `sizeof(struct segment_command_64)` with no sections.
const SEGMENT_64: usize = 72;
/// `sizeof(struct linkedit_data_command)` — the one size a real `LC_CODE_SIGNATURE` has.
const LINKEDIT_DATA: usize = 16;
/// Where `linkedit_data_command.dataoff` sits, from the command's start.
const DATAOFF_AT: usize = 8;
/// Where `linkedit_data_command.datasize` sits, from the command's start.
const DATASIZE_AT: usize = 12;

/// What comparing two executables modulo their code signatures found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Modulo {
    /// Byte-for-byte the same file.
    Identical,
    /// One program under two signatures: every byte outside the signature blob matches,
    /// except the three fields that record the blob's size — `LC_CODE_SIGNATURE`'s
    /// `datasize` and `__LINKEDIT`'s `vmsize` and `filesize`.
    SameProgram,
    /// A byte no code signature accounts for differs; `offset` is the first one,
    /// counted in the first file. Unless both files carry the real 16-byte signature
    /// command at the same place, no byte is accounted for.
    Different { offset: usize },
    /// Either file is not a thin 64-bit Mach-O this reads. No claim.
    Unparseable,
}

/// The parts of one Mach-O a signature explains.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Signed {
    /// The `LC_CODE_SIGNATURE` command's own bytes, as long as its `cmdsize` says.
    command: Range<usize>,
    /// The signature blob (`dataoff..dataoff + datasize`).
    blob: Range<usize>,
}

/// What [`parse`] reads out of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Parsed {
    signed: Option<Signed>,
    /// Where the `__LINKEDIT` segment command starts, and its `vmsize` and `filesize`
    /// fields — the two that grow with the signature at the segment's end.
    linkedit: Option<(usize, [Range<usize>; 2])>,
}

/// Compare `a` and `b` modulo their code signatures.
pub(crate) fn compare(a: &[u8], b: &[u8]) -> Modulo {
    if a == b {
        return Modulo::Identical;
    }
    let (Some(pa), Some(pb)) = (parse(a), parse(b)) else {
        return Modulo::Unparseable;
    };
    let raw = || Modulo::Different {
        offset: first_difference(a, b, &[]).unwrap_or(0),
    };
    // Without a signature on BOTH sides there is nothing to discount: a signed and an
    // unsigned file already differ in the header's command count.
    let (Some(sa), Some(sb)) = (&pa.signed, &pb.signed) else {
        return raw();
    };
    // Re-signing moves no load command (module docs), so the discount applies only when
    // both files carry the real 16-byte `LC_CODE_SIGNATURE` at the same offset, beside a
    // `__LINKEDIT` command at the same offset; anything else discounts nothing. The
    // first version discounted the whole command, as long as its `cmdsize` said, plus
    // the header's `sizeofcmds` — and a copy of 8589's `trustc` with `cmdsize` raised by
    // 0x1000 over 64 rewritten bytes of `__text` compared as the same program.
    if sa.command != sb.command || sa.command.len() != LINKEDIT_DATA || pa.linkedit != pb.linkedit {
        return raw();
    }
    // Exactly the three size fields, at offsets both files share. `dataoff` is NOT
    // discounted: a signature that starts somewhere else is a different file layout, and
    // the comparison below names that field as the first difference.
    let datasize = sa.command.start + DATASIZE_AT;
    let linkedit = pa
        .linkedit
        .iter()
        .flat_map(|(_, fields)| fields.iter().cloned());
    let skip: Vec<Range<usize>> = std::iter::once(datasize..datasize + 4)
        .chain(linkedit)
        .collect();
    let (head_a, head_b) = (&a[..sa.blob.start], &b[..sb.blob.start]);
    if let Some(offset) = first_difference(head_a, head_b, &skip) {
        return Modulo::Different { offset };
    }
    let (tail_a, tail_b) = (&a[sa.blob.end..], &b[sb.blob.end..]);
    if let Some(j) = first_difference(tail_a, tail_b, &[]) {
        return Modulo::Different {
            offset: sa.blob.end + j,
        };
    }
    Modulo::SameProgram
}

/// The first index where `a` and `b` differ outside `skip`, or where the shorter one ends.
fn first_difference(a: &[u8], b: &[u8], skip: &[Range<usize>]) -> Option<usize> {
    let common = a.len().min(b.len());
    (0..common)
        .find(|&i| a[i] != b[i] && !skip.iter().any(|r| r.contains(&i)))
        .or_else(|| (a.len() != b.len()).then_some(common))
}

/// A little-endian `u32` at `at`, or `None` past the end.
fn u32_at(file: &[u8], at: usize) -> Option<u32> {
    let bytes = file.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// A `u32` field as an offset or length.
fn usize_at(file: &[u8], at: usize) -> Option<usize> {
    usize::try_from(u32_at(file, at)?).ok()
}

/// Read the header and walk the load commands of a thin 64-bit Mach-O. `None` for any
/// other file, and for one whose commands or signature run past its end.
fn parse(file: &[u8]) -> Option<Parsed> {
    if u32_at(file, 0)? != MH_MAGIC_64 {
        return None;
    }
    let ncmds = usize_at(file, NCMDS_AT)?;
    let commands_end = HEADER_64.checked_add(usize_at(file, SIZEOFCMDS_AT)?)?;
    if commands_end > file.len() {
        return None;
    }
    let mut parsed = Parsed {
        signed: None,
        linkedit: None,
    };
    let mut at = HEADER_64;
    for _ in 0..ncmds {
        let cmd = u32_at(file, at)?;
        let size = usize_at(file, at + 4)?;
        let end = at.checked_add(size)?;
        if size < 8 || end > commands_end {
            return None;
        }
        match cmd {
            LC_CODE_SIGNATURE => {
                if size < LINKEDIT_DATA || parsed.signed.is_some() {
                    return None;
                }
                let dataoff = usize_at(file, at + DATAOFF_AT)?;
                let blob_end = dataoff.checked_add(usize_at(file, at + DATASIZE_AT)?)?;
                if dataoff < commands_end || blob_end > file.len() {
                    return None;
                }
                parsed.signed = Some(Signed {
                    command: at..end,
                    blob: dataoff..blob_end,
                });
            }
            LC_SEGMENT_64 if segment_name(file, at)? == b"__LINKEDIT" => {
                if size < SEGMENT_64 || parsed.linkedit.is_some() {
                    return None;
                }
                // segment_command_64: vmsize at +32, filesize at +48, eight bytes each.
                parsed.linkedit = Some((at, [at + 32..at + 40, at + 48..at + 56]));
            }
            _ => {}
        }
        at = end;
    }
    Some(parsed)
}

/// A segment command's `segname`, without its NUL padding.
fn segment_name(file: &[u8], command: usize) -> Option<&[u8]> {
    let name = file.get(command + 8..command + 24)?;
    name.split(|&c| c == 0).next()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Where the synthetic program's code starts, past the header and load commands.
    pub(crate) const CODE_AT: usize = 0x400;
    /// The code: a stand-in for `__TEXT`, the bytes that make two files one program.
    const CODE: &[u8] = b"\x1f\x20\x03\xd5 the same compiler driver, instruction for instruction";
    /// Where `__LINKEDIT` starts.
    const LINKEDIT_AT: usize = 0x4000;
    /// A page, for `__LINKEDIT`'s rounded `vmsize`.
    const PAGE: usize = 0x4000;
    /// Where the synthetic file's `LC_CODE_SIGNATURE` sits: after two segment commands.
    const SIGNATURE_COMMAND_AT: usize = HEADER_64 + 2 * SEGMENT_64;

    fn put32(file: &mut [u8], at: usize, v: u32) {
        file[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn put64(file: &mut [u8], at: usize, v: u64) {
        file[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn get32(file: &[u8], at: usize) -> u32 {
        u32_at(file, at).unwrap()
    }

    fn segment(file: &mut [u8], at: usize, name: &str, fileoff: usize, filesize: usize) {
        put32(file, at, LC_SEGMENT_64);
        put32(file, at + 4, SEGMENT_64 as u32);
        file[at + 8..at + 8 + name.len()].copy_from_slice(name.as_bytes());
        put64(file, at + 24, 0x1_0000_0000 + fileoff as u64);
        put64(
            file,
            at + 32,
            filesize.div_ceil(PAGE).max(1) as u64 * PAGE as u64,
        );
        put64(file, at + 40, fileoff as u64);
        put64(file, at + 48, filesize as u64);
    }

    /// A thin arm64 executable: header, `__TEXT` and `__LINKEDIT` segment commands, an
    /// `LC_CODE_SIGNATURE` when `identifier` is given (16 bytes, as in every real file),
    /// the code, `linkedit_payload` bytes of symbol-table stand-in, and a signature blob
    /// whose length follows its identifier — so a longer identifier moves `datasize`,
    /// `__LINKEDIT`'s `filesize` and (across a page) its `vmsize`, and nothing else in
    /// the header or load commands, exactly as `codesign` re-signing does.
    fn macho(identifier: Option<&str>, linkedit_payload: usize) -> Vec<u8> {
        let blob: Vec<u8> = identifier
            .map(|identifier| {
                let mut blob = b"\xfa\xde\x0c\xc0\x00\x00\x00\x00".to_vec();
                blob.extend_from_slice(identifier.as_bytes());
                blob.push(0);
                blob.resize(blob.len().next_multiple_of(16), 0);
                blob
            })
            .unwrap_or_default();
        let dataoff = (LINKEDIT_AT + linkedit_payload).next_multiple_of(16);
        let end = dataoff + blob.len();
        let mut file = vec![0u8; end];
        let (ncmds, sizeofcmds) = match identifier {
            Some(_) => (3, 2 * SEGMENT_64 + LINKEDIT_DATA),
            None => (2, 2 * SEGMENT_64),
        };
        put32(&mut file, 0, MH_MAGIC_64);
        put32(&mut file, 4, 0x0100_000c); // CPU_TYPE_ARM64
        put32(&mut file, 12, 2); // MH_EXECUTE
        put32(&mut file, NCMDS_AT, ncmds);
        put32(&mut file, SIZEOFCMDS_AT, sizeofcmds as u32);
        put32(&mut file, 24, 0x0020_0085);
        segment(&mut file, HEADER_64, "__TEXT", 0, LINKEDIT_AT);
        segment(
            &mut file,
            HEADER_64 + SEGMENT_64,
            "__LINKEDIT",
            LINKEDIT_AT,
            end - LINKEDIT_AT,
        );
        if identifier.is_some() {
            let at = SIGNATURE_COMMAND_AT;
            put32(&mut file, at, LC_CODE_SIGNATURE);
            put32(&mut file, at + 4, LINKEDIT_DATA as u32);
            put32(&mut file, at + DATAOFF_AT, dataoff as u32);
            put32(&mut file, at + DATASIZE_AT, blob.len() as u32);
        }
        file[CODE_AT..CODE_AT + CODE.len()].copy_from_slice(CODE);
        for (i, byte) in file[LINKEDIT_AT..LINKEDIT_AT + linkedit_payload]
            .iter_mut()
            .enumerate()
        {
            *byte = (i % 251) as u8;
        }
        file[dataoff..].copy_from_slice(&blob);
        file
    }

    /// A synthetic signed program whose ad-hoc signature carries `identifier` — two of
    /// these with different identifiers are one program under two signatures.
    pub(crate) fn signed(identifier: &str) -> Vec<u8> {
        macho(Some(identifier), 0x100)
    }

    #[test]
    fn one_file_twice_is_identical() {
        let a = signed("trustc-5555494429441d12e5e3340fa96ce4becbafab70");
        assert_eq!(compare(&a, &a.clone()), Modulo::Identical);
    }

    /// THE MEASURED SHAPE: same size, first difference inside the signature — an
    /// identifier one character longer, absorbed by the blob's padding.
    #[test]
    fn two_signatures_of_one_program_are_the_same_program() {
        let rustc = signed("rustc-5555494429441d12e5e3340fa96ce4becbafab70");
        let trustc = signed("trustc-5555494429441d12e5e3340fa96ce4becbafab70");
        assert_eq!(
            rustc.len(),
            trustc.len(),
            "the padding absorbs one character"
        );
        assert_ne!(rustc, trustc);
        assert_eq!(compare(&rustc, &trustc), Modulo::SameProgram);
        assert_eq!(compare(&trustc, &rustc), Modulo::SameProgram);
    }

    /// A signature long enough to move every field that records its size — the blob's
    /// `datasize`, `__LINKEDIT`'s `filesize` and, across a page, its `vmsize`, and the
    /// file's length — while the header's `sizeofcmds`, the command's `cmdsize` and its
    /// `dataoff` stay put, as they did under `codesign` (module docs). Still one program.
    #[test]
    fn a_longer_signature_moves_only_the_fields_that_record_it() {
        // 0x3fc0 of payload puts the short signature's end just inside the first page of
        // __LINKEDIT and the long one's past it.
        let rustc = macho(Some("rustc"), 0x3fc0);
        let trustc = macho(
            Some("trustc-5555494429441d12e5e3340fa96ce4becbafab70-and-more"),
            0x3fc0,
        );
        let (pr, pt) = (parse(&rustc).unwrap(), parse(&trustc).unwrap());
        let (lr, lt) = (pr.linkedit.unwrap().1, pt.linkedit.unwrap().1);
        assert_ne!(rustc[lr[0].clone()], trustc[lt[0].clone()], "vmsize moved");
        assert_ne!(
            rustc[lr[1].clone()],
            trustc[lt[1].clone()],
            "filesize moved"
        );
        let datasize = SIGNATURE_COMMAND_AT + DATASIZE_AT;
        assert_ne!(
            get32(&rustc, datasize),
            get32(&trustc, datasize),
            "datasize moved"
        );
        assert_ne!(rustc.len(), trustc.len(), "the file grew");
        for (field, at) in [
            ("sizeofcmds", SIZEOFCMDS_AT),
            ("cmdsize", SIGNATURE_COMMAND_AT + 4),
            ("dataoff", SIGNATURE_COMMAND_AT + DATAOFF_AT),
        ] {
            assert_eq!(get32(&rustc, at), get32(&trustc, at), "{field} stayed put");
        }
        assert_eq!(compare(&rustc, &trustc), Modulo::SameProgram);
        assert_eq!(compare(&trustc, &rustc), Modulo::SameProgram);
    }

    /// One code byte flipped, outside the signature: two programs, and the offset says
    /// where.
    #[test]
    fn a_flipped_code_byte_is_a_different_program() {
        let rustc = signed("rustc-5555494429441d12e5e3340fa96ce4becbafab70");
        let mut trustc = signed("trustc-5555494429441d12e5e3340fa96ce4becbafab70");
        trustc[CODE_AT + 7] ^= 0x01;
        assert_eq!(
            compare(&rustc, &trustc),
            Modulo::Different {
                offset: CODE_AT + 7
            }
        );
    }

    /// The discount is exactly the three size fields: a header field beside them (the
    /// CPU type, `sizeofcmds`), the command's `dataoff`, a symbol-table byte in
    /// `__LINKEDIT`, or a byte after the signature is a different program.
    #[test]
    fn nothing_outside_the_named_fields_is_discounted() {
        let rustc = signed("rustc-5555494429441d12e5e3340fa96ce4becbafab70");
        let base = signed("trustc-5555494429441d12e5e3340fa96ce4becbafab70");
        let mut x86 = base.clone();
        put32(&mut x86, 4, 0x0100_0007); // CPU_TYPE_X86_64
        assert_eq!(compare(&rustc, &x86), Modulo::Different { offset: 4 });
        // A header that claims 8 more bytes of commands than it has — still inside the
        // file, still walkable — is not a re-signing of anything.
        let mut wider = base.clone();
        put32(&mut wider, SIZEOFCMDS_AT, get32(&base, SIZEOFCMDS_AT) + 8);
        assert!(parse(&wider).is_some(), "still parses");
        assert_eq!(
            compare(&rustc, &wider),
            Modulo::Different {
                offset: SIZEOFCMDS_AT
            }
        );
        // The signature moved 16 bytes earlier and grown to the same end: the blob now
        // covers what is __LINKEDIT payload in the other file.
        let mut moved = base.clone();
        let at = SIGNATURE_COMMAND_AT;
        put32(
            &mut moved,
            at + DATAOFF_AT,
            get32(&base, at + DATAOFF_AT) - 16,
        );
        put32(
            &mut moved,
            at + DATASIZE_AT,
            get32(&base, at + DATASIZE_AT) + 16,
        );
        assert!(parse(&moved).is_some(), "still parses");
        assert_eq!(
            compare(&rustc, &moved),
            Modulo::Different {
                offset: at + DATAOFF_AT
            }
        );
        let mut symbols = base.clone();
        symbols[LINKEDIT_AT + 3] ^= 0xff;
        assert_eq!(
            compare(&rustc, &symbols),
            Modulo::Different {
                offset: LINKEDIT_AT + 3
            }
        );
        let mut trailing = base.clone();
        trailing.push(0);
        assert_eq!(
            compare(&rustc, &trailing),
            Modulo::Different {
                offset: rustc.len()
            }
        );
    }

    /// THE REVIEW'S COUNTEREXAMPLE, rebuilt on the synthetic layout: `LC_CODE_SIGNATURE`'s
    /// `cmdsize` raised until the command's range swallows the code, `sizeofcmds` raised
    /// to match so the file still parses, and the code rewritten. The first version
    /// called that the same program, because it discounted the whole command as long as
    /// `cmdsize` said. A command that is not the real 16 bytes discounts nothing — in one
    /// file or in both.
    #[test]
    fn an_inflated_signature_command_cannot_hide_code() {
        let rustc = signed("rustc-5555494429441d12e5e3340fa96ce4becbafab70");
        let inflate = |file: &mut Vec<u8>| {
            let at = SIGNATURE_COMMAND_AT;
            let end = CODE_AT + CODE.len() + 16;
            put32(file, at + 4, (end - at) as u32);
            put32(file, SIZEOFCMDS_AT, (end - HEADER_64) as u32);
            assert!(parse(file).is_some(), "the inflated file still parses");
            assert!(
                parse(file)
                    .unwrap()
                    .signed
                    .unwrap()
                    .command
                    .contains(&CODE_AT),
                "the command's range covers the code"
            );
        };
        let mut evil = signed("trustc-5555494429441d12e5e3340fa96ce4becbafab70");
        inflate(&mut evil);
        for byte in &mut evil[CODE_AT..CODE_AT + CODE.len()] {
            *byte ^= 0xa5;
        }
        assert_eq!(
            compare(&rustc, &evil),
            Modulo::Different {
                offset: SIZEOFCMDS_AT
            }
        );
        assert!(matches!(compare(&evil, &rustc), Modulo::Different { .. }));
        // Both inflated alike, code apart: the commands match each other, but neither is
        // a real signature command, so the code is compared and differs.
        let mut twin = rustc.clone();
        inflate(&mut twin);
        assert_eq!(compare(&twin, &evil), Modulo::Different { offset: CODE_AT });
    }

    /// The first version's own test built this shape — a 24-byte `LC_CODE_SIGNATURE`,
    /// `sizeofcmds` 8 bytes wider — which no re-signing produces and macOS will not run
    /// (measured: a small binary given that command and re-signed by `codesign` was
    /// killed at exec, while the same binary re-signed unmodified ran). In both files
    /// alike it still discounts nothing: the two differ first inside what would have
    /// been the blob, and that is reported, not forgiven.
    #[test]
    fn a_signature_command_that_is_not_16_bytes_discounts_nothing() {
        let widen = |mut file: Vec<u8>| {
            put32(&mut file, SIGNATURE_COMMAND_AT + 4, 24);
            let sizeofcmds = get32(&file, SIZEOFCMDS_AT);
            put32(&mut file, SIZEOFCMDS_AT, sizeofcmds + 8);
            assert!(parse(&file).is_some(), "still parses");
            file
        };
        let rustc = widen(signed("rustc-5555494429441d12e5e3340fa96ce4becbafab70"));
        let trustc = widen(signed("trustc-5555494429441d12e5e3340fa96ce4becbafab70"));
        let dataoff = get32(&rustc, SIGNATURE_COMMAND_AT + DATAOFF_AT) as usize;
        assert_eq!(
            compare(&rustc, &trustc),
            Modulo::Different {
                offset: dataoff + 8
            },
            "the identifiers' first letters, r against t"
        );
    }

    /// Nothing to discount unless both files carry a signature.
    #[test]
    fn a_signed_and_an_unsigned_copy_are_different() {
        let rustc = signed("rustc");
        let unsigned = macho(None, 0x100);
        assert!(matches!(
            compare(&rustc, &unsigned),
            Modulo::Different { .. }
        ));
    }

    /// No claim about a file this does not read: truncated, garbage, an ELF, a
    /// universal file.
    #[test]
    fn a_truncated_or_foreign_file_makes_no_claim() {
        let trustc = signed("trustc-5555494429441d12e5e3340fa96ce4becbafab70");
        // Cut inside the load commands, then inside the signature blob.
        let mid_commands = &trustc[..HEADER_64 + 40];
        assert_eq!(compare(mid_commands, &trustc), Modulo::Unparseable);
        let mid_blob = &trustc[..trustc.len() - 4];
        assert_eq!(compare(&trustc, mid_blob), Modulo::Unparseable);
        assert_eq!(compare(b"", &trustc), Modulo::Unparseable);
        assert_eq!(
            compare(b"garbage, not an executable at all", &trustc),
            Modulo::Unparseable
        );
        let mut elf = b"\x7fELF\x02\x01\x01\x00".to_vec();
        elf.resize(4096, 0);
        let mut elf2 = elf.clone();
        elf2[100] = 1;
        assert_eq!(compare(&elf, &elf2), Modulo::Unparseable);
        let mut fat = trustc.clone();
        fat[..4].copy_from_slice(&[0xca, 0xfe, 0xba, 0xbe]);
        assert_eq!(compare(&fat, &trustc), Modulo::Unparseable);
        // A command whose size runs past the header's total is malformed, not a program.
        let mut overrun = trustc.clone();
        put32(&mut overrun, HEADER_64 + 4, 0x1000);
        assert_eq!(compare(&overrun, &trustc), Modulo::Unparseable);
    }
}
