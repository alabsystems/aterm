// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What setup's one elevation installs for the LAN broker, and how an account with no root reads
//! whether that install is current (`docs/DESIGN-lan-broker-2026-11-05.md` §3, §5).
//!
//! The elevation clones the app bundle it ran from (the **source**) to [`super::IMAGE_BUNDLE`],
//! root-owned, and writes [`super::PLIST_PATH`] ([`plist_text`]), whose job runs the clone's
//! executable as `aterm lan serve --source <source>` ([`program_arguments`]). Both are readable
//! without root: the image is `root:wheel` with read bits for everyone, and so is the plist.
//!
//! A build's number is its bundle's `CFBundleVersion` ([`build_number`]): the one the updater
//! orders builds by, which a release stamps from its ledger and the signature seals (each
//! CodeDirectory's Info slot). The running broker only moves up. Besides its job's source, it
//! adopts the bundle of a team process that asked it to ([`consider_for`], `aterm-lan`'s
//! `askers`), and each only through a root-owned clone that passed its strict check and states a
//! strictly newer number than the image it serves. So an account reads ([`read`]):
//! - a bundle whose executable states no team [`Signer`] of [`BUNDLE_ID`] (a development build)
//!   owns no broker outside a VM guest ([`Part::Development`]): it owes nothing, and its LAN goes
//!   by macOS's allowance for it first (`super::direct`, [`development_here`]);
//! - the image as this build's code ([`Part::Current`]); an older build of this build's signer,
//!   which the broker adopts by itself once this build asks ([`Part::Adopting`], an expectation
//!   [`super::adoption`] bounds); a build of that signer at least as new, which it keeps
//!   ([`Part::Kept`]) beside whatever plist that build wrote, never owed, whatever launchd holds
//!   of its job ([`Reading::owed_with_job`]); and anything else as owed;
//! - the plist in place where it is byte for byte the one this build writes for any bundle
//!   ([`plist_text`]): which bundle it names owes nothing, as the broker takes every newer build
//!   that asks, whoever's bundle the job names.
//!
//! The numbers an account reads decide only whether it asks; nothing root does depends on them.
//! Root reads a candidate's number from its own sealed clone.
//!
//! Code is compared by the executable's **CodeDirectory** blobs ([`CodeDirectories`]), read
//! statically from the Mach-O file: for every architecture slice, every CodeDirectory its
//! embedded signature carries (the primary and each alternate). A cdhash is a hash of one of
//! those blobs, and each blob hashes every code page and, in its special slots, the bundle's
//! `Info.plist` and its sealed resources (`_CodeSignature/CodeResources`), so equal blobs are
//! equal code, resources included, whatever hash type the kernel ranks first. A clone is
//! byte-identical, so a current image's blobs equal the source's. Only the load commands and the
//! signature are read, never the whole file, and nothing is evaluated (no `SecStaticCode`), so
//! the reading needs no root, no framework and no network.
//!
//! The GUI (`aterm-gui`'s `clt_install`) and `aterm init` (`aterm_machines::root_steps`) both read
//! through [`read`], so the two can never disagree about what is owed.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub use crate::launchd::JobState;

/// The bundle identifier the broker's job is attributed to (`AssociatedBundleIdentifiers`), so
/// macOS's background-item notice names aterm, not a file.
pub const BUNDLE_ID: &str = "com.aterm.aterm";

/// A bundle's executable, relative to the bundle: what launchd runs in the clone
/// ([`super::IMAGE_EXECUTABLE`]) and what the clone is compared against in the source.
pub const BUNDLE_EXECUTABLE: &str = "Contents/MacOS/aterm";

/// A bundle's property list, relative to the bundle: where its build is read ([`build_of`]), and
/// what each CodeDirectory's Info slot seals.
pub const BUNDLE_INFO: &str = "Contents/Info.plist";

/// The longest plist [`Disk`] reads; anything longer is not one this build wrote.
const PLIST_MAX: u64 = 64 * 1024;

/// The argv launchd starts the broker with: the clone's executable, `lan serve`, and the source
/// bundle it was cloned from, which the broker watches for a signed update to adopt.
#[must_use]
pub fn program_arguments(source: &Path) -> Vec<String> {
    vec![
        super::IMAGE_EXECUTABLE.to_owned(),
        "lan".to_owned(),
        "serve".to_owned(),
        "--source".to_owned(),
        source.to_string_lossy().into_owned(),
    ]
}

/// Escape `text` for a plist `<string>`.
fn xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The broker's LaunchDaemon plist, for an image cloned from `source`: a system job with no
/// `UserName` (root, which macOS exempts from Local Network privacy), started at load and kept
/// alive, an interactive-class process (`Standard`: it serves another process's request), its
/// output in [`super::LOG_PATH`], attributed to [`BUNDLE_ID`].
#[must_use]
pub fn plist_text(source: &Path) -> String {
    let string = |text: &str| format!("<string>{}</string>", xml(text));
    let argv: String = program_arguments(source)
        .iter()
        .map(|arg| format!("\t\t{}\n", string(arg)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t{label}\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         {argv}\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<true/>\n\
         \t<key>ProcessType</key>\n\
         \t<string>Standard</string>\n\
         \t<key>AssociatedBundleIdentifiers</key>\n\
         \t<array>\n\
         \t\t{bundle}\n\
         \t</array>\n\
         \t<key>StandardOutPath</key>\n\
         \t{log}\n\
         \t<key>StandardErrorPath</key>\n\
         \t{log}\n\
         </dict>\n\
         </plist>\n",
        label = string(super::LABEL),
        bundle = string(BUNDLE_ID),
        log = string(super::LOG_PATH),
    )
}

/// The app bundle `exe` runs from: the nearest folder above it named `*.app`, or `None` for an
/// executable outside any bundle (a development build), which the broker never runs.
#[must_use]
pub fn source_bundle(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .skip(1)
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
}

/// The build a bundle's `Info.plist` states ([`BUNDLE_INFO`]): its decimal `CFBundleVersion`,
/// read where the release stamps it (`aterm_release::bundle::stamp_info_plist`): in an XML plist,
/// the first `<key>CFBundleVersion</key>` outside a comment, then, past whitespace and comments,
/// a `<string>` of 1 to 20 ASCII digits that is not zero and fits a `u64`. Anything else states
/// no build: a binary plist, the template's unstamped `0`, another spelling, no key.
#[must_use]
pub fn build_number(plist: &[u8]) -> Option<u64> {
    const KEY: &str = "<key>CFBundleVersion</key>";
    let text = std::str::from_utf8(plist).ok()?;
    let at = outside_comments(text)?
        .into_iter()
        .find_map(|(start, end)| text[start..end].find(KEY).map(|at| start + at))?;
    let mut rest = &text[at + KEY.len()..];
    loop {
        rest = rest.trim_start();
        let Some(comment) = rest.strip_prefix("<!--") else {
            break;
        };
        rest = &comment[comment.find("-->")? + 3..];
    }
    let rest = rest.strip_prefix("<string>")?;
    let digits = &rest[..rest.find("</string>")?];
    if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok().filter(|build| *build != 0)
}

/// The spans of `text` outside its XML comments (`<!-- ... -->`), as byte ranges, or `None` for
/// a comment never closed.
fn outside_comments(text: &str) -> Option<Vec<(usize, usize)>> {
    let mut spans = Vec::new();
    let mut at = 0;
    while let Some(open) = text[at..].find("<!--").map(|open| at + open) {
        spans.push((at, open));
        at = open + 4 + text[open + 4..].find("-->")? + 3;
    }
    spans.push((at, text.len()));
    Some(spans)
}

/// The build the bundle at `bundle` states ([`build_number`] of its [`BUNDLE_INFO`], read through
/// `files`): `None` where that file is missing or unreadable, or states none.
fn build_of(files: &dyn Files, bundle: &Path) -> Option<u64> {
    files
        .read(&bundle.join(BUNDLE_INFO))
        .ok()
        .and_then(|plist| build_number(&plist))
}

/// The whole regular file at `path`, at most [`PLIST_MAX`] bytes, read with no link anywhere on
/// its way: `path` must be its own canonical path, and the open refuses a link on it
/// ([`crate::nofollow::open_reader`]: `O_NOFOLLOW_ANY`, so a folder swapped for a link after the
/// first look answers `ELOOP`) and never waits on a FIFO (`O_NONBLOCK`, then `fstat`). The broker
/// reads a file of a bundle an account can change with it, as root.
///
/// # Errors
/// `InvalidInput` for a path that is not its canonical one, the open's error (`ELOOP` for a link,
/// `EFTYPE` for no regular file; `Unsupported` off macOS), or `InvalidData` for a file longer than
/// any plist this build reads.
pub fn read_unlinked(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    if std::fs::canonicalize(path)? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not its own canonical path", path.display()),
        ));
    }
    let file = std::fs::File::from(crate::nofollow::open_reader(path)?);
    let mut bytes = Vec::new();
    file.take(PLIST_MAX + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > PLIST_MAX {
        return Err(bad("the file is longer than any plist this build reads"));
    }
    Ok(bytes)
}

