// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What setup's one elevation installs for the LAN broker, and how an account with no root reads
//! whether that install is current (`docs/DESIGN-lan-broker-2026-11-05.md` §3, §5).
//!
//! The elevation clones the app bundle it ran from (the **source**) to [`super::IMAGE_BUNDLE`],
//! root-owned, and writes [`super::PLIST_PATH`] ([`plist_text`]), whose job runs the clone's
//! executable as `aterm lan serve --source <source>` ([`program_arguments`]). The install is owed
//! when the clone is missing, when its code differs from the source's (unless both name one
//! signing team, which the running broker adopts by itself: [`Part::Adopting`]), or when the plist
//! differs from the one this build writes. Each of the three is readable without root: the image is
//! `root:wheel` with read bits for everyone, and so is the plist.
//!
//! A Mac has one broker, and a team-signed one admits every client of its team (`aterm-lan`'s
//! `server`). So a job that names another bundle asks no password where that bundle is a real
//! one of this build's own signer and the image is its code, or adoptable by it ([`read`]): two
//! copies of a release (the installed app and a release cut's bundle) that each read only their
//! own name would each take the job from the other, at the price of the Mac password every
//! time. The elevation itself is unchanged: it installs the bundle it ran from and no other,
//! and judges the install against that bundle alone ([`read_own`]), so one that runs for any
//! reason, `sudo aterm machines here apply --plain` included, takes the job for its own bundle.
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

/// The bundle identifier the broker's job is attributed to (`AssociatedBundleIdentifiers`), so
/// macOS's background-item notice names aterm, not a file.
pub const BUNDLE_ID: &str = "com.aterm.aterm";

/// A bundle's executable, relative to the bundle: what launchd runs in the clone
/// ([`super::IMAGE_EXECUTABLE`]) and what the clone is compared against in the source.
pub const BUNDLE_EXECUTABLE: &str = "Contents/MacOS/aterm";

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

#[cfg(target_os = "macos")]
fn directory_hash(kind: u8, bytes: &[u8]) -> Option<[u8; 20]> {
    unsafe extern "C" {
        fn CC_SHA1(data: *const core::ffi::c_void, len: u32, out: *mut u8) -> *mut u8;
        fn CC_SHA256(data: *const core::ffi::c_void, len: u32, out: *mut u8) -> *mut u8;
        fn CC_SHA384(data: *const core::ffi::c_void, len: u32, out: *mut u8) -> *mut u8;
    }
    let len = u32::try_from(bytes.len()).ok()?;
    let mut digest = [0_u8; 48];
    // SAFETY: bytes holds len readable bytes, and digest holds the largest output (SHA-384).
    // Each CommonCrypto function finishes synchronously and retains neither pointer.
    let written = unsafe {
        match kind {
            1 => CC_SHA1(bytes.as_ptr().cast(), len, digest.as_mut_ptr()),
            2 | 3 => CC_SHA256(bytes.as_ptr().cast(), len, digest.as_mut_ptr()),
            4 => CC_SHA384(bytes.as_ptr().cast(), len, digest.as_mut_ptr()),
            _ => return None,
        }
    };
    (!written.is_null()).then(|| digest[..20].try_into().expect("fixed cdhash length"))
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
    /// The whole file at `path` (the plist).
    ///
    /// # Errors
    /// The read's error; `NotFound` where nothing is there.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// The code identity of the executable at `path` ([`CodeDirectories`]).
    ///
    /// # Errors
    /// [`CodeDirectories::of_file`]'s, `NotFound` where nothing is there.
    fn code_directories(&self, path: &Path) -> io::Result<CodeDirectories>;

    /// `path` with every link in it resolved, as [`std::fs::canonicalize`] answers.
    ///
    /// # Errors
    /// The resolution's error; `NotFound` where nothing is there.
    fn canonical(&self, path: &Path) -> io::Result<PathBuf>;

    /// The uid that owns the file at `path` itself (a link is not followed).
    ///
    /// # Errors
    /// The stat's error.
    fn owner(&self, path: &Path) -> io::Result<u32>;
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

    fn canonical(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn owner(&self, path: &Path) -> io::Result<u32> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::symlink_metadata(path).map(|meta| meta.uid())
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}

/// How one installed part reads against what this build would install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// As the bundle the reading is of installs it: this build's, or in a reading of a job
    /// another bundle of this build's signer holds ([`Reading::held_by`]), that bundle's.
    Current,
    /// Not there.
    Missing,
    /// There, and not what this build installs: another build's image, or a plist that is
    /// neither this bundle's nor one a bundle of this build's signer holds ([`read`]).
    Differs,
    /// The image is another build's, and it and the source state one non-ad-hoc [`Signer`]
    /// (the team named) with [`BUNDLE_ID`]: the running broker adopts the source by itself
    /// (`docs/DESIGN-lan-broker-2026-11-05.md` §3), so a release update owes no password. Not
    /// current either: an elevation that runs for any reason renews it. The source is the
    /// bundle the job names: in a held reading ([`Reading::held_by`]) the holder, not the reader.
    Adopting(String),
    /// There, and it could not be read: not owed by this reading (an unreadable reading never
    /// asks for a password), the words say why.
    Unread(String),
}

