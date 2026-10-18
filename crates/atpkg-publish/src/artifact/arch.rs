// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The arch gate: a pack's binaries must match its claimed target, read from each one's
//! own header (ELF `e_machine`, Mach-O thin and fat `cputype`, PE COFF `Machine`).
//!
//! [`expect`] is total: [`crate::triple::Triple`] names only targets the client ships. A
//! `#!` script in a tree is named and not gated; an unreadable object, a format or
//! architecture mismatch, and a tree with no object are refusals. The reader is
//! in-process, so no publisher skips the gate for lack of a tool.

use std::fmt;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::triple::{Arch, Os, Triple};

/// An object file format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// ELF (Linux).
    Elf,
    /// Mach-O, thin or fat (macOS).
    MachO,
    /// PE/COFF (Windows).
    Pe,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Elf => "elf",
            Self::MachO => "macho",
            Self::Pe => "pe",
        })
    }
}

/// One architecture an object carries, in `lipo`'s vocabulary. One the reader does not
/// name keeps what it read, so a refusal still says what the file is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cpu {
    /// x86-64.
    X86_64,
    /// AArch64 (`arm64`, `arm64e`).
    Arm64,
    /// Anything else, as read (`e_machine=243`, `cputype=7`, `class=1 data=1`).
    Other(String),
}

impl fmt::Display for Cpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::X86_64 => f.write_str("x86_64"),
            Self::Arm64 => f.write_str("arm64"),
            Self::Other(s) => f.write_str(s),
        }
    }
}

/// What a file's header says it is: its format and every architecture it carries
/// (more than one only for a fat Mach-O).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Object {
    /// The format.
    pub format: Format,
    /// The architectures, in header order.
    pub cpus: Vec<Cpu>,
}

impl fmt::Display for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format)?;
        for c in &self.cpus {
            write!(f, " {c}")?;
        }
        Ok(())
    }
}

/// What a pack claiming `triple` must contain: its format and its architecture.
#[must_use]
pub fn expect(triple: Triple) -> (Format, Cpu) {
    let format = match triple.os() {
        Os::Darwin => Format::MachO,
        Os::Linux => Format::Elf,
        Os::Windows => Format::Pe,
    };
    let cpu = match triple.arch() {
        Arch::X86_64 => Cpu::X86_64,
        Arch::Aarch64 => Cpu::Arm64,
    };
    (format, cpu)
}

/// The most a header read needs: a fat Mach-O's 16 slices of 32 bytes after its 8-byte
/// head, and a PE's 64-byte DOS header.
const HEAD: usize = 8 + 16 * 32;

/// Read what `path` is out of its own header. `None` for anything that is not an object
/// this reader knows: a script, text, an `ar` archive, a Java class, an empty, short,
/// missing or unreadable file — each of which the gate refuses.
#[must_use]
pub fn read(path: &Path) -> Option<Object> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut b = Vec::with_capacity(HEAD);
    f.by_ref().take(HEAD as u64).read_to_end(&mut b).ok()?;
    if b.len() < 20 {
        return None;
    }
    if b.starts_with(b"\x7fELF") {
        return Some(read_elf(&b));
    }
    if matches!(b[0], 0xcf | 0xce) && b[1..4] == [0xfa, 0xed, 0xfe] {
        let cpu = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        return Some(Object {
            format: Format::MachO,
            cpus: vec![macho_cpu(cpu)],
        });
    }
    if b[..3] == [0xca, 0xfe, 0xba] && matches!(b[3], 0xbe | 0xbf) {
        return read_fat(&b);
    }
    if b.starts_with(b"MZ") {
        return read_pe(&mut f, &b);
    }
    None
}

fn read_elf(b: &[u8]) -> Object {
    // Not 64-bit little-endian: never an aterm target, named so the refusal says so.
    let cpu = if b[4] != 2 || b[5] != 1 {
        Cpu::Other(format!("class={} data={}", b[4], b[5]))
    } else {
        match u16::from_le_bytes([b[18], b[19]]) {
            62 => Cpu::X86_64,
            183 => Cpu::Arm64,
            m => Cpu::Other(format!("e_machine={m}")),
        }
    };
    Object {
        format: Format::Elf,
        cpus: vec![cpu],
    }
}

fn macho_cpu(cputype: u32) -> Cpu {
    match cputype {
        0x0100_0007 => Cpu::X86_64,
        0x0100_000c => Cpu::Arm64,
        7 => Cpu::Other("i386".into()),
        12 => Cpu::Other("arm".into()),
        n => Cpu::Other(format!("cputype={n}")),
    }
}

/// FAT_MAGIC / FAT_MAGIC_64, big-endian by definition: `nfat_arch` at 4, then one
/// `fat_arch` (20 bytes) or `fat_arch_64` (32) per slice, each led by its cputype.
fn read_fat(b: &[u8]) -> Option<Object> {
    let esz = if b[3] == 0xbf { 32 } else { 20 };
    let n = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize;
    // 0xcafebabe is also a Java class file; a sane slice count tells them apart.
    if !(1..=16).contains(&n) || b.len() < 8 + n * esz {
        return None;
    }
    let cpus = (0..n)
        .map(|i| {
            let o = 8 + i * esz;
            macho_cpu(u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]))
        })
        .collect();
    Some(Object {
        format: Format::MachO,
        cpus,
    })
}

