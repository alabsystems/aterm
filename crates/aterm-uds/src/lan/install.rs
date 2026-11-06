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

/// The root-only (0700) folder the elevation clones the source into before it seals the clone
/// and swaps it in: nothing but root can reach the clone while its owners and modes are still
/// the source's.
pub const STAGING_DIR: &str = "/Library/Application Support/aterm/lan/.staging";

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
}

/// How one installed part reads against what this build would install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// As this build installs it.
    Current,
    /// Not there.
    Missing,
    /// There, and not what this build installs (another build's image, another source's plist).
    Differs,
    /// The image is another build's, and it and the source state one non-ad-hoc [`Signer`]
    /// (the team named) with [`BUNDLE_ID`]: the running broker adopts the source by itself
    /// (`docs/DESIGN-lan-broker-2026-11-05.md` §3), so a release update owes no password. Not
    /// current either: an elevation that runs for any reason renews it.
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
    /// The root-owned clone ([`super::IMAGE_BUNDLE`]) against the source bundle's code.
    pub image: Part,
    /// [`super::PLIST_PATH`] against [`plist_text`].
    pub plist: Part,
}

impl Reading {
    /// Whether setup's one elevation owes the broker: either part is missing or differs.
    #[must_use]
    pub const fn owed(&self) -> bool {
        self.image.owed() || self.plist.owed()
    }

    /// Whether both parts are as this build installs them.
    #[must_use]
    pub fn current(&self) -> bool {
        self.image == Part::Current && self.plist == Part::Current
    }

    /// The reading in one line: `image <state>, plist <state>`.
    #[must_use]
    pub fn words(&self) -> String {
        format!("image {}, plist {}", self.image.words(), self.plist.words())
    }
}