impl Part {
    /// Whether the elevation owes this part.
    #[must_use]
    pub const fn owed(&self) -> bool {
        matches!(self, Self::Missing | Self::Differs)
    }

    /// The part's state in a few words.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Current => "current".to_owned(),
            Self::Missing => "missing".to_owned(),
            Self::Differs => "another build's".to_owned(),
            Self::Adopting(team) => {
                format!("another build's of team {team}, which the broker adopts by itself")
            }
            Self::Unread(why) => format!("unread ({why})"),
        }
    }
}

/// The broker's install as an account reads it ([`read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The root-owned clone ([`super::IMAGE_BUNDLE`]) against the code of the bundle the reading
    /// is of: this build's, or the holder's ([`Self::held_by`]).
    pub image: Part,
    /// [`super::PLIST_PATH`] against [`plist_text`] of that bundle.
    pub plist: Part,
    /// The other bundle of this build's signer that holds the job, where the reading is of its
    /// install ([`read`]); `None` for a reading against this build's own bundle.
    pub held_by: Option<PathBuf>,
}

impl Reading {
    /// Whether setup's one elevation owes the broker: either part is missing or differs.
    #[must_use]
    pub const fn owed(&self) -> bool {
        self.image.owed() || self.plist.owed()
    }

    /// Whether setup owes a file repair or a known-stopped service, and the job is not one a
    /// person switched off. A running older signed image adopts its update itself. An unreadable
    /// file or job never creates service debt. `running` is read only when both files can
    /// already serve this build or adopt it.
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
        running: impl FnOnce() -> Option<bool>,
        switched_off: impl FnOnce() -> Option<bool>,
    ) -> bool {
        let owed = self.owed()
            || (matches!(self.image, Part::Current | Part::Adopting(_))
                && self.plist == Part::Current
                && running() == Some(false));
        owed && switched_off() != Some(true)
    }

    /// Whether both parts are as the bundle the reading is of installs them: this build's, or
    /// the holder's ([`Self::held_by`]).
    #[must_use]
    pub fn current(&self) -> bool {
        self.image == Part::Current && self.plist == Part::Current
    }

    /// The reading in one line: `image <state>, plist <state>`, and the holder where another
    /// bundle holds the job.
    #[must_use]
    pub fn words(&self) -> String {
        let parts = format!("image {}, plist {}", self.image.words(), self.plist.words());
        match &self.held_by {
            Some(holder) => format!(
                "{parts}, held by another copy of this team at {}",
                holder.display()
            ),
            None => parts,
        }
    }
}