/// `MZ`, `e_lfanew` (u32 LE at 0x3c) → `PE\0\0`, then the COFF `Machine` u16.
fn read_pe(f: &mut std::fs::File, b: &[u8]) -> Option<Object> {
    use std::io::{Seek as _, SeekFrom};
    if b.len() < 64 {
        return None;
    }
    let at = u32::from_le_bytes([b[60], b[61], b[62], b[63]]);
    let mut h = [0u8; 6];
    f.seek(SeekFrom::Start(u64::from(at))).ok()?;
    f.read_exact(&mut h).ok()?;
    if h[..4] != *b"PE\0\0" {
        return None;
    }
    let cpu = match u16::from_le_bytes([h[4], h[5]]) {
        0x8664 => Cpu::X86_64,
        0xaa64 => Cpu::Arm64,
        m => Cpu::Other(format!("machine={m}")),
    };
    Some(Object {
        format: Format::Pe,
        cpus: vec![cpu],
    })
}

/// Why the gate refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Nothing to read: no file named, or a tree holding no object (only these scripts).
    NoObjects {
        /// The claim.
        triple: Triple,
        /// The `#!` scripts the tree did hold.
        scripts: usize,
    },
    /// The tree to gate is not a directory.
    MissingDir {
        /// It.
        dir: PathBuf,
    },
    /// A file that is not an object this reader knows.
    Unreadable {
        /// It.
        file: PathBuf,
        /// The claim.
        triple: Triple,
    },
    /// An object of another format (an ELF claimed for macOS).
    FormatMismatch {
        /// It.
        file: PathBuf,
        /// What it is.
        got: Object,
        /// The claim.
        triple: Triple,
    },
    /// An object of the right format that carries no slice of the claimed architecture.
    ArchMismatch {
        /// It.
        file: PathBuf,
        /// What it is.
        got: Object,
        /// The claim.
        triple: Triple,
    },
}

fn base(p: &Path) -> String {
    p.file_name()
        .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into())
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoObjects { triple, scripts } => write!(
                f,
                "ARCH GATE — nothing to read ({scripts} script(s) and no object file): a pack \
                 that proves nothing about its own architecture is not signed ({triple})"
            ),
            Self::MissingDir { dir } => write!(
                f,
                "ARCH GATE — {} is not a directory; a tree nothing read is not signed",
                dir.display()
            ),
            Self::Unreadable { file, triple } => write!(
                f,
                "ARCH GATE — cannot read the architecture of {}: it is not an ELF, a Mach-O or \
                 a PE image. Refusing to sign a manifest claiming {triple} for it",
                base(file)
            ),
            Self::FormatMismatch { file, got, triple } => {
                let (want, _) = expect(*triple);
                write!(
                    f,
                    "OBJECT-FORMAT MISMATCH — {} is [{got}] but this pack claims {triple} \
                     (expected a {want} image): a binary for another OS cannot exec on the \
                     machines that select this target, however well its signature verifies",
                    base(file)
                )
            }
            Self::ArchMismatch { file, got, triple } => {
                let (_, want) = expect(*triple);
                write!(
                    f,
                    "ARCHITECTURE MISMATCH — {} is [{got}] but this pack claims {triple} \
                     (expected {want}): its signature would verify while the binaries cannot \
                     exec on the machines that select it. Build with --target {triple}",
                    base(file)
                )
            }
        }
    }
}

impl From<Refusal> for Error {
    fn from(r: Refusal) -> Self {
        Error::refused(r.to_string())
    }
}

/// Every one of `files` must be an object of `triple`'s format and architecture; a fat
/// Mach-O passes when it carries the claimed slice. Returns how many were read.
///
/// # Errors
///
/// The first file that is unreadable or mismatched, or an empty list.
pub fn gate(triple: Triple, files: &[PathBuf]) -> Result<usize, Refusal> {
    if files.is_empty() {
        return Err(Refusal::NoObjects { triple, scripts: 0 });
    }
    let (want_format, want_cpu) = expect(triple);
    for file in files {
        let Some(got) = read(file) else {
            return Err(Refusal::Unreadable {
                file: file.clone(),
                triple,
            });
        };
        if got.format != want_format {
            return Err(Refusal::FormatMismatch {
                file: file.clone(),
                got,
                triple,
            });
        }
        if !got.cpus.contains(&want_cpu) {
            return Err(Refusal::ArchMismatch {
                file: file.clone(),
                got,
                triple,
            });
        }
    }
    Ok(files.len())
}

/// What [`gate_tree`] read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeGate {
    /// Objects gated.
    pub objects: usize,
    /// The `#!` scripts named and not gated, relative to the tree.
    pub scripts: Vec<String>,
}

/// [`gate`] over every regular file under `dir` (no symlink followed), for a pre-built
/// tree with no `--target` to rely on. A `#!` script is named and not gated; anything else
/// that is not a readable object is a refusal.
///
/// # Errors
///
/// A missing directory, a tree with no object, or [`gate`]'s refusal.
pub fn gate_tree(triple: Triple, dir: &Path) -> Result<TreeGate, Refusal> {
    let files = crate::artifact::relocate::walk_files(dir).map_err(|_| Refusal::MissingDir {
        dir: dir.to_path_buf(),
    })?;
    let mut scripts = Vec::new();
    let mut objects = Vec::new();
    for rel in files {
        let path = dir.join(&rel);
        if is_script(&path) {
            scripts.push(rel.to_string_lossy().into_owned());
        } else {
            objects.push(path);
        }
    }
    if objects.is_empty() {
        return Err(Refusal::NoObjects {
            triple,
            scripts: scripts.len(),
        });
    }
    let objects = gate(triple, &objects)?;
    Ok(TreeGate { objects, scripts })
}

fn is_script(path: &Path) -> bool {
    let mut two = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut two))
        .is_ok_and(|()| two == *b"#!")
}
