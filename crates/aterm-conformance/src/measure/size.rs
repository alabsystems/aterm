// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The size matrix: the bytes of the release executable's slice for this Mac's architecture,
//! against a ceiling. Binary growth is measured on every bundle. A universal executable is judged
//! one slice at a time, so a bundle that carries the x86_64 slice beside the arm64 one is held to
//! the same ceiling as a one-slice dev build.

use std::io::Read as _;
use std::path::Path;

use super::instance::{CouldNotRun, could_not_run};
use super::starvation::ShapeVerdict;

/// `FAT_MAGIC` (`<mach-o/fat.h>`), big-endian on disk: a universal image with 32-bit offsets.
const FAT_MAGIC: u32 = 0xcafe_babe;
/// `FAT_MAGIC_64`: a universal image with 64-bit offsets.
const FAT_MAGIC_64: u32 = 0xcafe_babf;
/// `MH_MAGIC_64` (`<mach-o/loader.h>`), little-endian on disk on both shipped architectures.
const MH_MAGIC_64: u32 = 0xfeed_facf;

/// `CPU_TYPE_ARM64` (`<mach/machine.h>`).
pub const CPU_TYPE_ARM64: u32 = 0x0100_000c;
/// `CPU_TYPE_X86_64`.
pub const CPU_TYPE_X86_64: u32 = 0x0100_0007;

/// The header bytes read: enough for a thin header or a universal image's table of slices.
const HEADER_BYTES: usize = 4096;

/// The Mach-O CPU type of the architecture this process runs as.
#[must_use]
pub const fn host_cputype() -> Option<u32> {
    if cfg!(target_arch = "aarch64") {
        Some(CPU_TYPE_ARM64)
    } else if cfg!(target_arch = "x86_64") {
        Some(CPU_TYPE_X86_64)
    } else {
        None
    }
}

/// A CPU type's name.
#[must_use]
pub const fn cpu_name(cputype: u32) -> &'static str {
    match cputype {
        CPU_TYPE_ARM64 => "arm64",
        CPU_TYPE_X86_64 => "x86_64",
        _ => "other",
    }
}

/// One slice of an executable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slice {
    pub cputype: u32,
    pub bytes: u64,
}

/// The slices of a Mach-O image whose first bytes are `header` and whose whole length is
/// `file_bytes`: one, the whole file, when it is thin; each entry of its table when universal.
/// # Errors
/// The image is not a 64-bit Mach-O or a universal one, or its table runs past `header` or
/// names a slice past the file's end.
pub fn slices(header: &[u8], file_bytes: u64) -> Result<Vec<Slice>, String> {
    let word = |at: usize, big: bool| -> Result<u32, String> {
        let bytes: [u8; 4] = header
            .get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| format!("the header ends before byte {}", at + 4))?;
        Ok(if big {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    };
    let dword = |at: usize| -> Result<u64, String> {
        Ok(u64::from(word(at, true)?) << 32 | u64::from(word(at + 4, true)?))
    };
    if word(0, false)? == MH_MAGIC_64 {
        return Ok(vec![Slice {
            cputype: word(4, false)?,
            bytes: file_bytes,
        }]);
    }
    let magic = word(0, true)?;
    let wide = match magic {
        FAT_MAGIC => false,
        FAT_MAGIC_64 => true,
        _ => {
            return Err(format!(
                "not a 64-bit Mach-O or universal image (magic {magic:#010x})"
            ));
        }
    };
    let n = usize::try_from(word(4, true)?).map_err(|e| e.to_string())?;
    let entry = if wide { 32 } else { 20 };
    (0..n)
        .map(|i| {
            let at = 8 + i * entry;
            let cputype = word(at, true)?;
            let (offset, bytes) = if wide {
                (dword(at + 8)?, dword(at + 16)?)
            } else {
                (
                    u64::from(word(at + 8, true)?),
                    u64::from(word(at + 12, true)?),
                )
            };
            if offset.checked_add(bytes).is_none_or(|end| end > file_bytes) {
                return Err(format!(
                    "slice {i} ({}) runs past the file's {file_bytes} bytes",
                    cpu_name(cputype)
                ));
            }
            Ok(Slice { cputype, bytes })
        })
        .collect()
}

/// One size row: the host slice's bytes may be at most `max_bytes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeRow {
    pub max_bytes: u64,
}

/// One row's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SizeReport {
    pub row: SizeRow,
    pub cputype: u32,
    /// The host slice's bytes.
    pub bytes: u64,
    pub file_bytes: u64,
    /// Every slice the image carries, by name.
    pub slices: Vec<Slice>,
    pub verdict: ShapeVerdict,
}

impl std::fmt::Display for SizeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let all: Vec<String> = self
            .slices
            .iter()
            .map(|s| format!("{}:{}", cpu_name(s.cputype), s.bytes))
            .collect();
        write!(
            f,
            "SIZE slice={} bytes={} max_bytes={} headroom_bytes={} file_bytes={} slices={} \
             verdict={:?}",
            cpu_name(self.cputype),
            self.bytes,
            self.row.max_bytes,
            i128::from(self.row.max_bytes) - i128::from(self.bytes),
            self.file_bytes,
            all.join(","),
            self.verdict
        )
    }
}

/// The row's bound against the image's slices: the slice of `cputype` is present and at most
/// the row's ceiling. The other slices are reported, never judged.
#[must_use]
pub fn judge(row: &SizeRow, slices: &[Slice], cputype: u32) -> ShapeVerdict {
    match slices.iter().find(|s| s.cputype == cputype) {
        Some(s) if s.bytes <= row.max_bytes => ShapeVerdict::Pass,
        _ => ShapeVerdict::Fail,
    }
}

/// Measure one row against the executable at `exe`.
/// # Errors
/// [`CouldNotRun`] when the file cannot be read, is not a Mach-O image, or this host's
/// architecture has no Mach-O CPU type.
pub fn measure(exe: &Path, row: &SizeRow) -> Result<SizeReport, CouldNotRun> {
    let Some(cputype) = host_cputype() else {
        return could_not_run("this host's architecture has no shipped Mach-O slice");
    };
    let refused = |e: std::io::Error| CouldNotRun::Refused(format!("{}: {e}", exe.display()));
    let file_bytes = std::fs::metadata(exe).map_err(refused)?.len();
    let mut header = Vec::with_capacity(HEADER_BYTES);
    std::fs::File::open(exe)
        .map_err(refused)?
        .take(HEADER_BYTES as u64)
        .read_to_end(&mut header)
        .map_err(refused)?;
    let slices = slices(&header, file_bytes)
        .map_err(|why| CouldNotRun::Refused(format!("{}: {why}", exe.display())))?;
    let bytes = slices
        .iter()
        .find(|s| s.cputype == cputype)
        .map_or(0, |s| s.bytes);
    Ok(SizeReport {
        row: *row,
        cputype,
        bytes,
        file_bytes,
        verdict: judge(row, &slices, cputype),
        slices,
    })
}

#[cfg(test)]
#[path = "size_tests.rs"]
mod tests;