/// One executable's code identity: per architecture slice (its CPU type), every CodeDirectory
/// blob of its embedded signature, in the signature's order (module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeDirectories(Vec<(u32, Vec<u8>)>);

/// `FAT_MAGIC`, big-endian.
const FAT_MAGIC: u32 = 0xcafe_babe;
/// `FAT_MAGIC_64`, big-endian.
const FAT_MAGIC_64: u32 = 0xcafe_babf;
/// `MH_MAGIC`, in the file's (little-endian) order.
const MH_MAGIC: u32 = 0xfeed_face;
/// `MH_MAGIC_64`, in the file's (little-endian) order.
const MH_MAGIC_64: u32 = 0xfeed_facf;
/// `LC_CODE_SIGNATURE`.
const LC_CODE_SIGNATURE: u32 = 0x1d;
/// `CSMAGIC_EMBEDDED_SIGNATURE`, big-endian.
const EMBEDDED_SIGNATURE: u32 = 0xfade_0cc0;
/// `CSMAGIC_CODEDIRECTORY`, big-endian.
const CODE_DIRECTORY: u32 = 0xfade_0c02;
/// `CS_ADHOC` in a CodeDirectory's flags: signed with no identity.
const CS_ADHOC: u32 = 0x0000_0002;
/// `CS_LINKER_SIGNED`: the linker's own ad hoc signature.
const CS_LINKER_SIGNED: u32 = 0x0002_0000;
/// `CS_SUPPORTSTEAMID`: the first CodeDirectory version with a `teamOffset`.
const SUPPORTS_TEAM_ID: u32 = 0x0002_0200;
/// `CSSLOT_CODEDIRECTORY`, and the alternates' `CSSLOT_ALTERNATE_CODEDIRECTORIES` and the four
/// after it.
const DIRECTORY_SLOTS: [u32; 6] = [0, 0x1000, 0x1001, 0x1002, 0x1003, 0x1004];
/// The most slices a universal file may name.
const MAX_SLICES: u32 = 16;
/// The most load-command bytes one slice may declare.
const MAX_COMMANDS: u32 = 1 << 20;
/// The most blobs a signature's index may name.
const MAX_BLOBS: u32 = 64;
/// The longest CodeDirectory read (a 1 GiB executable's, at 4 KiB pages and 48-byte hashes, is
/// about 12 MiB), and the most CodeDirectory bytes one executable's reading keeps in all.
const MAX_DIRECTORY: u32 = 32 << 20;

/// Positioned reads of an executable: its bytes, or its file. A read that runs past the end is
/// refused (`UnexpectedEof`) before anything is allocated for it.
trait At {
    fn at(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>>;
}

impl At for &[u8] {
    fn at(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let start = usize::try_from(offset).map_err(|_| short())?;
        let end = start.checked_add(len).ok_or_else(short)?;
        self.get(start..end).map(<[u8]>::to_vec).ok_or_else(short)
    }
}

impl At for std::fs::File {
    fn at(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        use std::io::{Read as _, Seek as _};
        let end = plus(offset, u64::try_from(len).map_err(|_| short())?)?;
        if end > self.metadata()?.len() {
            return Err(short());
        }
        self.seek(io::SeekFrom::Start(offset))?;
        let mut bytes = vec![0_u8; len];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

#[cfg(target_os = "macos")]
impl At for &std::fs::File {
    fn at(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        use std::os::unix::fs::FileExt as _;
        if plus(offset, u64::try_from(len).map_err(|_| short())?)? > self.metadata()?.len() {
            return Err(short());
        }
        let mut bytes = vec![0; len];
        self.read_exact_at(&mut bytes, offset)?;
        Ok(bytes)
    }
}

/// `a + b` for offsets the file names: one past `u64` reads as a file that ends early.
fn plus(a: u64, b: u64) -> io::Result<u64> {
    a.checked_add(b).ok_or_else(short)
}

fn short() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "the executable ends early")
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_owned())
}

fn be(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_be_bytes)
}

fn le(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

fn be64(bytes: &[u8], at: usize) -> u64 {
    bytes
        .get(at..at + 8)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u64::from_be_bytes)
}

impl CodeDirectories {
    /// A supported CodeDirectory of this machine's architecture in a selected image.
    /// Unlike a kernel observation, this says nothing about a running process.
    #[cfg(target_os = "macos")]
    pub(crate) fn selected_native_hash(&self) -> io::Result<[u8; 20]> {
        #[cfg(target_arch = "aarch64")]
        let cpu = 0x0100_000c;
        #[cfg(target_arch = "x86_64")]
        let cpu = 0x0100_0007;
        self.0
            .iter()
            .filter(|(kind, _)| *kind == cpu)
            .filter_map(|(_, bytes)| bytes.get(37).and_then(|kind| directory_hash(*kind, bytes)))
            .next_back()
            .ok_or_else(|| io::Error::other("selected image has no supported native CodeDirectory"))
    }

    /// Read a held image without reopening its name or changing its shared cursor.
    /// # Errors
    /// The same malformed-image and read errors as [`Self::of_file`].
    #[cfg(target_os = "macos")]
    pub(crate) fn of_held(mut file: &std::fs::File) -> io::Result<Self> {
        Self::read(&mut file)
    }

