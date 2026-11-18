//! Universal system commands retain every load-command population. These are
//! hostile byte/table controls, never a native process or loaded-image claim.

use super::*;

fn image(cpu: u32, subtype: u32, library: &str) -> Vec<u8> {
    fn command(kind: u32, text: &str, fixed: usize) -> Vec<u8> {
        let length = (fixed + text.len() + 1).next_multiple_of(8);
        let mut out = vec![0; length];
        out[..4].copy_from_slice(&kind.to_le_bytes());
        out[4..8].copy_from_slice(&(length as u32).to_le_bytes());
        out[8..12].copy_from_slice(&(fixed as u32).to_le_bytes());
        out[fixed..fixed + text.len()].copy_from_slice(text.as_bytes());
        out
    }
    let cmds = [command(0xe, "/usr/lib/dyld", 12), command(0xc, library, 24)];
    let mut out = vec![0; 32];
    for (at, n) in [
        (0, 0xfeed_facfu32),
        (4, cpu),
        (8, subtype),
        (12, 2),
        (16, 2),
        (20, cmds.iter().map(Vec::len).sum::<usize>() as u32),
    ] {
        out[at..at + 4].copy_from_slice(&n.to_le_bytes());
    }
    out.extend(cmds.into_iter().flatten());
    out
}

fn fat(wide: bool) -> Vec<u8> {
    // Two genuine distinct arm64 subtypes are legal. Their ALL-slice union
    // avoids trying to predict which native subtype the kernel will choose.
    let images = [
        image(0x0100_0007, 3, "/usr/lib/foreign.dylib"),
        image(0x0100_000c, 2, "/usr/lib/native.dylib"),
        image(0x0100_000c, 0x8000_0002, "/usr/lib/native-new.dylib"),
    ];
    let stride = if wide { 32 } else { 20 };
    let mut out = vec![0; 4096 * images.len()];
    out[..4].copy_from_slice(&(if wide { 0xcafe_babfu32 } else { 0xcafe_babe }).to_be_bytes());
    out[4..8].copy_from_slice(&(images.len() as u32).to_be_bytes());
    for (i, image) in images.into_iter().enumerate() {
        let at = 8 + i * stride;
        out[at..at + 4].copy_from_slice(&word(&image, 4).unwrap().to_be_bytes());
        out[at + 4..at + 8].copy_from_slice(&word(&image, 8).unwrap().to_be_bytes());
        let offset = (i + 1) * 4096;
        if wide {
            out[at + 8..at + 16].copy_from_slice(&(offset as u64).to_be_bytes());
            out[at + 16..at + 24].copy_from_slice(&(image.len() as u64).to_be_bytes());
            out[at + 24..at + 28].copy_from_slice(&12u32.to_be_bytes());
        } else {
            out[at + 8..at + 12].copy_from_slice(&(offset as u32).to_be_bytes());
            out[at + 12..at + 16].copy_from_slice(&(image.len() as u32).to_be_bytes());
            out[at + 16..at + 20].copy_from_slice(&12u32.to_be_bytes());
        }
        out.resize(offset + image.len(), 0);
        out[offset..].copy_from_slice(&image);
    }
    out
}

fn parse(raw: &[u8]) -> io::Result<Commands> {
    system_commands(
        &raw[..raw.len().min(32)],
        raw.len() as u64,
        &mut |offset, out| {
            let at = usize::try_from(offset).map_err(|_| refused("test offset overflow"))?;
            out.copy_from_slice(
                raw.get(at..at + out.len())
                    .ok_or_else(|| refused("test short read"))?,
            );
            Ok(())
        },
    )
}

#[test]
fn both_universal_tables_include_every_distinct_native_and_foreign_slice() {
    for wide in [false, true] {
        let raw = fat(wide);
        assert_eq!(
            parse(&raw).unwrap().loads,
            [
                "/usr/lib/foreign.dylib",
                "/usr/lib/native.dylib",
                "/usr/lib/native-new.dylib"
            ]
        );
        assert!(
            commands(&raw, true).is_err(),
            "ordinary metadata remains thin-only"
        );
        assert!(
            produced_commands(&raw, true).is_err(),
            "produced input remains thin-only"
        );
    }
}

#[test]
fn malformed_population_ranges_headers_and_every_truncation_refuse() {
    for wide in [false, true] {
        let raw = fat(wide);
        let stride = if wide { 32 } else { 20 };
        for (at, value) in [
            (4, 0u32),
            (4, 17),
            (8, 0),
            (8 + stride, 0),
            (4096 + 4, 0),
            (4096 + 8, 4),
            (4096 + 20, u32::MAX),
        ] {
            let mut changed = raw.clone();
            changed[at..at + 4].copy_from_slice(&value.to_be_bytes());
            assert!(parse(&changed).is_err(), "accepted {wide}/{at}/{value}");
        }
        // A duplicate architecture/subtype or overlapping extent is refused.
        for (from, to, len) in [
            (8, 8 + stride, 8),
            (16, 16 + stride, if wide { 8 } else { 4 }),
        ] {
            let mut changed = raw.clone();
            let original = changed[from..from + len].to_vec();
            changed[to..to + len].copy_from_slice(&original);
            assert!(parse(&changed).is_err());
        }
        let align = 8 + if wide { 24 } else { 16 };
        let mut changed = raw.clone();
        changed[align..align + 4].copy_from_slice(&64u32.to_be_bytes());
        assert!(parse(&changed).is_err());
        if wide {
            let mut changed = raw.clone();
            changed[36..40].copy_from_slice(&1u32.to_be_bytes());
            assert!(parse(&changed).is_err());
            let mut changed = raw.clone();
            changed[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
            assert!(parse(&changed).is_err(), "offset/extent overflow refuses");
        }
        for len in 0..raw.len() {
            assert!(
                parse(&raw[..len]).is_err(),
                "accepted truncated {wide}/{len}"
            );
        }
    }
}

#[test]
fn one_foreign_or_native_non_system_dependency_cannot_hide_in_a_clean_union() {
    for wide in [false, true] {
        for offset in [4096, 8192, 12288] {
            let mut raw = fat(wide);
            // All three fixtures have a library string at the same fixed
            // command offset. Alter one slice without disturbing its table.
            let at = offset + 32 + 32 + 24;
            raw[at..at + 8].copy_from_slice(b"@rpath/x");
            assert!(parse(&raw).is_err());
        }
    }
}

#[test]
fn universal_system_commands_require_a_native_slice_and_propagate_read_failures() {
    let mut raw = fat(false);
    raw[4..8].copy_from_slice(&1u32.to_be_bytes());
    assert!(
        parse(&raw).is_err(),
        "foreign-only table is no native command"
    );
    let raw = fat(false);
    assert!(
        system_commands(&raw[..32], raw.len() as u64, &mut |_, _| {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .is_err()
    );
}
