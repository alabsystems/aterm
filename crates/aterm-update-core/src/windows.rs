// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Windows release identity and portable-zip validation shared by `aterm-release` and `aterm-update`.
//!
//! A Windows release asset is one zip of the portable folder `apps/aterm-win/build.ps1` lays
//! out, every member under the single top-level folder [`ZIP_ROOT`] (what `Compress-Archive
//! -Path <dist>\aterm` writes): [`CONSOLE_IMAGE`] and [`WINDOWED_IMAGE`] beside the rest of
//! that folder. The signed appcast carries the zip's name, SHA-256 and size
//! ([`crate::Manifest::windows_artifact`]); [`WindowsTarget::validate_zip`] is what its bytes
//! must be.

use std::collections::BTreeMap;
use std::io::{Read, Seek};

/// A deliberately bounded portable-zip download (512 MiB).
pub const WINDOWS_ZIP_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// The one top-level folder every member of the portable zip sits in.
pub const ZIP_ROOT: &str = "aterm/";

/// The console image: a shell waits for it, so verbs, pipes and scripts work.
pub const CONSOLE_IMAGE: &str = "aterm/aterm.exe";

/// The windowed image: what the Start Menu and Explorer start, with no console.
pub const WINDOWED_IMAGE: &str = "aterm/aterm-gui.exe";

/// The most members a portable zip may declare.
pub const MAX_ZIP_ENTRIES: u64 = 4096;

/// How much of each image's start [`WindowsTarget::validate_zip`] reads for its headers.
pub const PE_HEADER_PREFIX: u64 = 4096;

/// The DOS header, whose last field (`e_lfanew`, at 60) is where the PE signature sits.
const DOS_HEADER_LEN: usize = 64;
/// `PE\0\0` and the COFF file header after it.
const PE_SIGNATURE_AND_COFF_LEN: usize = 4 + 20;
/// A PE32+ optional header up to and including `NumberOfRvaAndSizes`.
const OPTIONAL_HEADER64_FIXED_LEN: usize = 112;
const PE32_PLUS_MAGIC: u16 = 0x20b;
const IMAGE_FILE_EXECUTABLE_IMAGE: u16 = 0x0002;
const IMAGE_FILE_DLL: u16 = 0x2000;
/// `Subsystem`'s offset in the optional header (the same in PE32 and PE32+).
const SUBSYSTEM_OFFSET: usize = 68;
/// The Unix file-type bits of a member's external attributes, and a symbolic link's.
const UNIX_TYPE_MASK: u32 = 0o170_000;
const UNIX_SYMLINK: u32 = 0o120_000;

/// The PE subsystem an image declares: which of the portable folder's two images it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsystem {
    /// `IMAGE_SUBSYSTEM_WINDOWS_GUI` (2): started with no console.
    Windowed,
    /// `IMAGE_SUBSYSTEM_WINDOWS_CUI` (3): attached to its parent's console, or given one.
    Console,
}

impl Subsystem {
    /// The optional header's `Subsystem` value.
    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::Windowed => 2,
            Self::Console => 3,
        }
    }
}

/// Windows targets for which the release protocol defines a portable-zip asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsTarget {
    X86_64,
}

impl WindowsTarget {
    /// Every target, in the order the manifest's fields are read.
    pub const ALL: [Self; 1] = [Self::X86_64];

    /// The executable target, not the host on which a cross-build runs.
    #[must_use]
    pub const fn native() -> Option<Self> {
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Some(Self::X86_64)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn arch(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
        }
    }