    /// Whether the loaded directory seals external bundle metadata. Unknown or malformed
    /// hash tables refuse classification; they must never authorize a bare image copy.
    #[cfg(target_os = "macos")]
    pub(crate) fn bundle_bound(&self, expected: [u8; 20]) -> Option<bool> {
        let bytes = &self
            .0
            .iter()
            .find(|(_, bytes)| {
                bytes.get(37).and_then(|kind| directory_hash(*kind, bytes)) == Some(expected)
            })?
            .1;
        let size = usize::from(*bytes.get(36)?);
        if !matches!(size, 20 | 32 | 48) {
            return None;
        }
        let count = usize::try_from(be(bytes, 24)).ok()?;
        let start = usize::try_from(be(bytes, 16)).ok()?;
        let first = start.checked_sub(count.checked_mul(size)?)?;
        bytes.get(first..start)?;
        // CSSLOT_INFOSLOT and CSSLOT_RESOURCEDIR. Other special slots, such as
        // entitlements and requirements, live inside the executable's signature.
        Some([1, 3].into_iter().any(|slot| {
            slot <= count
                && bytes[start - slot * size..start - (slot - 1) * size]
                    .iter()
                    .any(|b| *b != 0)
        }))
    }

    /// The code identity of the executable at `path`, read with no link anywhere on its way
    /// ([`crate::nofollow::open_reader`]), never waiting on a FIFO: the broker's read of an
    /// executable in a bundle an account can change.
    ///
    /// # Errors
    /// The open's error (`ELOOP` for a link on the way, `EFTYPE` for no regular file), and
    /// [`Self::of_bytes`]'s.
    #[cfg(target_os = "macos")]
    pub fn of_unlinked(path: &Path) -> io::Result<Self> {
        let file = std::fs::File::from(crate::nofollow::open_reader(path)?);
        Self::of_held(&file)
    }

    /// Whether every CodeDirectory binds an `Info.plist`: a hash size of 20, 32 or 48 bytes that
    /// agrees with its hash type, at least one special slot, and a nonzero Info slot (the first
    /// special slot) inside the directory ([`info_slot_of`]). A clone the broker adopts shows it
    /// before its number counts: an unbound plist could state any number.
    #[must_use]
    pub fn binds_info_plist(&self) -> bool {
        !self.0.is_empty() && self.0.iter().all(|(_, blob)| info_slot_of(blob).is_some())
    }

    /// Whether every CodeDirectory binds `info` ([`Self::binds_info_plist`]) and its Info slot is
    /// `info`'s digest in that directory's own hash type ([`InfoSlot::seals`]): the plist read is
    /// the one each signed directory names, whichever the kernel ranks first.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn seals_info_plist(&self, info: &[u8]) -> bool {
        self.binds_info_plist()
            && self.0.iter().all(|(_, blob)| {
                info_slot_of(blob).is_some_and(|(kind, slot)| InfoSlot::of(kind, slot).seals(info))
            })
    }

    /// The Info slot of the CodeDirectory whose cdhash is `cdhash` (the one the kernel loaded for
    /// a process, [`crate::process::read_loaded_code_of`]): `None` where no directory has it or
    /// that one binds no plist ([`info_slot_of`]).
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn info_slot(&self, cdhash: [u8; 20]) -> Option<InfoSlot> {
        self.0
            .iter()
            .find(|(_, blob)| {
                blob.get(37).and_then(|kind| directory_hash(*kind, blob)) == Some(cdhash)
            })
            .and_then(|(_, blob)| info_slot_of(blob))
            .map(|(kind, slot)| InfoSlot::of(kind, slot))
    }

    /// Whether a CodeDirectory has the kernel's loaded cdhash. This is code identity, not
    /// signature validation: macOS still validates the image when it is executed.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn matches_loaded_hash(&self, expected: [u8; 20]) -> bool {
        self.0.iter().any(|(_, bytes)| {
            bytes.get(37).and_then(|kind| directory_hash(*kind, bytes)) == Some(expected)
        })
    }

    /// Each CodeDirectory, with its slice's CPU type, in the signature's order: what a digest
    /// of this identity reads.
    pub fn slices(&self) -> impl Iterator<Item = (u32, &[u8])> {
        self.0.iter().map(|(cpu, bytes)| (*cpu, bytes.as_slice()))
    }

    /// The code identity of the executable whose whole bytes are `bytes`.
    ///
    /// # Errors
    /// `InvalidData` for anything but a little-endian Mach-O (thin or universal) whose every
    /// slice carries an embedded signature with a CodeDirectory, within this reader's bounds;
    /// `UnexpectedEof` for one that ends early.
    pub fn of_bytes(bytes: &[u8]) -> io::Result<Self> {
        let mut source = bytes;
        Self::read(&mut source)
    }

    /// The code identity of the executable at `path`, reading its headers and signature only.
    /// A link at the end of `path` is refused.
    ///
    /// # Errors
    /// The open's error (`NotFound` for no file there), `InvalidInput` for no regular file, and
    /// [`Self::of_bytes`]'s.
    pub fn of_file(path: &Path) -> io::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let mut file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is no regular file", path.display()),
            ));
        }
        Self::read(&mut file)
    }

    fn read(source: &mut dyn At) -> io::Result<Self> {
        let head = source.at(0, 8)?;
        let mut directories = Vec::new();
        let mut budget = u64::from(MAX_DIRECTORY);
        match be(&head, 0) {
            magic @ (FAT_MAGIC | FAT_MAGIC_64) => {
                let wide = magic == FAT_MAGIC_64;
                let count = be(&head, 4);
                if count == 0 || count > MAX_SLICES {
                    return Err(bad("a universal executable names no slice, or too many"));
                }
                let entry = if wide { 32 } else { 20 };
                let table = source.at(8, entry * count as usize)?;
                for slice in table.chunks_exact(entry) {
                    let offset = if wide {
                        be64(slice, 8)
                    } else {
                        u64::from(be(slice, 8))
                    };
                    directories.extend(Self::slice(source, offset, &mut budget)?);
                }
            }
            _ => directories.extend(Self::slice(source, 0, &mut budget)?),
        }
        Ok(Self(directories))
    }

    /// The CodeDirectories of the thin Mach-O at `base`, each with the slice's CPU type, their
    /// bytes taken from `budget` (what is left of [`MAX_DIRECTORY`] for the whole file).
    fn slice(source: &mut dyn At, base: u64, budget: &mut u64) -> io::Result<Vec<(u32, Vec<u8>)>> {
        let size = match le(&source.at(base, 4)?, 0) {
            MH_MAGIC_64 => 32_u64,
            MH_MAGIC => 28,
            _ => return Err(bad("no little-endian Mach-O header")),
        };
        let header = source.at(base, 28)?;
        let cpu = le(&header, 4);
        let commands_len = le(&header, 20);
        if commands_len > MAX_COMMANDS {
            return Err(bad("the load commands are larger than any aterm's"));
        }
        let commands = source.at(plus(base, size)?, commands_len as usize)?;
        let mut at = 0_usize;
        let mut signature = None;
        while at + 8 <= commands.len() {
            let (cmd, cmdsize) = (le(&commands, at), le(&commands, at + 4) as usize);
            if cmdsize < 8
                || at
                    .checked_add(cmdsize)
                    .is_none_or(|end| end > commands.len())
            {
                return Err(bad("a load command runs past the others"));
            }
            if cmd == LC_CODE_SIGNATURE && cmdsize >= 16 {
                signature = Some((le(&commands, at + 8), le(&commands, at + 12)));
            }
            at += cmdsize;
        }
        let Some((offset, len)) = signature else {
            return Err(bad("the executable carries no code signature"));
        };
        let start = plus(base, u64::from(offset))?;
        let superblob = source.at(start, 12)?;
        if be(&superblob, 0) != EMBEDDED_SIGNATURE {
            return Err(bad("the code signature is no embedded signature"));
        }
        let count = be(&superblob, 8);
        if count > MAX_BLOBS {
            return Err(bad("the code signature names too many blobs"));
        }
        let index = source.at(plus(start, 12)?, 8 * count as usize)?;
        let mut found = Vec::new();
        for entry in index.as_chunks::<8>().0 {
            let (slot, blob) = (be(entry, 0), be(entry, 4));
            if !DIRECTORY_SLOTS.contains(&slot) {
                continue;
            }
            let blob_at = plus(start, u64::from(blob))?;
            let blob_head = source.at(blob_at, 8)?;
            let blob_len = be(&blob_head, 4);
            if be(&blob_head, 0) != CODE_DIRECTORY
                || !(8..=MAX_DIRECTORY).contains(&blob_len)
                || u64::from(blob) + u64::from(blob_len) > u64::from(len)
            {
                return Err(bad("a code directory slot holds no code directory"));
            }
            *budget = budget
                .checked_sub(u64::from(blob_len))
                .ok_or_else(|| bad("the code directories are larger than any aterm's"))?;
            found.push((cpu, source.at(blob_at, blob_len as usize)?));
        }
        if found.is_empty() {
            return Err(bad("the code signature has no code directory"));
        }
        Ok(found)
    }
}