/// Read the broker's install against `source`, the app bundle this build runs from: the clone's
/// code against the source executable's (another build's of the same team the broker adopts,
/// [`Part::Adopting`]), and the plist against [`plist_text`].
///
/// # Errors
/// The source's own code could not be read (there is nothing to compare the clone with), in one
/// line.
pub fn read(files: &dyn Files, source: &Path) -> Result<Reading, String> {
    let ours_path = source.join(BUNDLE_EXECUTABLE);
    let ours = files
        .code_directories(&ours_path)
        .map_err(|e| format!("{}: {e}", ours_path.display()))?;
    let image = match files.code_directories(Path::new(super::IMAGE_EXECUTABLE)) {
        Ok(theirs) if theirs == ours => Part::Current,
        Ok(theirs) => match (theirs.signer(), ours.signer()) {
            (Some(theirs), Some(ours)) if theirs == ours && ours.identifier == BUNDLE_ID => {
                Part::Adopting(ours.team)
            }
            _ => Part::Differs,
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Part::Missing,
        Err(e) => Part::Unread(format!("{}: {e}", super::IMAGE_EXECUTABLE)),
    };
    let plist = match files.read(Path::new(super::PLIST_PATH)) {
        Ok(bytes) if bytes == plist_text(source).as_bytes() => Part::Current,
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
#[must_use]
pub fn read_here() -> Option<(PathBuf, Result<Reading, String>)> {
    let source = source_here()?;
    let reading = read(&Disk, &source);
    Some((source, reading))
}

/// Whether setup's one elevation owes this Mac the broker, as this process reads it with no
/// root: `false` with no source and for a reading that failed, so no password is asked for want
/// of a reading.
#[must_use]
pub fn owed_here() -> bool {
    read_here().is_some_and(|(_, reading)| reading.is_ok_and(|reading| reading.owed()))
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
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A universal file of `slices` (32-bit fat table, each slice 16-byte aligned).
    fn universal(slices: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&FAT_MAGIC.to_be_bytes());
        out.extend_from_slice(&(slices.len() as u32).to_be_bytes());
        let mut at = 8 + 20 * slices.len();
        let mut body = Vec::new();
        for slice in slices {
            let pad = (16 - at % 16) % 16;
            at += pad;
            body.resize(body.len() + pad, 0);
            for word in [le(slice, 4), 0, at as u32, slice.len() as u32, 4] {
                out.extend_from_slice(&word.to_be_bytes());
            }
            body.extend_from_slice(slice);
            at += slice.len();
        }
        out.extend_from_slice(&body);
        out
    }

    /// The flat `<key>`/value pairs of a plist dict, read back from its text: each value a string,
    /// `true`, or a `[..]` list of strings.
    fn keys(text: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let mut lines = text.lines().map(str::trim).peekable();
        let inner = |line: &str, tag: &str| {
            line.strip_prefix(&format!("<{tag}>"))
                .and_then(|rest| rest.strip_suffix(&format!("</{tag}>")))
                .map(|text| {
                    text.replace("&lt;", "<")
                        .replace("&gt;", ">")
                        .replace("&quot;", "\"")
                        .replace("&apos;", "'")
                        .replace("&amp;", "&")
                })
        };
        while let Some(line) = lines.next() {
            let Some(key) = inner(line, "key") else {
                continue;
            };
            let value = match lines.next().unwrap() {
                "<true/>" => "true".to_owned(),
                "<array>" => {
                    let mut items = Vec::new();
                    for item in lines.by_ref() {
                        if item == "</array>" {
                            break;
                        }
                        items.push(inner(item, "string").unwrap());
                    }
                    format!("[{}]", items.join("|"))
                }
                other => inner(other, "string").unwrap(),
            };
            out.insert(key, value);
        }
        out
    }

    /// The plist's keys are the job the design names: a root system daemon (no `UserName`)
    /// running the clone's executable with `lan serve --source <the source>`, kept alive, its
    /// output in the broker's log. A source path with XML's own characters reads back whole.
    #[test]
    fn the_plist_runs_the_clone_as_root_with_its_source_named() {
        let source = Path::new("/Applications/a&b <dev>.app");
        let text = plist_text(source);
        let keys = keys(&text);
        let want = [
            ("Label", super::super::LABEL.to_owned()),
            (
                "ProgramArguments",
                format!(
                    "[{}|lan|serve|--source|/Applications/a&b <dev>.app]",
                    super::super::IMAGE_EXECUTABLE
                ),
            ),
            ("RunAtLoad", "true".to_owned()),
            ("KeepAlive", "true".to_owned()),
            ("ProcessType", "Standard".to_owned()),
            (
                "AssociatedBundleIdentifiers",
                "[com.aterm.aterm]".to_owned(),
            ),
            ("StandardOutPath", super::super::LOG_PATH.to_owned()),
            ("StandardErrorPath", super::super::LOG_PATH.to_owned()),
        ];
        assert_eq!(
            keys,
            want.iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
            "{text}"
        );
        assert!(!text.contains("UserName"), "{text}");
        assert_ne!(text, plist_text(Path::new("/Applications/aterm.app")));
    }

    /// Thin and universal files read every CodeDirectory of every slice, from bytes and from a
    /// file alike. NEGATIVE CONTROLS: a file with no signature, a non-Mach-O, and one cut short
    /// are refused, never read as some identity.
    #[test]
    fn code_directories_are_read_from_each_slice() {
        let arm = test_image(0x0100_000c, &[b"sha1 directory", b"sha256 directory"]);
        let x86 = test_image(0x0100_0007, &[b"x86 directory"]);
        let thin = CodeDirectories::of_bytes(&arm).unwrap();
        assert_eq!(
            thin,
            CodeDirectories(vec![
                (
                    0x0100_000c,
                    [
                        &CODE_DIRECTORY.to_be_bytes()[..],
                        &22_u32.to_be_bytes(),
                        b"sha1 directory"
                    ]
                    .concat()
                ),
                (
                    0x0100_000c,
                    [
                        &CODE_DIRECTORY.to_be_bytes()[..],
                        &24_u32.to_be_bytes(),
                        b"sha256 directory"
                    ]
                    .concat()
                ),
            ])
        );
        let fat = CodeDirectories::of_bytes(&universal(&[arm.clone(), x86])).unwrap();
        assert_eq!(fat.0.len(), 3);
        assert_eq!(fat.0[..2], thin.0[..]);
        assert_eq!(fat.0[2].0, 0x0100_0007);

        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("aterm");
        std::fs::write(&path, &arm).unwrap();
        assert_eq!(CodeDirectories::of_file(&path).unwrap(), thin);
        let other = test_image(0x0100_000c, &[b"sha1 directorz", b"sha256 directory"]);
        assert_ne!(CodeDirectories::of_bytes(&other).unwrap(), thin);

        let mut unsigned = arm.clone();
        unsigned[32..36].copy_from_slice(&0x19_u32.to_le_bytes());
        for (bytes, kind) in [
            (unsigned, io::ErrorKind::InvalidData),
            (b"#!/bin/sh\necho hi\n".to_vec(), io::ErrorKind::InvalidData),
            (arm[..arm.len() - 1].to_vec(), io::ErrorKind::UnexpectedEof),
        ] {
            assert_eq!(CodeDirectories::of_bytes(&bytes).unwrap_err().kind(), kind);
        }
    }

    /// Offsets and lengths the file names are checked, never trusted: a 64-bit universal entry
    /// at the end of `u64`, and a CodeDirectory that claims more than the file holds, read as a
    /// file that ends early, from bytes and from a file alike, with nothing allocated for them.
    #[test]
    fn offsets_past_the_file_read_as_short_never_overflow() {
        let mut wide = Vec::new();
        wide.extend_from_slice(&FAT_MAGIC_64.to_be_bytes());
        wide.extend_from_slice(&1_u32.to_be_bytes());
        for word in [0x0100_000c_u32, 0] {
            wide.extend_from_slice(&word.to_be_bytes());
        }
        wide.extend_from_slice(&(u64::MAX - 1).to_be_bytes());
        wide.extend_from_slice(&64_u64.to_be_bytes());
        wide.extend_from_slice(&[0; 8]);

        let mut claims = test_image(0x0100_000c, &[b"this build"]);
        // The signature's length in the load command, then the blob's own length.
        claims[44..48].copy_from_slice(&u32::MAX.to_le_bytes());
        let blob = 48 + 12 + 8;
        claims[blob + 4..blob + 8].copy_from_slice(&MAX_DIRECTORY.to_be_bytes());

        let dir = aterm_tempfile::tempdir().unwrap();
        for (name, bytes) in [("wide", wide), ("claims", claims)] {
            assert_eq!(
                CodeDirectories::of_bytes(&bytes).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof,
                "{name}"
            );
            let path = dir.path().join(name);
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(
                CodeDirectories::of_file(&path).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof,
                "{name}"
            );
        }
    }

    /// A real signed executable (this test binary, which Apple's linker signs ad hoc on arm64)
    /// reads the same from its file's headers as from its whole bytes, and names no signer.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn a_real_executable_reads_the_same_from_its_file_and_its_bytes() {
        let exe = std::env::current_exe().unwrap();
        let from_file = CodeDirectories::of_file(&exe).unwrap();
        assert!(!from_file.0.is_empty());
        assert_eq!(
            from_file.signer(),
            None,
            "the linker's signature names no team"
        );
        assert_eq!(
            from_file,
            CodeDirectories::of_bytes(&std::fs::read(&exe).unwrap()).unwrap()
        );
    }

    struct Fixed {
        files: BTreeMap<PathBuf, io::Result<Vec<u8>>>,
    }

    impl Fixed {
        fn new(entries: Vec<(&str, io::Result<Vec<u8>>)>) -> Self {
            Self {
                files: entries
                    .into_iter()
                    .map(|(path, bytes)| (PathBuf::from(path), bytes))
                    .collect(),
            }
        }
    }

    impl Files for Fixed {
        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            match self.files.get(path) {
                Some(Ok(bytes)) => Ok(bytes.clone()),
                Some(Err(e)) => Err(io::Error::from(e.kind())),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }

        fn code_directories(&self, path: &Path) -> io::Result<CodeDirectories> {
            CodeDirectories::of_bytes(&self.read(path)?)
        }
    }

    /// A CodeDirectory blob's body (what follows its magic and length) of `version` and `flags`
    /// naming `identifier` and, at a nonzero `teamOffset`, `team`.
    fn signed(version: u32, flags: u32, identifier: &str, team: Option<&str>) -> Vec<u8> {
        let ident_at = 52_u32;
        let team_at = team.map_or(0, |_| ident_at + identifier.len() as u32 + 1);
        let mut body = Vec::new();
        for word in [version, flags, 0, ident_at, 0, 0, 0, 0, 0, 0, team_at] {
            body.extend_from_slice(&word.to_be_bytes());
        }
        body.extend_from_slice(identifier.as_bytes());
        body.push(0);
        if let Some(team) = team {
            body.extend_from_slice(team.as_bytes());
            body.push(0);
        }
        body
    }

    /// A team signature names its identifier and team; an ad-hoc, linker-signed, teamless or
    /// pre-team directory names none, and neither do two that disagree.
    #[test]
    fn a_signer_is_read_only_from_a_team_signature() {
        let signer = |bodies: &[&[u8]]| {
            CodeDirectories::of_bytes(&test_image(0x0100_000c, bodies))
                .unwrap()
                .signer()
        };
        let team = signed(0x20400, 0, BUNDLE_ID, Some("ABCDE12345"));
        assert_eq!(
            signer(&[&team, &team]),
            Some(Signer {
                identifier: BUNDLE_ID.to_owned(),
                team: "ABCDE12345".to_owned()
            })
        );
        let other = signed(0x20400, 0, BUNDLE_ID, Some("ZZZZZ99999"));
        for bodies in [
            vec![signed(0x20400, CS_ADHOC, BUNDLE_ID, Some("ABCDE12345"))],
            vec![signed(
                0x20400,
                CS_LINKER_SIGNED,
                BUNDLE_ID,
                Some("ABCDE12345"),
            )],
            vec![signed(0x20400, 0, BUNDLE_ID, None)],
            vec![signed(0x20100, 0, BUNDLE_ID, Some("ABCDE12345"))],
            vec![team.clone(), other],
            vec![b"this build".to_vec()],
        ] {
            let bodies: Vec<&[u8]> = bodies.iter().map(Vec::as_slice).collect();
            assert_eq!(signer(&bodies), None, "{bodies:?}");
        }
    }

    /// Owed while the clone is missing or another build's, or the plist is missing or another
    /// source's; current when both match. Another build's image that names the source's own team
    /// and identifier is the broker's to adopt, and owes nothing. NEGATIVE CONTROLS: an
    /// unreadable clone or plist owes nothing, a source whose own code cannot be read is no
    /// reading at all, and an ad-hoc pair, two teams, or another identifier stay owed.
    #[test]
    fn the_install_is_owed_by_what_an_account_reads() {
        const SOURCE: &str = "/Applications/aterm.app";
        const OURS: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
        let ours = test_image(0x0100_000c, &[b"this build"]);
        let theirs = test_image(0x0100_000c, &[b"another build"]);
        let release = |identifier: &str, team: &str, flags: u32, page: u8| {
            let mut body = signed(0x20400, flags, identifier, Some(team));
            body.push(page);
            test_image(0x0100_000c, &[&body])
        };
        let plist = plist_text(Path::new(SOURCE)).into_bytes();
        let elsewhere = plist_text(Path::new("/Users//a/Applications/aterm.app")).into_bytes();
        let denied = || Err(io::Error::from(io::ErrorKind::PermissionDenied));
        let image = super::super::IMAGE_EXECUTABLE;
        let daemon = super::super::PLIST_PATH;
        type Case<'a> = (Vec<(&'a str, io::Result<Vec<u8>>)>, Part, Part);
        let cases: Vec<Case<'_>> = vec![
            (vec![], Part::Missing, Part::Missing),
            (
                vec![(image, Ok(ours.clone())), (daemon, Ok(plist.clone()))],
                Part::Current,
                Part::Current,
            ),
            (
                vec![(image, Ok(theirs)), (daemon, Ok(plist.clone()))],
                Part::Differs,
                Part::Current,
            ),
            (
                vec![(image, Ok(ours.clone())), (daemon, Ok(elsewhere))],
                Part::Current,
                Part::Differs,
            ),
            (
                vec![(image, denied()), (daemon, denied())],
                Part::Unread(format!("{image}: permission denied")),
                Part::Unread(format!("{daemon}: permission denied")),
            ),
        ];
        for (mut files, image_part, plist_part) in cases {
            files.push((OURS, Ok(ours.clone())));
            check(files, image_part, plist_part);
        }
        let team = "ABCDE12345";
        let dev = "com.aterm.aterm.dev";
        let updates = [
            (
                BUNDLE_ID,
                team,
                0,
                BUNDLE_ID,
                team,
                Part::Adopting(team.to_owned()),
            ),
            (BUNDLE_ID, "ZZZZZ99999", 0, BUNDLE_ID, team, Part::Differs),
            (dev, team, 0, dev, team, Part::Differs),
            (BUNDLE_ID, team, CS_ADHOC, BUNDLE_ID, team, Part::Differs),
        ];
        for (identifier, image_team, flags, source_identifier, source_team, image_part) in updates {
            check(
                vec![
                    (image, Ok(release(identifier, image_team, flags, 1))),
                    (daemon, Ok(plist.clone())),
                    (OURS, Ok(release(source_identifier, source_team, flags, 2))),
                ],
                image_part,
                Part::Current,
            );
        }
        assert!(read(&Fixed::new(vec![]), Path::new(SOURCE)).is_err());
    }

    /// [`read`] of `files` against the source reads `image` and `plist`, owed and current as
    /// those two say.
    fn check(files: Vec<(&str, io::Result<Vec<u8>>)>, image: Part, plist: Part) {
        let reading = read(&Fixed::new(files), Path::new("/Applications/aterm.app")).unwrap();
        let owed = image.owed() || plist.owed();
        let current = image == Part::Current && plist == Part::Current;
        assert_eq!(reading, Reading { image, plist });
        assert_eq!(
            (reading.owed(), reading.current()),
            (owed, current),
            "{reading:?}"
        );
    }

    /// The source is the bundle around the executable; a bare build has none.
    #[test]
    fn the_source_is_the_bundle_the_executable_sits_in() {
        assert_eq!(
            source_bundle(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")),
            Some(PathBuf::from("/Applications/aterm.app"))
        );
        assert_eq!(
            source_bundle(Path::new("/Users//a/aterm/target/debug/aterm")),
            None
        );
        assert_eq!(source_bundle(Path::new("/Applications/aterm.app")), None);
    }
}
