// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// Build a raw 512-byte USTAR header.
pub(crate) fn raw_header_mode(
    name: &str,
    typeflag: u8,
    linkname: &str,
    size: usize,
    mode: u32,
) -> [u8; 512] {
    let mut h = [0u8; 512];
    let nb = name.as_bytes();
    let n = nb.len().min(100);
    h[..n].copy_from_slice(&nb[..n]);
    h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[148..156].copy_from_slice(b"        ");
    h[156] = typeflag;
    let lb = linkname.as_bytes();
    let l = lb.len().min(100);
    h[157..157 + l].copy_from_slice(&lb[..l]);
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

/// Build a tar archive from `(name, typeflag, linkname, content, mode)` tuples.
pub(crate) fn tar_bytes(entries: &[(&str, u8, &str, &[u8], u32)]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (name, tf, link, content, mode) in entries {
        tar.extend_from_slice(&raw_header_mode(name, *tf, link, content.len(), *mode));
        tar.extend_from_slice(content);
        let pad = (512 - content.len() % 512) % 512;
        tar.resize(tar.len() + pad, 0);
    }
    tar.resize(tar.len() + 1024, 0);
    tar
}

/// Wrap `raw` in a checksummed zstd frame.
pub(crate) fn zstd_bytes(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    aterm_zstd::frame::write_raw(&mut out, raw, aterm_zstd::frame::Checksum::Xxh64).unwrap();
    out
}

/// Wrap `raw` in a RFC 1952 gzip member.
pub(crate) fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    out.extend_from_slice(&deflate_bytes(raw));
    out.extend_from_slice(&crc32(raw).to_le_bytes());
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out
}

/// Wrap `raw` in stored RFC 1951 DEFLATE blocks.
pub(crate) fn deflate_bytes(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 5 * (raw.len() / 0xFFFF + 1));
    let mut rest = raw;
    loop {
        let (block, tail) = rest.split_at(rest.len().min(0xFFFF));
        out.push(u8::from(tail.is_empty()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
        if tail.is_empty() {
            return out;
        }
        rest = tail;
    }
}

/// Single member specification for [`zip_bytes`].
pub(crate) struct ZipMember<'a> {
    pub(crate) name: &'a str,
    pub(crate) mode: u32,
    pub(crate) data: &'a [u8],
    pub(crate) deflate: bool,
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn le64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Build a complete zip archive (optionally with ZIP64 headers).
pub(crate) fn zip_bytes(members: &[ZipMember<'_>], zip64: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for m in members {
        let body: Vec<u8> = if m.deflate {
            deflate_bytes(m.data)
        } else {
            m.data.to_vec()
        };
        let method: u16 = if m.deflate { 8 } else { 0 };
        let crc = crc32(m.data);
        let local_offset = out.len() as u64;
        le32(&mut out, 0x0403_4b50);
        le16(&mut out, 20);
        le16(&mut out, 0);
        le16(&mut out, method);
        le16(&mut out, 0);
        le16(&mut out, 0);
        le32(&mut out, crc);
        le32(&mut out, body.len() as u32);
        le32(&mut out, m.data.len() as u32);
        le16(&mut out, m.name.len() as u16);
        le16(&mut out, 0);
        out.extend_from_slice(m.name.as_bytes());
        out.extend_from_slice(&body);
        let host: u16 = if m.mode == 0 { 0 } else { 3 };
        le32(&mut central, 0x0201_4b50);
        le16(&mut central, (host << 8) | 20);
        le16(&mut central, 20);
        le16(&mut central, 0);
        le16(&mut central, method);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le32(&mut central, crc);
        let mut extra = Vec::new();
        if zip64 {
            le32(&mut central, 0xFFFF_FFFF);
            le32(&mut central, 0xFFFF_FFFF);
            le16(&mut extra, 0x0001);
            le16(&mut extra, 24);
            le64(&mut extra, m.data.len() as u64);
            le64(&mut extra, body.len() as u64);
            le64(&mut extra, local_offset);
        } else {
            le32(&mut central, body.len() as u32);
            le32(&mut central, m.data.len() as u32);
        }
        le16(&mut central, m.name.len() as u16);
        le16(&mut central, extra.len() as u16);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le32(&mut central, m.mode << 16);
        le32(
            &mut central,
            if zip64 {
                0xFFFF_FFFF
            } else {
                local_offset as u32
            },
        );
        central.extend_from_slice(m.name.as_bytes());
        central.extend_from_slice(&extra);
    }
    let cd_offset = out.len() as u64;
    let cd_size = central.len() as u64;
    out.extend_from_slice(&central);
    if zip64 {
        let eocd64_offset = out.len() as u64;
        le32(&mut out, 0x0606_4b50);
        le64(&mut out, 44);
        le16(&mut out, 45);
        le16(&mut out, 45);
        le32(&mut out, 0);
        le32(&mut out, 0);
        le64(&mut out, members.len() as u64);
        le64(&mut out, members.len() as u64);
        le64(&mut out, cd_size);
        le64(&mut out, cd_offset);
        le32(&mut out, 0x0706_4b50);
        le32(&mut out, 0);
        le64(&mut out, eocd64_offset);
        le32(&mut out, 1);
        le32(&mut out, 0x0605_4b50);
        le16(&mut out, 0xFFFF);
        le16(&mut out, 0xFFFF);
        le16(&mut out, 0xFFFF);
        le16(&mut out, 0xFFFF);
        le32(&mut out, 0xFFFF_FFFF);
        le32(&mut out, 0xFFFF_FFFF);
        le16(&mut out, 0);
    } else {
        le32(&mut out, 0x0605_4b50);
        le16(&mut out, 0);
        le16(&mut out, 0);
        le16(&mut out, members.len() as u16);
        le16(&mut out, members.len() as u16);
        le32(&mut out, cd_size as u32);
        le32(&mut out, cd_offset as u32);
        le16(&mut out, 0);
    }
    out
}