/// The Info slot of the CodeDirectory `blob`: its hash type and the slot's hash. `None` where
/// the hash size is not 20, 32 or 48 bytes or disagrees with the hash type (SHA-1 and truncated
/// SHA-256 20, SHA-256 32, SHA-384 48), the directory names no special slot, the slot lies outside
/// the directory, or the slot is all zero (no plist bound).
fn info_slot_of(blob: &[u8]) -> Option<(u8, &[u8])> {
    let (size, kind) = (usize::from(*blob.get(36)?), *blob.get(37)?);
    if !matches!((kind, size), (1 | 3, 20) | (2, 32) | (4, 48)) || be(blob, 24) == 0 {
        return None;
    }
    let start = usize::try_from(be(blob, 16)).ok()?;
    let slot = blob.get(start.checked_sub(size)?..start)?;
    slot.iter().any(|b| *b != 0).then_some((kind, slot))
}

/// A CodeDirectory's Info slot ([`CodeDirectories::info_slot`]): the hash type it was made with,
/// and the hash it holds of the bundle's `Info.plist`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoSlot {
    kind: u8,
    hash: Vec<u8>,
}

impl InfoSlot {
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "slots are digested on macOS")
    )]
    fn of(kind: u8, slot: &[u8]) -> Self {
        Self {
            kind,
            hash: slot.to_vec(),
        }
    }

    /// Whether `info` is the plist this slot seals: its full digest in the slot's hash type,
    /// truncated to the slot's length as the signature truncates it, is the slot's hash.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn seals(&self, info: &[u8]) -> bool {
        digest(self.kind, info)
            .is_some_and(|full| full.get(..self.hash.len()) == Some(&self.hash[..]))
    }
}

/// The full digest of `bytes` in CodeDirectory hash type `kind` (SHA-1, SHA-256 for both its full
/// and truncated types, SHA-384), by CommonCrypto; `None` for another type.
#[cfg(target_os = "macos")]
fn digest(kind: u8, bytes: &[u8]) -> Option<Vec<u8>> {
    unsafe extern "C" {
        fn CC_SHA1(data: *const core::ffi::c_void, len: u32, out: *mut u8) -> *mut u8;
        fn CC_SHA256(data: *const core::ffi::c_void, len: u32, out: *mut u8) -> *mut u8;
        fn CC_SHA384(data: *const core::ffi::c_void, len: u32, out: *mut u8) -> *mut u8;
    }
    let len = u32::try_from(bytes.len()).ok()?;
    let mut digest = [0_u8; 48];
    // SAFETY: bytes holds len readable bytes, and digest holds the largest output (SHA-384).
    // Each CommonCrypto function finishes synchronously and retains neither pointer.
    let (written, size) = unsafe {
        match kind {
            1 => (CC_SHA1(bytes.as_ptr().cast(), len, digest.as_mut_ptr()), 20),
            2 | 3 => (
                CC_SHA256(bytes.as_ptr().cast(), len, digest.as_mut_ptr()),
                32,
            ),
            4 => (
                CC_SHA384(bytes.as_ptr().cast(), len, digest.as_mut_ptr()),
                48,
            ),
            _ => return None,
        }
    };
    (!written.is_null()).then(|| digest[..size].to_vec())
}

/// A CodeDirectory's cdhash: its [`digest`] in its own hash type, cut to 20 bytes.
#[cfg(target_os = "macos")]
fn directory_hash(kind: u8, bytes: &[u8]) -> Option<[u8; 20]> {
    digest(kind, bytes)?.get(..20)?.try_into().ok()
}

/// Who a signature says signed an executable ([`CodeDirectories::signer`]): its identifier and
/// its team, as the CodeDirectories state them. Read, not verified: a hint for whether the
/// running broker can adopt an update (§3), never a reason to trust code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signer {
    /// The signature's identifier (`com.aterm.aterm` for a release).
    pub identifier: String,
    /// The signing team's identifier.
    pub team: String,
}

/// The NUL-terminated string at `at` in `blob`, or `None` for none there.
fn c_string(blob: &[u8], at: u32) -> Option<String> {
    let rest = blob.get(usize::try_from(at).ok()?..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    std::str::from_utf8(&rest[..end]).ok().map(str::to_owned)
}

impl CodeDirectories {
    /// The one [`Signer`] every CodeDirectory names, or `None` for an ad-hoc or linker signature
    /// (no team), a directory too old to name a team, or directories that disagree.
    #[must_use]
    pub fn signer(&self) -> Option<Signer> {
        let mut signers = self.0.iter().map(|(_, blob)| {
            let (version, flags) = (be(blob, 8), be(blob, 12));
            if flags & (CS_ADHOC | CS_LINKER_SIGNED) != 0 || version < SUPPORTS_TEAM_ID {
                return None;
            }
            let team_at = be(blob, 48);
            let (identifier, team) = (c_string(blob, be(blob, 20))?, c_string(blob, team_at)?);
            (team_at != 0 && !identifier.is_empty() && !team.is_empty())
                .then_some(Signer { identifier, team })
        });
        let first = signers.next()??;
        signers.all(|s| s.as_ref() == Some(&first)).then_some(first)
    }
}

/// What [`read`] reads with: the files themselves ([`Disk`]), or a test's.
pub trait Files {
    /// The whole file at `path` (the plist, a bundle's `Info.plist`).
    ///
    /// # Errors
    /// The read's error; `NotFound` where nothing is there.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// The code identity of the executable at `path` ([`CodeDirectories`]).
    ///
    /// # Errors
    /// [`CodeDirectories::of_file`]'s, `NotFound` where nothing is there.
    fn code_directories(&self, path: &Path) -> io::Result<CodeDirectories>;

    /// Whether this machine runs as a virtual machine's guest (`aterm_sysprobe::vm_guest`), where
    /// a development build may own the broker ([`Part::Development`]); `None` where unread, which
    /// counts as a real Mac.
    fn vm_guest(&self) -> Option<bool>;
}

/// [`Files`] on this machine's disk.
pub struct Disk;

impl Files for Disk {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        use std::io::Read as _;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is no regular file", path.display()),
            ));
        }
        let mut bytes = Vec::new();
        file.take(PLIST_MAX + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > PLIST_MAX {
            return Err(bad("the file is longer than any plist this build writes"));
        }
        Ok(bytes)
    }

    fn code_directories(&self, path: &Path) -> io::Result<CodeDirectories> {
        CodeDirectories::of_file(path)
    }

    fn vm_guest(&self) -> Option<bool> {
        #[cfg(any(unix, windows))]
        {
            aterm_sysprobe::vm_guest()
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }
}