/// The image ([`super::IMAGE_EXECUTABLE`]) against a source executable's code: the same code,
/// another build's of the source's own team with [`BUNDLE_ID`] (which the broker adopts by
/// itself), another's, missing or unread.
fn image_against(files: &dyn Files, source: &CodeDirectories) -> Part {
    match files.code_directories(Path::new(super::IMAGE_EXECUTABLE)) {
        Ok(image) if image == *source => Part::Current,
        Ok(image) => match (image.signer(), source.signer()) {
            (Some(image), Some(source)) if image == source && source.identifier == BUNDLE_ID => {
                Part::Adopting(source.team)
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

/// The reading of a job whose `plist` names another bundle, where that bundle holds the broker
/// for this build too ([`read`]); `None` where it does not, and the reading against this build's
/// own bundle `running` stands (`ours` is its code, `own_image` that reading's image).
fn held_by_another(
    files: &dyn Files,
    running: &Path,
    ours: &CodeDirectories,
    own_image: &Part,
    plist: &[u8],
) -> Option<Reading> {
    // An image that is missing or not this signer's is owed whoever the job names: it is no
    // code of a holder of this signer, and none adopts it.
    if own_image.owed() {
        return None;
    }
    let signer = ours.signer().filter(|s| s.identifier == BUNDLE_ID)?;
    let source = named_source(plist)?;
    let exe = source.join(BUNDLE_EXECUTABLE);
    // Root may read a bundle this account may not: a reading that then asked for the password
    // would ask at every launch, for an install the root half may leave as it is.
    let unread = |e: &io::Error| {
        let why = format!("{}: {e}", exe.display());
        Some(Reading {
            image: Part::Unread(why.clone()),
            plist: Part::Unread(why),
            held_by: None,
        })
    };
    let denied = |e: &io::Error| e.kind() == io::ErrorKind::PermissionDenied;
    // No link anywhere on the way to the executable: the broker clones none, and a linked
    // folder would lend the holder another bundle's code.
    match files.canonical(&exe) {
        Ok(real) if real == exe => {}
        Err(e) if denied(&e) => return unread(&e),
        _ => return None,
    }
    let theirs = match files.code_directories(&exe) {
        Ok(theirs) => theirs,
        Err(e) if denied(&e) => return unread(&e),
        Err(_) => return None,
    };
    if theirs.signer().as_ref() != Some(&signer) {
        return None;
    }
    // Another account's bundle silences no debt of this one's.
    match (
        files.owner(&exe),
        files.owner(&running.join(BUNDLE_EXECUTABLE)),
    ) {
        (Ok(theirs), Ok(ours)) if theirs == 0 || theirs == ours => {}
        _ => return None,
    }
    let image = image_against(files, &theirs);
    (!image.owed()).then_some(Reading {
        image,
        plist: Part::Current,
        held_by: Some(source),
    })
}

fn read_with(files: &dyn Files, source: &Path, held: bool) -> Result<Reading, String> {
    let ours_path = source.join(BUNDLE_EXECUTABLE);
    let ours = files
        .code_directories(&ours_path)
        .map_err(|e| format!("{}: {e}", ours_path.display()))?;
    let image = image_against(files, &ours);
    let plist = match files.read(Path::new(super::PLIST_PATH)) {
        Ok(bytes) if bytes == plist_text(source).as_bytes() => Part::Current,
        Ok(bytes) => {
            if held && let Some(held) = held_by_another(files, source, &ours, &image, &bytes) {
                return Ok(held);
            }
            Part::Differs
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Part::Missing,
        Err(e) => Part::Unread(format!("{}: {e}", super::PLIST_PATH)),
    };
    Ok(Reading {
        image,
        plist,
        held_by: None,
    })
}

/// Read the broker's install against `source`, the app bundle this build runs from, and that
/// bundle alone: the clone's code against the source executable's (another build's of the same
/// team the broker adopts, [`Part::Adopting`]), and the plist against [`plist_text`]. It is the
/// elevation's reading: a root half installs its own bundle, so a job another bundle holds is
/// another's to it ([`Part::Differs`]).
///
/// # Errors
/// The source's own code could not be read (there is nothing to compare the clone with), in one
/// line.
pub fn read_own(files: &dyn Files, source: &Path) -> Result<Reading, String> {
    read_with(files, source, false)
}

/// Read the broker's install as an account decides whether to ask for it: [`read_own`]'s
/// reading, save that a job another bundle holds is read against that bundle
/// ([`Reading::held_by`]) and is in place where all of this holds. The plist is byte for byte
/// the one this build writes for that bundle. The bundle's executable is there under its own
/// name, no link on the way to it. It states this build's own non-ad-hoc [`Signer`] of
/// [`BUNDLE_ID`], and this build's executable's owner or root owns it. And the image is that
/// bundle's code or adoptable by it.
///
/// The broker then serves this build as it serves that one (it admits its team), and no
/// password is asked to move the job; where an elevation runs anyway it moves it
/// ([`read_own`]), and the other bundle then reads this one as the holder. The signer is read,
/// not verified, and decides nothing root installs or runs. What a holder does decide is which
/// build of the team the broker adopts next, as a job's source always did: an account that
/// reads a held job does not ask to take it back while the holder stays as above. A bundle
/// with no team signature (a development build) shares no broker: an ad-hoc broker admits its
/// own code alone.
///
/// # Errors
/// [`read_own`]'s.
pub fn read(files: &dyn Files, source: &Path) -> Result<Reading, String> {
    read_with(files, source, true)
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
#[must_use]
pub fn read_here() -> Option<(PathBuf, Result<Reading, String>)> {
    let source = source_here()?;
    let reading = read(&Disk, &source);
    Some((source, reading))
}

/// Whether setup's one elevation owes this Mac the broker, using this process's file reading
/// and the injected service readings ([`Reading::owed_with_job`]). `false` with no source
/// and for a file reading that failed, so no password is asked for want of a reading.
#[must_use]
pub fn owed_here(
    running: impl FnOnce() -> Option<bool>,
    switched_off: impl FnOnce() -> Option<bool>,
) -> bool {
    read_here().is_some_and(|(_, reading)| {
        reading.is_ok_and(|reading| reading.owed_with_job(running, switched_off))
    })
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

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