    #[must_use]
    pub const fn triple(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64-pc-windows-msvc",
        }
    }

    /// The COFF `Machine` both images must declare (`IMAGE_FILE_MACHINE_AMD64`).
    #[must_use]
    pub const fn machine(self) -> u16 {
        match self {
            Self::X86_64 => 0x8664,
        }
    }

    /// The one versioned asset spelling. Admission also validates `version`.
    #[must_use]
    pub fn asset_name(self, version: &str) -> String {
        format!("aterm-{version}-windows-{}.zip", self.arch())
    }

    /// Refuse a truncated or foreign image header: `bytes` (an image's start, through its
    /// PE32+ optional header) must be an MZ header whose `e_lfanew` names a `PE\0\0` signature
    /// past it, a COFF header of this target's machine with at least one section, flagged an
    /// executable image and not a DLL, and a PE32+ optional header declaring `subsystem`.
    pub fn validate_pe_header(self, bytes: &[u8], subsystem: Subsystem) -> Result<(), String> {
        if bytes.len() < DOS_HEADER_LEN || bytes.get(..2) != Some(b"MZ") {
            return Err("Windows image is not a complete MZ header".into());
        }
        let pe = le32(bytes, 60).and_then(|at| usize::try_from(at).ok());
        let optional = pe.and_then(|pe| pe.checked_add(PE_SIGNATURE_AND_COFF_LEN));
        let end = optional.and_then(|at| at.checked_add(OPTIONAL_HEADER64_FIXED_LEN));
        let (Some(pe), Some(optional), Some(end)) = (pe, optional, end) else {
            return Err("Windows image's PE header offset overflows".into());
        };
        if pe < DOS_HEADER_LEN || end > bytes.len() {
            return Err("Windows image's PE header overlaps its DOS header or is truncated".into());
        }
        if bytes.get(pe..pe + 4) != Some(b"PE\0\0") {
            return Err("Windows image has no PE signature where its MZ header points".into());
        }
        let field = |at: usize| le16(bytes, at).unwrap_or(0);
        let machine = field(pe + 4);
        let sections = field(pe + 6);
        let optional_len = usize::from(field(pe + 20));
        let characteristics = field(pe + 22);
        if machine != self.machine() {
            return Err(format!(
                "Windows image PE machine {machine:#06x} does not match {} ({:#06x})",
                self.triple(),
                self.machine()
            ));
        }
        if sections == 0
            || optional_len < OPTIONAL_HEADER64_FIXED_LEN
            || characteristics & IMAGE_FILE_EXECUTABLE_IMAGE == 0
            || characteristics & IMAGE_FILE_DLL != 0
        {
            return Err("Windows image is not a sectioned executable PE image".into());
        }
        if field(optional) != PE32_PLUS_MAGIC {
            return Err("Windows image is not PE32+".into());
        }
        let declared = field(optional + SUBSYSTEM_OFFSET);
        if declared != subsystem.code() {
            return Err(format!(
                "Windows image declares subsystem {declared}, not {subsystem:?} ({})",
                subsystem.code()
            ));
        }
        Ok(())
    }

    /// Refuse anything but this target's portable zip: a zip archive of at most
    /// [`MAX_ZIP_ENTRIES`] members, each unencrypted, stored or deflated, no symbolic link,
    /// and named by a path every Windows extractor lays out the same way under [`ZIP_ROOT`]
    /// ([`member_path`]) that no other member aliases (ignoring case) or uses as a folder; with
    /// [`CONSOLE_IMAGE`] a console image and [`WINDOWED_IMAGE`] a windowed one, each passing
    /// [`Self::validate_pe_header`] on its first [`PE_HEADER_PREFIX`] bytes.
    ///
    /// The headers are read, not every body: the signed SHA-256 binds the bytes, and this
    /// binds what they are.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn validate_zip<R: Read + Seek>(self, zip: R) -> Result<(), String> {
        let zip_error = |e: std::io::Error| format!("Windows artifact is not a readable zip: {e}");
        let mut archive = aterm_codec::zip::Archive::open(zip).map_err(zip_error)?;
        if archive.len() > MAX_ZIP_ENTRIES {
            return Err(format!(
                "Windows artifact declares {} members, more than {MAX_ZIP_ENTRIES}",
                archive.len()
            ));
        }
        // Each member's folded path, and whether it is a folder.
        let mut members: BTreeMap<String, bool> = BTreeMap::new();
        let mut console = None;
        let mut windowed = None;
        while let Some(entry) = archive.next_entry().map_err(zip_error)? {
            let (path, folder) = member_path(&entry.name)?;
            if entry.encrypted() || !matches!(entry.method, 0 | 8) {
                return Err(format!(
                    "Windows artifact member {path:?} is encrypted or neither stored nor deflated"
                ));
            }
            if entry.unix_mode() & UNIX_TYPE_MASK == UNIX_SYMLINK {
                return Err(format!(
                    "Windows artifact member {path:?} is a symbolic link"
                ));
            }
            let folded = path.trim_end_matches('/').to_lowercase();
            if members.insert(folded, folder).is_some() {
                return Err(format!(
                    "Windows artifact names {path:?} twice (Windows paths ignore case)"
                ));
            }
            match path {
                CONSOLE_IMAGE => console = Some(entry),
                WINDOWED_IMAGE => windowed = Some(entry),
                _ => {}
            }
        }
        for path in members.keys() {
            let mut parent = path.as_str();
            while let Some((up, _)) = parent.rsplit_once('/') {
                if members.get(up) == Some(&false) {
                    return Err(format!(
                        "Windows artifact member {up:?} is a file another member is inside"
                    ));
                }
                parent = up;
            }
        }
        for (name, entry, subsystem) in [
            (CONSOLE_IMAGE, console, Subsystem::Console),
            (WINDOWED_IMAGE, windowed, Subsystem::Windowed),
        ] {
            let entry = entry.ok_or_else(|| format!("Windows artifact has no {name}"))?;
            let mut head = Vec::new();
            archive
                .body(&entry)
                .and_then(|body| body.take(PE_HEADER_PREFIX).read_to_end(&mut head))
                .map_err(|e| format!("Windows artifact member {name:?} is unreadable: {e}"))?;
            self.validate_pe_header(&head, subsystem)
                .map_err(|why| format!("{name}: {why}"))?;
        }
        Ok(())
    }
}