/// How one installed part reads against what this build would install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// As this build installs it: the image is this build's code; the plist is the one this build
    /// writes for a bundle, whichever bundle it names ([`read`]).
    Current,
    /// Not there.
    Missing,
    /// There, and not what this build installs: another signer's image, or a plist this build
    /// writes for no bundle.
    Differs,
    /// The image is an older build of this build's own non-ad-hoc [`Signer`] (the team named) of
    /// [`BUNDLE_ID`], or one whose number does not read while this build's does: the running
    /// broker adopts this build by itself once it asks ([`consider_for`];
    /// `docs/DESIGN-lan-broker-2026-11-05.md` §3), so a release update owes no password. It is
    /// an expectation, which [`super::adoption::expect`] bounds: one the broker did not meet
    /// reads [`Self::NotAdopted`].
    Adopting(String),
    /// The image is another build of this build's own [`Signer`] of [`BUNDLE_ID`] whose number
    /// (`image`) is at least this build's (`ours`): the broker keeps it, as it never moves to an
    /// older build, so this build neither owes nor asks for it, nor for the plist beside it (that
    /// newer build's, whatever it says), whatever launchd holds of its job
    /// ([`Reading::owed_with_job`]), and its elevation never publishes its own image over it: one
    /// launchd holds no job for is loaded in place (`aterm_machines`' LAN item).
    Kept {
        /// The signing team the image and this build both name.
        team: String,
        /// The image's build ([`build_number`]).
        image: u64,
        /// This build's.
        ours: u64,
    },
    /// An [`Self::Adopting`] image the running broker did not take up: past its bound
    /// ([`super::adoption::bound`]) after this account first saw it serve another build, it still
    /// served `broker` ([`super::adoption::expect`]). Owed: the next Mac password setup asks for
    /// its root steps, or a person's own `aterm init`, renews it, though none is asked for it
    /// alone where the broker serves this build's protocol
    /// (`aterm_machines::root_steps::Owed::LanBrokerAdoption`); one that speaks another serves
    /// this build nothing, so its renewal asks (`aterm_machines::root_steps::Owed::LanBroker`).
    /// `why` is the broker's own record of why it did not adopt this build's bundle
    /// ([`super::adoption::Refusal`]), where it wrote one for that bundle.
    NotAdopted {
        /// The signing team the image and this build both name.
        team: String,
        /// The build the broker's hello still names.
        broker: String,
        /// Whether the broker speaks this build's protocol, and so still serves its LAN traffic.
        serves: bool,
        /// Why the broker did not adopt this build's bundle, in its own words.
        why: Option<String>,
    },
    /// This build's executable states no team [`Signer`] of [`BUNDLE_ID`] (a development build:
    /// ad hoc, self-signed, or another identifier) and this is no VM guest ([`Files::vm_guest`]):
    /// it owns no broker, so neither part is owed, and its LAN goes by macOS's allowance for it
    /// (`super::direct`) where one stands. Nothing else is read.
    Development,
    /// There, and it could not be read, or this build's own number could not be: not owed by
    /// this reading (an unreadable reading never asks for a password), the words say why.
    Unread(String),
}

impl Part {
    /// Whether the elevation owes this part.
    #[must_use]
    pub const fn owed(&self) -> bool {
        matches!(
            self,
            Self::Missing | Self::Differs | Self::NotAdopted { .. }
        )
    }

    /// The part's state in a few words.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Current => "current".to_owned(),
            Self::Missing => "missing".to_owned(),
            Self::Differs => "another build's".to_owned(),
            Self::Adopting(team) => {
                format!("an older build's of team {team}, which the broker adopts by itself")
            }
            Self::Kept { team, image, ours } => format!(
                "build {image} of team {team}, which the broker keeps over this build {ours}: it \
                 never moves to an older build"
            ),
            Self::NotAdopted {
                team,
                broker,
                serves,
                why,
            } => format!(
                "an older build's of team {team}, which the running broker did not adopt in its \
                 looks: it still serves {broker}{} ({})",
                if *serves {
                    ""
                } else {
                    ", on another protocol than this aterm's, so aterm's LAN traffic is down \
                     until setup's Mac password renews it"
                },
                why.as_deref().unwrap_or("it recorded no reason"),
            ),
            Self::Development => {
                "left alone: a development build runs the broker only in a VM guest".to_owned()
            }
            Self::Unread(why) => format!("unread ({why})"),
        }
    }
}

/// The broker's install as an account reads it ([`read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The root-owned clone ([`super::IMAGE_BUNDLE`]) against this build's code.
    pub image: Part,
    /// [`super::PLIST_PATH`] against [`plist_text`] of any bundle.
    pub plist: Part,
}

impl Reading {
    /// Whether setup's one elevation owes the broker: either part is missing or differs. Beside
    /// an image the broker keeps over this build ([`Part::Kept`]) the plist is that newer build's,
    /// whatever it says, and this build owes it nothing: an older build's elevation never rewrites
    /// what a newer one wrote, and two builds that each owed the other's plist would ask in turn.
    #[must_use]
    pub const fn owed(&self) -> bool {
        self.image.owed() || (self.plist.owed() && !matches!(self.image, Part::Kept { .. }))
    }

    /// Whether setup owes a file repair or a known-stopped service, and the job is not one a
    /// person switched off. A running older signed image adopts its update itself. A newer one
    /// this build keeps ([`Part::Kept`]) is never owed, whatever launchd holds of its job: a job
    /// it holds with no pid is stopped or between two of its starts, and one it holds none of may
    /// be inside the newer build's own `bootout` and `bootstrap`, which a read cannot tell apart,
    /// and no password is asked of this account for a broker it does not own. An elevation that
    /// runs loads it in place where launchd holds none. An unreadable file or job never creates
    /// service debt. `job` is read only when the files can already serve this build or adopt it
    /// (the plist current).
    ///
    /// `switched_off` is read only when something would be owed: a job launchd lists switched off
    /// (`Some(true)`: its background item turned off in Login Items, or `launchctl disable`)
    /// stays the person's, so it owes nothing, files current or not, and no password is asked to
    /// undo their choice; putting it back is theirs, as it is for the node job
    /// (`aterm_agent::node_service::read_for_sync_with`). Unlike the node job, one still running
    /// owes nothing either: a repair boots it out, and launchd refuses to start again a job it
    /// lists switched off. Where launchd cannot say (`None`), the debt stands as it reads.
    #[must_use]
    pub fn owed_with_job(
        &self,
        job: impl FnOnce() -> Option<JobState>,
        switched_off: impl FnOnce() -> Option<bool>,
    ) -> bool {
        let owed = self.owed()
            || match self.image {
                Part::Current | Part::Adopting(_) => {
                    self.plist == Part::Current
                        && matches!(job(), Some(JobState::Idle | JobState::Unloaded))
                }
                _ => false,
            };
        owed && switched_off() != Some(true)
    }

    /// Whether both parts are as this build installs them.
    #[must_use]
    pub fn current(&self) -> bool {
        self.image == Part::Current && self.plist == Part::Current
    }

