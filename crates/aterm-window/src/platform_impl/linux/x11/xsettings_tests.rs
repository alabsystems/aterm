// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/xsettings.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

const XSETTINGS: &str = include_str!("tests/xsettings.dat");

#[test]
fn empty() {
    let err = match read_settings(&[]) {
        Ok(_) => panic!(),
        Err(err) => err,
    };
    match err {
        ParserError::NoMoreBytes { expected, found } => {
            assert_eq!(expected.get(), 1);
            assert_eq!(found, 0);
        },

        _ => panic!(),
    }

    for (endian, negative, excessive) in [
        (LITTLE_ENDIAN, (-1i32).to_le_bytes(), i32::MAX.to_le_bytes()),
        (BIG_ENDIAN, (-1i32).to_be_bytes(), i32::MAX.to_be_bytes()),
    ] {
        let mut header = vec![endian, 0, 0, 0, 0, 0, 0, 0];
        header.extend_from_slice(&negative);
        assert!(matches!(read_settings(&header), Err(ParserError::NegativeCount(-1))));
        header[8..12].copy_from_slice(&excessive);
        let mut settings = read_settings(&header).expect("nonnegative count");
        assert!(settings.next().expect("one malformed entry").is_err());
        assert!(settings.next().is_none(), "the first error ends the list");
    }
}

#[test]
fn parse_xsettings() {
    let data = XSETTINGS
        .trim()
        .split(',')
        .map(|tok| {
            let val = tok.strip_prefix("0x").unwrap();
            u8::from_str_radix(val, 16).unwrap()
        })
        .collect::<Vec<_>>();

    let settings = read_settings(&data).unwrap().collect::<Result<Vec<_>>>().unwrap();

    let dpi = settings.iter().find(|s| s.name == b"Xft/DPI").unwrap();
    assert_int(&dpi.data, 96 * 1024);
    let hinting = settings.iter().find(|s| s.name == b"Xft/Hinting").unwrap();
    assert_int(&hinting.data, 1);

    let rgba = settings.iter().find(|s| s.name == b"Xft/RGBA").unwrap();
    assert_string(&rgba.data, "rgb");
    let lcd = settings.iter().find(|s| s.name == b"Xft/Lcdfilter").unwrap();
    assert_string(&lcd.data, "lcddefault");
    assert_eq!(dpi_from_settings(&data).unwrap(), Some(96.0));
}

#[test]
fn negative_or_zero_dpi_and_malformed_lengths_are_handled() {
    for raw_dpi in [-1i32, 0] {
        let mut buf = vec![LITTLE_ENDIAN, 0, 0, 0, 0, 0, 0, 0];
        buf.extend_from_slice(&1i32.to_le_bytes());
        buf.extend_from_slice(&[0, 0]); // SettingType::Integer + pad
        buf.extend_from_slice(&(DPI_NAME.len() as u16).to_le_bytes());
        buf.extend_from_slice(DPI_NAME);
        buf.push(0); // pad to 4
        buf.extend_from_slice(&0u32.to_le_bytes()); // serial
        buf.extend_from_slice(&raw_dpi.to_le_bytes());
        assert_eq!(dpi_from_settings(&buf).unwrap(), None);
    }

    // Unsigned CARD16 name_len >= 0x8000 reports its true expected byte count rather than sign-extending.
    let mut big_name = vec![LITTLE_ENDIAN, 0, 0, 0, 0, 0, 0, 0];
    big_name.extend_from_slice(&1i32.to_le_bytes());
    big_name.extend_from_slice(&[0, 0]);
    big_name.extend_from_slice(&0x8000u16.to_le_bytes());
    let mut settings = read_settings(&big_name).unwrap();
    assert!(matches!(
        settings.next(),
        Some(Err(ParserError::NoMoreBytes { expected, found: 0 })) if expected.get() == 0x8000
    ));

    // Negative i32 string length is rejected as NegativeCount rather than sign-extending.
    let mut neg_str = vec![LITTLE_ENDIAN, 0, 0, 0, 0, 0, 0, 0];
    neg_str.extend_from_slice(&1i32.to_le_bytes());
    neg_str.extend_from_slice(&[1, 0]); // SettingType::String + pad
    neg_str.extend_from_slice(&4u16.to_le_bytes());
    neg_str.extend_from_slice(b"test");
    neg_str.extend_from_slice(&0u32.to_le_bytes()); // serial
    neg_str.extend_from_slice(&(-1i32).to_le_bytes());
    let mut settings = read_settings(&neg_str).unwrap();
    assert!(matches!(settings.next(), Some(Err(ParserError::NegativeCount(-1)))));
}

fn assert_string(dat: &SettingData<'_>, s: &str) {
    match dat {
        SettingData::String(left) => assert_eq!(*left, s.as_bytes()),
        _ => panic!("invalid data type"),
    }
}

fn assert_int(dat: &SettingData<'_>, i: i32) {
    match dat {
        SettingData::Integer(left) => assert_eq!(*left, i),
        _ => panic!("invalid data type"),
    }
}