/// A zip member's name as a path under [`ZIP_ROOT`], and whether it names a folder (a trailing
/// `/`): UTF-8, `/`-separated, and each component a name Windows keeps as written (no empty,
/// `.` or `..` component, none of `<>:"\|?*` or a control character, no trailing dot or space,
/// no reserved device name), so no extractor can place it anywhere else.
pub fn member_path(raw: &[u8]) -> Result<(&str, bool), String> {
    let path = std::str::from_utf8(raw)
        .map_err(|_| "Windows artifact has a member whose name is not UTF-8".to_string())?;
    let refuse = |why: &str| Err(format!("Windows artifact member {path:?} {why}"));
    let Some(rest) = path.strip_prefix(ZIP_ROOT) else {
        return refuse(&format!("is outside the top-level {ZIP_ROOT} folder"));
    };
    if rest.is_empty() {
        return Ok((path, true));
    }
    let (body, folder) = match rest.strip_suffix('/') {
        Some(body) => (body, true),
        None => (rest, false),
    };
    for component in body.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return refuse("has an empty, . or .. component");
        }
        if component
            .chars()
            .any(|c| c.is_control() || "<>:\"\\|?*".contains(c))
        {
            return refuse("has a character Windows does not allow in a file name");
        }
        if component.ends_with(['.', ' ']) {
            return refuse("has a component Windows would strip of its trailing dot or space");
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or(component)
            .to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|port| {
                stem.strip_prefix(port).is_some_and(|n| {
                    matches!(n, "¹" | "²" | "³")
                        || (n.len() == 1 && n.bytes().all(|b| b.is_ascii_digit()))
                })
            });
        if device {
            return refuse("names a reserved Windows device");
        }
    }
    Ok((path, folder))
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    let pair = bytes.get(at..at.checked_add(2)?)?;
    <[u8; 2]>::try_from(pair).ok().map(u16::from_le_bytes)
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    let quad = bytes.get(at..at.checked_add(4)?)?;
    <[u8; 4]>::try_from(quad).ok().map(u32::from_le_bytes)
}

/// A validated Windows target entry from a release manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowsArtifact<'a> {
    pub target: WindowsTarget,
    pub name: &'a str,
    pub sha256: &'a str,
    pub size: u64,
}

pub(crate) fn artifact<'a>(
    target: WindowsTarget,
    version: &str,
    name: Option<&'a str>,
    sha256: Option<&'a str>,
    size: Option<u64>,
) -> Result<Option<WindowsArtifact<'a>>, String> {
    let rule = crate::manifest::AssetRule {
        platform: "Windows",
        arch: target.arch(),
        canonical: target.asset_name(version),
        max: WINDOWS_ZIP_MAX_BYTES,
    };
    Ok(rule
        .claim(version, name, sha256, size)?
        .map(|(name, sha256, size)| WindowsArtifact {
            target,
            name,
            sha256,
            size,
        }))
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