    /// Whether the install stays as it is for this build: the image is one the broker keeps over
    /// it ([`Part::Kept`]), whose plist is that newer build's whatever it says, or the image is
    /// this build's or none of its to own ([`Part::Development`]) and the plist is in place or
    /// none of its to own.
    #[must_use]
    pub fn in_place(&self) -> bool {
        matches!(self.image, Part::Kept { .. })
            || (matches!(self.image, Part::Current | Part::Development)
                && matches!(self.plist, Part::Current | Part::Development))
    }

    /// The reading in one line: `image <state>, plist <state>`.
    #[must_use]
    pub fn words(&self) -> String {
        format!("image {}, plist {}", self.image.words(), self.plist.words())
    }
}

/// The image ([`super::IMAGE_EXECUTABLE`]) against this build's code `ours`, of the bundle at
/// `source`: the same code; another build of this build's own [`Signer`] of [`BUNDLE_ID`], older
/// (or of no number while this build's reads, [`Part::Adopting`]) or at least as new
/// ([`Part::Kept`]), by [`build_of`] each bundle; this build's own number unread
/// ([`Part::Unread`]); another's; missing or unread.
fn image_against(files: &dyn Files, ours: &CodeDirectories, source: &Path) -> Part {
    match files.code_directories(Path::new(super::IMAGE_EXECUTABLE)) {
        Ok(image) if image == *ours => Part::Current,
        Ok(image) => match (image.signer(), ours.signer()) {
            (Some(image), Some(ours)) if image == ours && ours.identifier == BUNDLE_ID => {
                let Some(build) = build_of(files, source) else {
                    return Part::Unread(format!(
                        "{} states no build",
                        source.join(BUNDLE_INFO).display()
                    ));
                };
                match build_of(files, Path::new(super::IMAGE_BUNDLE)) {
                    Some(kept) if kept >= build => Part::Kept {
                        team: ours.team,
                        image: kept,
                        ours: build,
                    },
                    _ => Part::Adopting(ours.team),
                }
            }
            _ => Part::Differs,
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Part::Missing,
        Err(e) => Part::Unread(format!("{}: {e}", super::IMAGE_EXECUTABLE)),
    }
}

/// The bundle a job's plist names as its `--source`: `Some` only where `plist` is, byte for
/// byte, the plist this build writes for that bundle ([`plist_text`]) and the path is absolute.
fn named_source(plist: &[u8]) -> Option<PathBuf> {
    const BEFORE: &str = "<string>--source</string>\n\t\t<string>";
    let text = std::str::from_utf8(plist).ok()?;
    let rest = &text[text.find(BEFORE)? + BEFORE.len()..];
    let mut named = rest[..rest.find("</string>")?].to_owned();
    // `&amp;` last: no other entity's text may come out of an escaped ampersand.
    for (entity, c) in [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&apos;", "'"),
        ("&amp;", "&"),
    ] {
        named = named.replace(entity, c);
    }
    let source = PathBuf::from(named);
    (source.is_absolute() && plist == plist_text(&source).as_bytes()).then_some(source)
}

/// Read the broker's install against `source`, the app bundle this build runs from, as an account
/// decides whether to ask for it and as the elevation decides what it installs. A build whose
/// executable states no team [`Signer`] of [`BUNDLE_ID`] reads [`Part::Development`] for both
/// parts outside a VM guest, and nothing else is read. Otherwise the clone's code is read against
/// the source executable's ([`image_against`]), and the plist is in place where it is byte for
/// byte the one this build writes for any absolute bundle (`named_source`): the broker adopts
/// every strictly newer team build that asks, so which bundle the job names leaves nothing owed,
/// and a job that names a bundle since deleted, or an older one, pins nothing.
///
/// # Errors
/// The source's own code could not be read (there is nothing to compare the clone with), in one
/// line.
pub fn read(files: &dyn Files, source: &Path) -> Result<Reading, String> {
    let ours_path = source.join(BUNDLE_EXECUTABLE);
    let ours = files
        .code_directories(&ours_path)
        .map_err(|e| format!("{}: {e}", ours_path.display()))?;
    if ours
        .signer()
        .is_none_or(|signer| signer.identifier != BUNDLE_ID)
        && files.vm_guest() != Some(true)
    {
        return Ok(Reading {
            image: Part::Development,
            plist: Part::Development,
        });
    }
    let image = image_against(files, &ours, source);
    let plist = match files.read(Path::new(super::PLIST_PATH)) {
        Ok(bytes) if named_source(&bytes).is_some() => Part::Current,
        Ok(_) => Part::Differs,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Part::Missing,
        Err(e) => Part::Unread(format!("{}: {e}", super::PLIST_PATH)),
    };
    Ok(Reading { image, plist })
}

/// This process's own source: the app bundle its canonical executable runs from. `None` off
/// macOS (no Local Network privacy, so no broker), outside a bundle, or when the executable
/// cannot name itself.
#[must_use]
pub fn source_here() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    source_bundle(&exe)
}

/// [`read`] on this machine for this process's [`source_here`]: `None` where there is no source.
/// A reading that finds the image adopting asks the broker to consider this bundle
/// ([`consider_for`]).
#[must_use]
pub fn read_here() -> Option<(PathBuf, Result<Reading, String>)> {
    let source = source_here()?;
    let reading = read(&Disk, &source);
    if let Ok(found) = &reading {
        consider_for(found);
    }
    Some((source, reading))
}

/// Whether setup's one elevation owes this Mac the broker, using this process's file reading
/// and the injected service readings ([`Reading::owed_with_job`]). `false` with no source
/// and for a file reading that failed, so no password is asked for want of a reading.
#[must_use]
pub fn owed_here(
    job: impl FnOnce() -> Option<JobState>,
    switched_off: impl FnOnce() -> Option<bool>,
) -> bool {
    read_here().is_some_and(|(_, reading)| {
        reading.is_ok_and(|reading| reading.owed_with_job(job, switched_off))
    })
}

impl Reading {
    /// Whether the broker serves this build's requests as the files read and launchd runs it,
    /// with no password: the plist as this build writes it, the image this build's own code
    /// ([`Part::Current`]), and launchd running the job (`job`, read only where both are so). Any
    /// other build's image is another build's broker: one still to be adopted
    /// ([`Part::Adopting`], [`Part::NotAdopted`]) may predate a request this build makes, and one
    /// the broker keeps over this build ([`Part::Kept`]) is a later build's, whose requests this
    /// build cannot know, so none of them serves it. A reading that serves still leaves the
    /// broker's own answer to each request; one that does not is no promise it would refuse. It
    /// is what an automatic ask reads before it owes setup's password a step the broker does
    /// (`aterm_machines::root_steps::Owed::Spotlight`).
    #[must_use]
    pub fn serves_with(&self, job: impl FnOnce() -> Option<JobState>) -> bool {
        self.plist == Part::Current
            && self.image == Part::Current
            && job() == Some(JobState::Running)
    }
}

/// [`Reading::serves_with`] on this machine for this process's [`source_here`], read without
/// root: `false` with no source and for a file reading that failed, so a step the broker does is
/// still asked of setup's password where nothing says the broker serves.
#[must_use]
pub fn serves_this_build_here(job: impl FnOnce() -> Option<JobState>) -> bool {
    read_here().is_some_and(|(_, reading)| reading.is_ok_and(|reading| reading.serves_with(job)))
}

