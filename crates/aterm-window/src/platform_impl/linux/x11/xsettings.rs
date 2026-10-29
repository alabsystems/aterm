// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/xsettings.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by the aterm project in 2026; see the repository NOTICE. (aterm-x11's requests, where
// upstream used x11rb's.)

//! Parser for the xsettings data format.

use std::num::NonZeroUsize;

use aterm_x11::proto::xproto;

use super::atoms::*;
use super::XConnection;

type Result<T> = core::result::Result<T, ParserError>;

const DPI_NAME: &[u8] = b"Xft/DPI";
const DPI_MULTIPLIER: f64 = 1024.0;
const LITTLE_ENDIAN: u8 = b'l';
const BIG_ENDIAN: u8 = b'B';

impl XConnection {
    /// Get the DPI from XSettings.
    pub(crate) fn xsettings_dpi(
        &self,
        xsettings_screen: xproto::Atom,
    ) -> core::result::Result<Option<f64>, super::X11Error> {
        let atoms = self.atoms();

        let owner = self.request(xproto::get_selection_owner(xsettings_screen))?;
        if owner.owner == aterm_x11::proto::NONE {
            return Ok(None);
        }

        let data: Vec<u8> =
            self.get_property(owner.owner, atoms[_XSETTINGS_SETTINGS], atoms[_XSETTINGS_SETTINGS])?;

        Ok(dpi_from_settings(&data)?)
    }
}

fn dpi_from_settings(data: &[u8]) -> Result<Option<f64>> {
    let dpi_setting = read_settings(data)?
        .find(|res| res.as_ref().map_or(true, |s| s.name == DPI_NAME))
        .transpose()?;
    let Some(dpi_setting) = dpi_setting else {
        return Ok(None);
    };
    let dpi = match dpi_setting.data {
        SettingData::Integer(dpi) => dpi,
        SettingData::String(_) => return Err(ParserError::BadType(SettingType::String)),
        SettingData::Color(_) => return Err(ParserError::BadType(SettingType::Color)),
    };
    if dpi <= 0 {
        return Ok(None);
    }
    Ok(Some(f64::from(dpi) / DPI_MULTIPLIER))
}

fn read_settings(data: &[u8]) -> Result<impl Iterator<Item = Result<Setting<'_>>> + '_> {
    let mut parser = Parser::new(data)?;
    let count = parser.i32()?;
    let mut remaining = usize::try_from(count).map_err(|_| ParserError::NegativeCount(count))?;

    // Stop after the first malformed setting so an excessive count cannot loop.
    Ok(std::iter::from_fn(move || {
        if remaining == 0 {
            return None;
        }
        remaining -= 1;
        let setting = Setting::parse(&mut parser);
        if setting.is_err() {
            remaining = 0;
        }
        Some(setting)
    }))
}

struct Setting<'a> {
    /// The name of the setting.
    name: &'a [u8],
    /// The data contained in the setting.
    data: SettingData<'a>,
}

enum SettingData<'a> {
    Integer(i32),
    String(#[allow(dead_code)] &'a [u8]),
    Color(#[allow(dead_code)] [i16; 4]),
}

impl<'a> Setting<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<Self> {
        let ty: SettingType = parser.i8()?.try_into()?;
        parser.advance(1)?;

        let name_len = usize::from(parser.u16()?);
        let name = parser.advance(name_len)?;
        parser.pad(name.len(), 4)?;

        parser.advance(4)?;

        let data = match ty {
            SettingType::Integer => SettingData::Integer(parser.i32()?),

            SettingType::String => {
                let data_len = parser.i32()?;
                let data_len =
                    usize::try_from(data_len).map_err(|_| ParserError::NegativeCount(data_len))?;
                let data = parser.advance(data_len)?;
                parser.pad(data.len(), 4)?;

                SettingData::String(data)
            },

            SettingType::Color => {
                let (red, blue, green, alpha) =
                    (parser.i16()?, parser.i16()?, parser.i16()?, parser.i16()?);

                SettingData::Color([red, blue, green, alpha])
            },
        };

        Ok(Setting { name, data })
    }
}

#[derive(Debug)]
pub enum SettingType {
    Integer = 0,
    String = 1,
    Color = 2,
}

impl TryFrom<i8> for SettingType {
    type Error = ParserError;

    fn try_from(value: i8) -> Result<Self> {
        Ok(match value {
            0 => Self::Integer,
            1 => Self::String,
            2 => Self::Color,
            x => return Err(ParserError::InvalidType(x)),
        })
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    endianness: Endianness,
}

impl<'a> Parser<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self> {
        let (endianness, bytes) = bytes.split_first().ok_or_else(|| ParserError::ran_out(1, 0))?;
        let endianness = match *endianness {
            BIG_ENDIAN => Endianness::Big,
            LITTLE_ENDIAN => Endianness::Little,
            _ => Endianness::native(),
        };

        Ok(Self {
            // Skip three bytes of padding and the four-byte serial.
            bytes: bytes.get(7..).ok_or_else(|| ParserError::ran_out(7, bytes.len()))?,
            endianness,
        })
    }

    fn advance(&mut self, n: usize) -> Result<&'a [u8]> {
        if n == 0 {
            return Ok(&[]);
        }

        if n > self.bytes.len() {
            Err(ParserError::ran_out(n, self.bytes.len()))
        } else {
            let (part, rem) = self.bytes.split_at(n);
            self.bytes = rem;
            Ok(part)
        }
    }

    fn pad(&mut self, size: usize, pad: usize) -> Result<()> {
        let advance = (pad - (size % pad)) % pad;
        self.advance(advance)?;
        Ok(())
    }

    fn i8(&mut self) -> Result<i8> {
        self.advance(1).map(|s| s[0] as i8)
    }

    fn i16(&mut self) -> Result<i16> {
        self.advance(2).map(|s| {
            let bytes: &[u8; 2] = s.try_into().unwrap();
            match self.endianness {
                Endianness::Big => i16::from_be_bytes(*bytes),
                Endianness::Little => i16::from_le_bytes(*bytes),
            }
        })
    }

    fn u16(&mut self) -> Result<u16> {
        self.advance(2).map(|s| {
            let bytes: &[u8; 2] = s.try_into().unwrap();
            match self.endianness {
                Endianness::Big => u16::from_be_bytes(*bytes),
                Endianness::Little => u16::from_le_bytes(*bytes),
            }
        })
    }

    fn i32(&mut self) -> Result<i32> {
        self.advance(4).map(|s| {
            let bytes: &[u8; 4] = s.try_into().unwrap();
            match self.endianness {
                Endianness::Big => i32::from_be_bytes(*bytes),
                Endianness::Little => i32::from_le_bytes(*bytes),
            }
        })
    }
}

enum Endianness {
    Little,
    Big,
}

impl Endianness {
    #[cfg(target_endian = "little")]
    fn native() -> Self {
        Endianness::Little
    }

    #[cfg(target_endian = "big")]
    fn native() -> Self {
        Endianness::Big
    }
}

/// Parser errors.
#[allow(dead_code)]
#[derive(Debug)]
pub enum ParserError {
    /// Ran out of bytes.
    NoMoreBytes { expected: NonZeroUsize, found: usize },

    /// Invalid type.
    InvalidType(i8),

    /// A settings list cannot have a negative number of entries.
    NegativeCount(i32),

    /// Bad setting type.
    BadType(SettingType),
}

impl ParserError {
    fn ran_out(expected: usize, found: usize) -> ParserError {
        let expected = NonZeroUsize::new(expected).unwrap();
        Self::NoMoreBytes { expected, found }
    }
}

#[cfg(test)]
mod tests {
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
}