/// Whether this process is a development build on a real Mac: the code the kernel loaded for it
/// states no team [`Signer`] of [`BUNDLE_ID`] ([`development_at`], read at the path the kernel
/// records for its image, `proc_pidpath`, against the cdhash it loaded, so a link a build
/// retargets since this process started changes nothing) and this is no VM guest (where a
/// development build owns its own broker, [`Part::Development`]), read once; off macOS, none is.
/// Such a process owns no broker: its LAN uses take its direct path (`super::direct`) first,
/// where macOS allowed its code, and dial the installed broker only where none stands
/// ([`super::broker`]). A test's thread may say instead ([`super::Seam::development`]).
#[must_use]
pub fn development_here() -> bool {
    #[cfg(any(test, debug_assertions))]
    if let Some(development) = super::seam().development {
        return development;
    }
    static DEVELOPMENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DEVELOPMENT.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let loaded = {
            let pid = i32::try_from(std::process::id()).unwrap_or(0);
            crate::process::read_process_path(pid).is_some_and(|exe| {
                development_at(&exe, crate::process::read_code_directory_hash(pid))
            })
        };
        #[cfg(not(target_os = "macos"))]
        let loaded = false;
        loaded && Disk.vm_guest() != Some(true)
    })
}

/// Whether the code a process loaded, the cdhash `loaded` the kernel names for it, is a
/// development build's: the file `exe` names (every link on the way followed) holds that code's
/// CodeDirectory, so what is read is the code the process runs and not whatever a name holds now,
/// and states no team [`Signer`] of [`BUNDLE_ID`]. Code whose cdhash did not read, a file that no
/// longer holds it, and a signature that does not read are not one: such a process stands on the
/// broker's answer, as a release does. Off macOS, none is.
#[must_use]
pub fn development_at(exe: &Path, loaded: Option<[u8; 20]>) -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some(loaded) = loaded else {
            return false;
        };
        exe.canonicalize()
            .and_then(|exe| CodeDirectories::of_file(&exe))
            .is_ok_and(|code| {
                code.matches_loaded_hash(loaded)
                    && code
                        .signer()
                        .is_none_or(|signer| signer.identifier != BUNDLE_ID)
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (exe, loaded);
        false
    }
}

/// What decides whether a process asks the broker to consider its bundle ([`Ask::begin`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Asking<'a> {
    /// The process is a Cargo test binary ([`crate::test_binary::scratch_root`]).
    pub(crate) test_binary: bool,
    /// The process runs in a scratch world ([`crate::world::MARKER`] is set).
    pub(crate) world: bool,
    /// The process runs from an app bundle ([`source_here`]).
    pub(crate) bundled: bool,
    /// Its install's reading ([`read`]).
    pub(crate) reading: &'a Reading,
}

impl Asking<'_> {
    /// Whether to ask: not a test binary, not in a scratch world, run from a bundle, and the image
    /// read [`Part::Adopting`]: so a scratch world's qualification, an older build beside a newer
    /// broker ([`Part::Kept`]), a build the broker already serves and a development build ask
    /// nothing.
    const fn asks(&self) -> bool {
        !self.test_binary
            && !self.world
            && self.bundled
            && matches!(self.reading.image, Part::Adopting(_))
    }
}

/// Which broker answers at a socket: its file's device and inode, read with no link followed. A
/// broker binds its socket anew each time it starts, so another file is another broker, which
/// keeps none of the asks the one before it heard: the broker keeps its askers in memory alone.
pub(crate) type Generation = (u64, u64);

/// The [`Generation`] of the broker this thread's client dials (`super::dialled_socket`): `None`
/// where its socket does not read.
fn socket_generation() -> Option<Generation> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::symlink_metadata(super::dialled_socket())
            .ok()
            .map(|meta| (meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// How long an ask a broker heard stands ([`Ask`]): five minutes, past which a process whose image
/// still reads adopting asks that broker again. The broker keeps a bounded list of asks
/// (`aterm-lan`'s `askers`), and an ask it heard may go from it before its bundle is adopted; one
/// ask in five minutes is one admission check of this process's code in that time.
pub(crate) const ASK_AGAIN: Duration = Duration::from_secs(5 * 60);

/// What a process knows of its asks ([`Ask`]).
#[derive(Debug, Default)]
struct Asks {
    /// The broker an ask last reached (it admitted this process, and the request was written),
    /// and when.
    reached: Option<(Generation, Instant)>,
    /// An ask is on its way.
    sending: bool,
}

/// A process's asks of the broker to consider its bundle ([`consider_for`]): at most one on its
/// way; one more for each broker that did not hear one, as an ask counts only once a broker heard
/// it and a broker that restarted (after an adoption, an elevation or a crash) heard none; and one
/// more of the same broker each [`ASK_AGAIN`].
pub(crate) struct Ask(std::sync::Mutex<Asks>);

impl Ask {
    pub(crate) const fn new() -> Self {
        Self(std::sync::Mutex::new(Asks {
            reached: None,
            sending: false,
        }))
    }

    fn asks(&self) -> std::sync::MutexGuard<'_, Asks> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether to start an ask at `at` where `asking` asks ([`Asking::asks`]) and the broker is of
    /// generation `now`: none is on its way, and none reached that broker in the [`ASK_AGAIN`]
    /// before `at` (a socket that did not read is no broker an ask reached). Starting marks one on
    /// its way, which [`Self::end`] ends.
    pub(crate) fn begin(&self, asking: &Asking<'_>, now: Option<Generation>, at: Instant) -> bool {
        if !asking.asks() {
            return false;
        }
        let mut asks = self.asks();
        let heard = asks.reached.is_some_and(|(reached, when)| {
            Some(reached) == now && at.saturating_duration_since(when) < ASK_AGAIN
        });
        if asks.sending || heard {
            return false;
        }
        asks.sending = true;
        true
    }

    /// The end at `at` of the ask [`Self::begin`] started: `reached`, the broker that heard it,
    /// `None` where none did, which leaves the next reading to ask again.
    pub(crate) fn end(&self, reached: Option<Generation>, at: Instant) {
        let mut asks = self.asks();
        asks.sending = false;
        if let Some(reached) = reached {
            asks.reached = Some((reached, at));
        }
    }
}

/// The most tries of one ask ([`ask_until_heard`]): about ten minutes of them, past which the
/// next reading or served LAN use asks again.
pub(crate) const ASK_TRIES: u32 = 8;

/// How long after its `n`th failed try an ask is tried again: one of the broker's looks
/// ([`super::ADOPTION_LOOK`]), doubling, at most five minutes: a broker restarting, at its
/// connection bound or waiting on `trustd` answers again within that.
pub(crate) fn ask_again_after(n: u32) -> Duration {
    let factor = 1u32 << n.saturating_sub(1).min(31);
    super::ADOPTION_LOOK
        .saturating_mul(factor)
        .min(Duration::from_secs(5 * 60))
}

/// One ask, tried until a broker hears it: `send` tries it (`true` where the broker admitted this
/// process and the request was written), each try at the broker `generation` then names, `wait`
/// between tries ([`ask_again_after`]), at most [`ASK_TRIES`], ending early where `still` says
/// this process no longer asks (its image no longer reads adopting). The broker that heard it,
/// `None` where none did.
pub(crate) fn ask_until_heard(
    mut send: impl FnMut() -> bool,
    generation: impl Fn() -> Option<Generation>,
    mut wait: impl FnMut(Duration),
    still: impl Fn() -> bool,
) -> Option<Generation> {
    for n in 1..=ASK_TRIES {
        let at = generation();
        if send() {
            return at;
        }
        if n == ASK_TRIES {
            break;
        }
        wait(ask_again_after(n));
        if !still() {
            return None;
        }
    }
    None
}

/// This process's asks ([`Ask`]).
static ASKED: Ask = Ask::new();

/// Ask the installed root broker to consider the bundle this process runs from
/// ([`super::wire::Request::Consider`]), where `reading` (this process's install, [`read`]) finds
/// the image adopting, this is no test binary, and no scratch world ([`crate::world::MARKER`]):
/// the broker then authenticates the bundle by the code the kernel loaded for this process and
/// adopts it only through a root-owned clone that passes its strict check and states a strictly
/// newer build. The ask goes on a detached thread, tried until a broker hears it
/// ([`ask_until_heard`]), and is made again of a broker that heard it only after [`ASK_AGAIN`]
/// ([`Ask`]); what the broker then does is not read (one that does not know the request closes).
pub fn consider_for(reading: &Reading) {
    let asking = Asking {
        test_binary: crate::test_binary::scratch_root().is_some(),
        world: std::env::var_os(crate::world::MARKER).is_some(),
        bundled: source_here().is_some(),
        reading,
    };
    if !ASKED.begin(&asking, socket_generation(), Instant::now()) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("lan-consider".into())
        .spawn(|| {
            let still = || {
                source_here().is_some_and(|source| {
                    read(&Disk, &source)
                        .is_ok_and(|reading| matches!(reading.image, Part::Adopting(_)))
                })
            };
            let reached = ask_until_heard(
                || super::broker::consider().is_ok(),
                socket_generation,
                std::thread::sleep,
                still,
            );
            ASKED.end(reached, Instant::now());
        });
    if spawned.is_err() {
        ASKED.end(None, Instant::now());
    }
}

/// [`consider_for`] this process's own reading ([`read`] of [`source_here`]), looked at once per
/// broker ([`Generation`]) and [`ASK_AGAIN`], on a detached thread: what each LAN use the
/// installed root broker served calls (`super::broker`), so a build that never reads its install
/// still asks, asks again of a broker that restarted, and asks again while its image still reads
/// adopting.
pub fn consider_here() {
    static LOOKED: std::sync::Mutex<Option<(Generation, Instant)>> = std::sync::Mutex::new(None);
    if crate::test_binary::scratch_root().is_some()
        || std::env::var_os(crate::world::MARKER).is_some()
    {
        return;
    }
    let Some(now) = socket_generation() else {
        return;
    };
    {
        let mut looked = LOOKED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let at = Instant::now();
        if looked.is_some_and(|(generation, when)| {
            generation == now && at.saturating_duration_since(when) < ASK_AGAIN
        }) {
            return;
        }
        *looked = Some((now, at));
    }
    let _ = std::thread::Builder::new()
        .name("lan-consider".into())
        .spawn(|| {
            let _ = read_here();
        });
}

/// A test's thin 64-bit Mach-O of CPU type `cpu` whose embedded signature carries `directories`
/// (each wrapped as a CodeDirectory blob), for readers' tests here and in `aterm-machines`.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn test_image(cpu: u32, directories: &[&[u8]]) -> Vec<u8> {
    let commands_len = 16_u32;
    let header_len = 32_u32;
    let signature_at = header_len + commands_len;
    let index_len = 12 + 8 * directories.len() as u32;
    let mut blobs = Vec::new();
    let mut index = Vec::new();
    for (n, body) in directories.iter().enumerate() {
        let slot = if n == 0 { 0 } else { 0x1000 + n as u32 - 1 };
        index.extend_from_slice(&slot.to_be_bytes());
        index.extend_from_slice(&(index_len + blobs.len() as u32).to_be_bytes());
        blobs.extend_from_slice(&CODE_DIRECTORY.to_be_bytes());
        blobs.extend_from_slice(&(8 + body.len() as u32).to_be_bytes());
        blobs.extend_from_slice(body);
    }
    let signature_len = index_len + blobs.len() as u32;
    let mut out = Vec::new();
    for word in [MH_MAGIC_64, cpu, 0, 2, 1, commands_len, 0, 0] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    for word in [LC_CODE_SIGNATURE, commands_len, signature_at, signature_len] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.extend_from_slice(&EMBEDDED_SIGNATURE.to_be_bytes());
    out.extend_from_slice(&signature_len.to_be_bytes());
    out.extend_from_slice(&(directories.len() as u32).to_be_bytes());
    out.extend_from_slice(&index);
    out.extend_from_slice(&blobs);
    out
}

/// A test's CodeDirectory body (what follows its magic and length): `flags`, naming `identifier`
/// and, at a nonzero `teamOffset`, `team`, at the team-supporting version; 32-byte SHA-256
/// hashes; one code slot of `page`; and where `info` is given one special slot, the Info slot,
/// holding its SHA-256, as a signature that binds that `Info.plist` does. For readers' tests here,
/// in `aterm-lan` and in `aterm-machines`.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn test_directory(
    identifier: &str,
    team: Option<&str>,
    flags: u32,
    page: u8,
    info: Option<&[u8]>,
) -> Vec<u8> {
    let ident_at = 52_u32;
    let team_at = ident_at + identifier.len() as u32 + 1;
    let strings_end = team.map_or(team_at, |team| team_at + team.len() as u32 + 1);
    let special = u32::from(info.is_some());
    let hash_at = strings_end + 32 * special;
    // hashSize 32, hashType 2 (SHA-256), no platform, 4 KiB pages.
    let sizes = (32_u32 << 24) | (2 << 16) | 12;
    let mut body = Vec::new();
    for word in [
        SUPPORTS_TEAM_ID + 0x200,
        flags,
        hash_at,
        ident_at,
        special,
        1,
        0,
        sizes,
        0,
        0,
        team.map_or(0, |_| team_at),
    ] {
        body.extend_from_slice(&word.to_be_bytes());
    }
    body.extend_from_slice(identifier.as_bytes());
    body.push(0);
    if let Some(team) = team {
        body.extend_from_slice(team.as_bytes());
        body.push(0);
    }
    if let Some(info) = info {
        body.extend_from_slice(&aterm_digest::Sha256::digest(info));
    }
    body.extend_from_slice(&[page; 32]);
    body
}

/// A test's release executable: one CodeDirectory of team `team` naming [`BUNDLE_ID`], its code
/// `page`, binding `info` ([`test_directory`]), as [`test_image`] wraps it for this machine's
/// architecture.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn test_release(team: &str, page: u8, info: &[u8]) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    let cpu = 0x0100_0007;
    #[cfg(not(target_arch = "x86_64"))]
    let cpu = 0x0100_000c;
    test_image(
        cpu,
        &[&test_directory(BUNDLE_ID, Some(team), 0, page, Some(info))],
    )
}

/// The cdhash the kernel would load for a test's executable `exe` ([`test_release`]): its first
/// CodeDirectory's, as an asker's record carries it.
#[cfg(all(target_os = "macos", any(test, debug_assertions)))]
#[must_use]
pub fn test_cdhash(exe: &[u8]) -> Option<[u8; 20]> {
    let code = CodeDirectories::of_bytes(exe).ok()?;
    let (_, blob) = code.0.first()?;
    directory_hash(*blob.get(37)?, blob)
}

/// A test's `Info.plist` stating build `build` as the release stamps it, `CFBundleVersion` among
/// the other keys ([`build_number`]).
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn test_info(build: u64) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
         \t<key>CFBundleIdentifier</key>\n\t<string>{BUNDLE_ID}</string>\n\
         \t<key>CFBundleVersion</key>\n\t<string>{build}</string>\n</dict>\n</plist>\n"
    )
    .into_bytes()
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
