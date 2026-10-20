// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH IMAGE HOLDS THE TERMINAL, published before its bytes
//! (`docs/DESIGN-card-engine-2026-10-19.md` §3.4-§3.6, invariant I1).
//!
//! The PTY gather samples the foreground group at every dry-gap park and batch
//! end (`crate::foreground_handback`). At each edge it reads the new holder's
//! image from the kernel ([`read`]: the executable, its start time and, for a
//! script shell only, its first argument) and classifies it
//! (`atpkg::image::image_build`). The result rides the batch as a
//! [`HolderImage`]. The parse stage offers it to the session's timeline
//! before the term-lock hold of the chosen slice
//! (`SessionTimeline::offer_holder_image`). This orders a sampled observation
//! before that hold, not a proof of which process wrote every queued byte.
//!
//! Only CARD-RELEVANT edges publish ([`HolderImage::card_relevant`]): entering
//! or leaving a card drawer (Claude Code, agy, Muse Code) or atpkg's own shim
//! before its `exec`. Every other edge costs the gather's read and nothing
//! else: no timeline write, no inbox push, no wake. Publication is
//! deduplicated by token ([`Offer::Same`]); the engine's foreground cut never
//! is.
//!
//! A published edge also pushes the session's id into the App's
//! [`EdgeInbox`], which the status sweep empties first on its next wake,
//! bypassing the observation interval (§3.5). Publication wakes naming; it
//! does not establish a one-frame or elapsed-time naming guarantee.
//!
//! THE BYTE-DRIVEN ARM (§3.6, [`ByteArm`]): an `exec` in place (`exec claude`,
//! a shim's `exec` of the store build) changes the image with no group edge.
//! The first nonempty batch observes the initial holder, including direct
//! `-e` and an already-running adopted program. An accepted signed C owes
//! another read before the next slice; while its shell remains unresolved,
//! it is observed once per later batch until the command ends or an image
//! resolves. A Launcher is also observed once per batch. Settled drawer and
//! other non-shell output batches add no image reads. A changed description
//! is offered as an Exec edge, not proof of a kernel exec generation. The slice loop ends a slice right after every
//! complete OSC 133/633 C ([`CScan`]) while the arm is on, so the re-read runs
//! before the bytes after the C are parsed. Lane A2's kqueue edges will turn
//! the arm off on macOS; in A1 it runs on every unix.
//!
//! The dev rows ([`DEV_ROWS`]: debug builds and `dev-seams`) put each step on
//! the session timeline with the present ordinal (`metrics::frames_presented`)
//! and the process clock (`metrics::now_ns`): `holder-edge`, `holder-image`,
//! `holder-named`, `holder-claim` and `card-seen`. The latter requires the
//! current drawer's family and known version on screen, but is only a screen
//! observation: stale same-build cells and other tty writers are not ruled
//! out. These rows alone do not establish writer attribution or lead times.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use atpkg::image::{ImageBuild, ImageRoots, ImageSource};

/// Whether the dev timeline rows are recorded: debug builds and an optimized
/// `dev-seams` QA build. A shipping build records none.
pub(crate) const DEV_ROWS: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// How many batches the dev `card-seen` probe looks for one drawer token's
/// card before it gives up (`DEV_ROWS` builds only).
pub(crate) const CARD_PROBE_BATCHES: u16 = 256;

/// The longest unfinished OSC [`CScan`] carries into the next batch: a signed
/// `133;C;id=<64 hex>` is under 80 bytes.
const C_CARRY_MAX: usize = 512;

/// A program that draws a launch card the harness decorates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CardProgram {
    Claude,
    Agy,
    Muse,
}

impl CardProgram {
    /// The card drawer `program` names (`aterm_phase::program_of`: `claude`,
    /// `claude-code`, `agy`, `muse`, `muse-bin-<version>`), or `None`.
    pub(crate) fn of(program: &str) -> Option<Self> {
        match aterm_phase::program_of(program)? {
            aterm_phase::Program::Claude => Some(Self::Claude),
            aterm_phase::Program::Agy => Some(Self::Agy),
            aterm_phase::Program::Muse => Some(Self::Muse),
            aterm_phase::Program::Codex
            | aterm_phase::Program::Gemini
            | aterm_phase::Program::Generic => None,
        }
    }

    /// The word a timeline row prints.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Agy => "agy",
            Self::Muse => "muse",
        }
    }
}

/// How the holder's image changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EdgeKind {
    /// The foreground group changed (the gather's sampled cut).
    Group,
    /// The group's image changed in place (the byte-driven arm's re-read).
    Exec,
}

impl EdgeKind {
    /// The word a timeline row prints.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Exec => "exec",
        }
    }
}

/// One reading of a foreground group leader's image (§3.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HolderImage {
    /// A hash of pid, process start, executable and classified build
    /// ([`token_of`]). This names a sampled image description, not an exec
    /// generation: re-execing the same path and build can keep it. `None`
    /// when the start or executable could not be read (I5).
    pub(crate) token: Option<u64>,
    /// The group (its leader's pid) the image was read for.
    pub(crate) pgid: i32,
    /// The leader's start, in microseconds on the platform's start clock
    /// (macOS: the BSD info's wall time; Linux: `CLOCK_BOOTTIME`); `0` unread.
    pub(crate) start_us: u64,
    /// The executable the kernel mapped, canonical; empty when unread.
    pub(crate) exe: Arc<Path>,
    /// What `atpkg::image` makes of it.
    pub(crate) build: ImageBuild,
    /// The card drawer the image is, when it is one (and not a launcher).
    pub(crate) drawer: Option<CardProgram>,
    /// The card drawer atpkg's shim will `exec`, for a Launcher.
    pub(crate) launcher_for: Option<CardProgram>,
    /// When it was read, on `metrics::now_ns` (the dev rows' `read_ns`).
    pub(crate) read_ns: u64,
}

impl HolderImage {
    /// The image of `pgid` from its parts: `start_us` and `exe` as read (`None`
    /// where the read failed) and the classifier's `build`.
    pub(crate) fn of(
        pgid: i32,
        start_us: Option<u64>,
        exe: Option<PathBuf>,
        build: ImageBuild,
        read_ns: u64,
    ) -> Self {
        let token = match (&exe, start_us) {
            (Some(exe), Some(start)) if start > 0 && pgid > 0 => {
                Some(token_of(pgid, start, exe, &build))
            }
            _ => None,
        };
        let program = build.program.as_ref().map(atpkg::image::ProgramTag::as_str);
        let (drawer, launcher_for) = match (build.source, program.and_then(CardProgram::of)) {
            (ImageSource::Launcher, card) => (None, card),
            (_, card) => (card, None),
        };
        Self {
            token,
            pgid,
            start_us: start_us.unwrap_or(0),
            exe: Arc::from(exe.unwrap_or_default()),
            build,
            drawer,
            launcher_for,
            read_ns,
        }
    }

    /// An image nothing could be read of: the inert arm the test App's factory
    /// carries ([`unread`]), and what a read of a gone group gives.
    pub(crate) fn unread(pgid: i32) -> Self {
        Self::of(pgid, None, None, ImageBuild::unknown(None), 0)
    }

    /// Whether an edge into (or out of) this image is the card's business: a
    /// card drawer, or atpkg's shim that will `exec` one.
    pub(crate) const fn card_relevant(&self) -> bool {
        self.drawer.is_some() || self.launcher_for.is_some()
    }

    /// Whether the represented image descriptions agree. Compare the fields
    /// too: neither a hash collision nor a refreshed classification keeps an
    /// older build or launcher on the timeline.
    pub(crate) fn same_image(&self, other: &Self) -> bool {
        self.token == other.token
            && self.pgid == other.pgid
            && self.start_us == other.start_us
            && self.exe == other.exe
            && self.build == other.build
    }

    /// Whether this is a script shell still able to exec the command it
    /// signed. Only its mapped executable decides this, not argv text.
    fn script_shell(&self) -> bool {
        self.exe
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| {
                atpkg::image::SCRIPT_SHELLS.contains(&n.trim_end_matches(" (deleted)"))
            })
    }

    /// A matching family and known version on the screen. This is evidence
    /// about visible text, never proof that this process wrote those cells.
    pub(crate) fn matches_card(&self, rows_kind: usize, version: &str) -> bool {
        let expected_kind = match self.drawer {
            Some(CardProgram::Claude) => 3,
            Some(CardProgram::Agy) => 4,
            Some(CardProgram::Muse) => 1,
            None => return false,
        };
        rows_kind == expected_kind
            && self.build.version().is_some_and(|expected| {
                atpkg::vendor_direct::Version::parse(version) == Some(expected)
            })
    }

    /// `holder-image`'s payload: `token= pgid= program= build= source=
    /// drawer= launcher_for=`.
    pub(crate) fn payload(&self) -> String {
        format!(
            "token={} pgid={} program={} build={} source={} drawer={} launcher_for={}",
            token_word(self.token),
            self.pgid,
            self.build
                .program
                .as_ref()
                .map_or("-", atpkg::image::ProgramTag::as_str),
            self.build
                .version()
                .map_or_else(|| "-".to_string(), |v| v.to_string()),
            self.build.source.word(),
            self.drawer.map_or("-", CardProgram::word),
            self.launcher_for.map_or("-", CardProgram::word),
        )
    }
}

/// A token as a timeline row prints it: 16 hex digits, or `-`.
pub(crate) fn token_word(token: Option<u64>) -> String {
    token.map_or_else(|| "-".to_string(), |t| format!("{t:016x}"))
}

/// THE SNAPSHOT TOKEN (I5): FNV-1a/64 of the pid, process start,
/// executable and classification. A different mapped path or classified
/// program/build/source refreshes it. The represented fields cannot detect
/// an invisible exec of the same path and build; this is not a kernel exec
/// generation, and a finite hash is not a collision-free identity.
pub(crate) fn token_of(pid: i32, start_us: u64, exe: &Path, build: &ImageBuild) -> u64 {
    let mut h = aterm_hash::Fnv1a64::new();
    h.write_u32_le(pid.cast_unsigned());
    h.write_u64_le(start_us);
    h.write(exe.as_os_str().as_encoded_bytes());
    h.write(&[0]);
    h.write(build.program.as_ref().map_or("", |p| p.as_str()).as_bytes());
    h.write(&[0]);
    h.write_u64_le(build.build);
    h.write(build.source.word().as_bytes());
    h.finish()
}

/// The test App's inert `SessionFactory::edge_image` (and ConPTY's): reads
/// nothing.
#[cfg(any(test, feature = "bench-support", not(unix)))]
pub(crate) fn unread(pgid: i32) -> HolderImage {
    HolderImage::unread(pgid)
}

/// THE READ (shipping `SessionFactory::edge_image`), on the gather thread at
/// an edge, or on the parse stage for the byte arm's re-read: `pgid`'s
/// leader's executable (`proc_pidpath`, `/proc/<pid>/exe`), its start (BSD
/// info; `/proc/<pid>/stat`), for a script shell its first argument
/// (`KERN_PROCARGS2`, `/proc/<pid>/cmdline`), and the classifier over the
/// managed prefix's roots ([`roots`]).
pub(crate) fn read(pgid: i32) -> HolderImage {
    let read_ns = crate::metrics::now_ns();
    let Some(pid) = u32::try_from(pgid).ok().filter(|p| *p > 1) else {
        return HolderImage::unread(pgid);
    };
    let Some(exe) = aterm_sysprobe::process_path(pid) else {
        return HolderImage::of(pgid, None, None, ImageBuild::unknown(None), read_ns);
    };
    let start_us = start_us_of(pgid);
    let shell = exe
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| atpkg::image::SCRIPT_SHELLS.contains(&n.trim_end_matches(" (deleted)")));
    let argv1 = shell
        .then(|| crate::session_status::program::argv_head_of(pgid))
        .flatten()
        .and_then(|(_, argv1)| argv1);
    let roots = roots();
    let build = atpkg::image::image_build(roots.as_deref(), &exe, argv1.as_deref().map(Path::new));
    HolderImage::of(pgid, start_us, Some(exe), build, read_ns)
}

/// `pgid`'s leader's start in microseconds, or `None` (gone, another user's).
#[cfg(target_vendor = "apple")]
fn start_us_of(pgid: i32) -> Option<u64> {
    let birth = aterm_uds::process::read_process_birth(pgid)?;
    birth
        .seconds
        .checked_mul(1_000_000)?
        .checked_add(birth.microseconds)
}

/// `pgid`'s leader's start in microseconds, or `None` (gone, another user's).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn start_us_of(pgid: i32) -> Option<u64> {
    aterm_uds::process::read_process_start(pgid).map(|s| s.boot.0 / 1_000)
}

/// No start record wired here: every image is untracked.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn start_us_of(_pgid: i32) -> Option<u64> {
    None
}

/// A file's identity for the roots cache: inode, size and modification time.
type Stamp = Option<(u64, u64, i128)>;

fn stamp(path: &Path) -> Stamp {
    let md = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(&md);
    #[cfg(not(unix))]
    let ino = 0;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX));
    Some((ino, md.len(), mtime))
}

/// The roots last resolved, and what they were resolved from: `aterm.toml`'s
/// stamp and the prefix's.
struct RootsCache {
    config: Stamp,
    prefix: Stamp,
    roots: Option<Arc<ImageRoots>>,
}

static ROOTS: Mutex<Option<RootsCache>> = Mutex::new(None);

/// The managed prefix's [`ImageRoots`], resolved once and again only when
/// `aterm.toml` or the prefix directory changes (two `stat`s per call): every
/// PTY gather reads images through one cache, and none re-reads the config
/// per edge.
pub(crate) fn roots() -> Option<Arc<ImageRoots>> {
    let config = atpkg::config::config_path().and_then(|p| stamp(&p));
    let mut cache = ROOTS.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(c) = cache.as_ref()
        && c.config == config
        && c.prefix == c.roots.as_ref().and_then(|r| stamp(&r.layout().prefix))
    {
        return c.roots.clone();
    }
    let roots = atpkg::store::resolve_configured().map(|layout| Arc::new(ImageRoots::new(&layout)));
    let prefix = roots.as_ref().and_then(|r| stamp(&r.layout().prefix));
    *cache = Some(RootsCache {
        config,
        prefix,
        roots: roots.clone(),
    });
    roots
}

/// What the timeline made of an offered image
/// (`SessionTimeline::offer_holder_image`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    /// Published: it is the session's holder image now.
    Published,
    /// The image already published: nothing moved.
    Same,
    /// Neither it nor the image it would replace is the card's business.
    Irrelevant,
}

/// THE PUBLICATION RULE, pure: an image is published when it, or the image it
/// replaces, is card-relevant, and it is not that same image.
pub(crate) fn decide(current: Option<&HolderImage>, offered: &HolderImage) -> Offer {
    if !offered.card_relevant() && !current.is_some_and(HolderImage::card_relevant) {
        return Offer::Irrelevant;
    }
    if current.is_some_and(|c| c.same_image(offered)) {
        return Offer::Same;
    }
    Offer::Published
}

/// THE EDGED INBOX (§3.5): the sessions whose holder image was published since
/// the status sweep last looked. Owned by the App (through its
/// `SessionFactory`), cloned into every session's parse stage, which pushes its
/// id at most once per published edge; the sweep takes the whole list first
/// and visits exactly those sessions, bypassing their observation interval.
#[derive(Clone, Debug, Default)]
pub(crate) struct EdgeInbox(Arc<Mutex<Vec<u64>>>);

impl EdgeInbox {
    /// `session` has a published edge the sweep has not seen: queued once.
    pub(crate) fn push(&self, session: u64) {
        let mut ids = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if !ids.contains(&session) {
            ids.push(session);
        }
    }

    /// Every queued id, oldest first; the inbox is left empty.
    pub(crate) fn take(&self) -> Vec<u64> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Whether nothing is queued.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
    }
}

/// THE C-CUT SCAN (§3.6): the offsets in a batch right after each complete
/// OSC 133 or 633 `C` (`ESC ] 133;C[;…] BEL|ESC \`), so the slice loop ends a
/// slice there and the byte arm's re-read runs before the next byte is parsed.
/// Whether the engine accepted the C (its nonce) is the engine's to say; a cut
/// at an unsigned one only splits a slice, which the streaming parser makes
/// byte-identical. An OSC unfinished at a batch's end is carried (at most
/// [`C_CARRY_MAX`] bytes) and its end found in the next batch.
#[derive(Debug, Default)]
pub(crate) struct CScan {
    carry: Vec<u8>,
}

impl CScan {
    /// Clear `ends` and fill it with the cut offsets in `batch`, ascending,
    /// each in `1..=batch.len()`.
    pub(crate) fn ends(&mut self, batch: &[u8], ends: &mut Vec<usize>) {
        ends.clear();
        let carried = self.carry.len();
        let mut at = 0;
        if carried > 0 {
            // The carried OSC's end, found in this batch.
            let mut joined = std::mem::take(&mut self.carry);
            let take = batch.len().min(C_CARRY_MAX);
            joined.extend_from_slice(&batch[..take]);
            match osc_c_end(&joined, 0) {
                OscEnd::C(end) if end > carried => {
                    ends.push(end - carried);
                    at = end - carried;
                }
                OscEnd::Open if joined.len() < C_CARRY_MAX && take == batch.len() => {
                    self.carry = joined;
                    return;
                }
                _ => {}
            }
        }
        while let Some(esc) = find_osc(batch, at) {
            match osc_c_end(batch, esc) {
                OscEnd::C(end) => {
                    ends.push(end);
                    at = end;
                }
                OscEnd::Open => {
                    if batch.len() - esc < C_CARRY_MAX {
                        self.carry.extend_from_slice(&batch[esc..]);
                    }
                    return;
                }
                OscEnd::NotC => at = esc + 1,
            }
        }
        // A lone ESC at the end may open an OSC whose `]` the next batch
        // brings, which the search by `]` cannot see from here.
        if batch.len() > at && batch.last() == Some(&0x1b) {
            self.carry.push(0x1b);
        }
    }
}

/// What [`osc_c_end`] found at an ESC.
#[derive(Debug, PartialEq, Eq)]
enum OscEnd {
    /// A complete OSC 133/633 C ending at this offset (one past its terminator).
    C(usize),
    /// A prefix of one, cut by the end of the bytes.
    Open,
    /// Anything else.
    NotC,
}

/// Whether `bytes[esc..]` opens an OSC 133 or 633 `C` and where it ends.
fn osc_c_end(bytes: &[u8], esc: usize) -> OscEnd {
    const HEADS: [&[u8]; 2] = [b"\x1b]133;C", b"\x1b]633;C"];
    let rest = &bytes[esc..];
    let mut open = false;
    let mut head_len = 0;
    for head in HEADS {
        let n = rest.len().min(head.len());
        if rest[..n] == head[..n] {
            if n < head.len() {
                open = true;
            } else {
                head_len = head.len();
            }
        }
    }
    if head_len == 0 {
        return if open { OscEnd::Open } else { OscEnd::NotC };
    }
    let mut i = esc + head_len;
    // `C` alone, or `C;params`: anything else (`133;CX`) is another sequence.
    match bytes.get(i) {
        None => return OscEnd::Open,
        Some(b';' | 0x07 | 0x1b) => {}
        Some(_) => return OscEnd::NotC,
    }
    while let Some(&b) = bytes.get(i) {
        match b {
            0x07 => return OscEnd::C(i + 1),
            0x1b => {
                return match bytes.get(i + 1) {
                    Some(b'\\') => OscEnd::C(i + 2),
                    None => OscEnd::Open,
                    Some(_) => OscEnd::NotC,
                };
            }
            0x18 | 0x1a => return OscEnd::NotC,
            _ => i += 1,
        }
    }
    OscEnd::Open
}

/// The next OSC introducer (`ESC ]`) whose ESC is at or after `from`: its
/// ESC's offset. Found by its `]`, which output rarely carries outside an OSC,
/// where colour runs put an ESC every few bytes.
fn find_osc(bytes: &[u8], from: usize) -> Option<usize> {
    let mut at = from + 1;
    while let Some(close) = find_byte(bytes, at, b']') {
        if bytes[close - 1] == 0x1b {
            return Some(close - 1);
        }
        at = close + 1;
    }
    None
}

/// The first `needle` in `bytes` at or after `from`: 32 bytes a step (four
/// SWAR words, their zero tests OR-ed), then byte by byte inside the block
/// that holds it and in the tail.
fn find_byte(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let splat = LO * u64::from(needle);
    let hay = bytes.get(from..)?;
    let (blocks, _) = hay.as_chunks::<32>();
    let mut base = 0;
    for block in blocks {
        let (words, _) = block.as_chunks::<8>();
        let hit = words.iter().fold(0, |hit, word| {
            let x = u64::from_le_bytes(*word) ^ splat;
            hit | (x.wrapping_sub(LO) & !x & HI)
        });
        if hit != 0 {
            break;
        }
        base += 32;
    }
    hay[base..]
        .iter()
        .position(|&b| b == needle)
        .map(|p| from + base + p)
}

/// THE BYTE-DRIVEN ARM's state in one PTY reader's parse stage (§3.6).
#[derive(Debug)]
pub(crate) struct ByteArm {
    /// One initial observation also covers an already-running or direct -e
    /// holder, where the foreground group never changes.
    initial: bool,
    /// The signed command is still an unresolved shell. It can exec after
    /// any number of output batches, not just the next four slices.
    unresolved: bool,
    /// An accepted C owes a read before the next slice in this batch.
    boundary_due: bool,
    /// At most one ordinary observation per nonempty batch.
    batch_due: bool,
    /// A launcher's group, re-read until a resolved image replaces it.
    launcher: i32,
    /// The C-cut scan and this batch's cuts.
    pub(crate) scan: CScan,
    pub(crate) ends: Vec<usize>,
}

impl Default for ByteArm {
    fn default() -> Self {
        Self {
            initial: true,
            unresolved: false,
            boundary_due: false,
            batch_due: false,
            launcher: 0,
            scan: CScan::default(),
            ends: Vec::new(),
        }
    }
}

impl ByteArm {
    /// The arm of a reader starting on `timeline`: a Launcher already
    /// published (a reader re-attached mid-launch) is re-read from the first
    /// batch. An engine carrying an accepted executing command also remains
    /// unresolved: its C may predate this reader.
    pub(crate) fn seeded(
        timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
        executing: bool,
    ) -> Self {
        let launcher = timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .holder_image()
            .filter(|i| i.launcher_for.is_some())
            .map_or(0, |i| i.pgid);
        Self {
            launcher,
            unresolved: executing,
            ..Self::default()
        }
    }

    /// The first cut after `off` in this batch, or `None`.
    pub(crate) fn next_cut(&self, off: usize) -> Option<usize> {
        self.ends
            .get(self.ends.partition_point(|&e| e <= off))
            .copied()
    }

    /// Allow one ordinary read in this batch. A settled drawer or other
    /// non-shell image owes none, however many output batches follow.
    pub(crate) fn begin_batch(&mut self) {
        self.batch_due = true;
    }

    /// Read the initial holder, an unresolved signed command, or a launcher.
    /// There is at most one ordinary read per batch plus one after each
    /// accepted C. No term lock is held by this caller.
    pub(crate) fn read_due(
        &mut self,
        holder: i32,
        read: impl FnOnce(i32) -> HolderImage,
    ) -> Option<HolderImage> {
        if holder <= 0 {
            return None;
        }
        if self.launcher != holder {
            self.launcher = 0;
        }
        if !self.initial
            && !self.boundary_due
            && !(self.batch_due && (self.unresolved || self.launcher > 0))
        {
            return None;
        }
        self.initial = false;
        self.boundary_due = false;
        self.batch_due = false;
        Some(read(holder))
    }

    /// A gather read or re-read satisfies this batch's observation even if
    /// publication was irrelevant. A known drawer/non-shell resolves the
    /// command; a shell may still exec and stays watched until its signed
    /// command ends. An unread launcher is retried while its group remains.
    pub(crate) fn note_read(&mut self, image: &HolderImage) {
        self.initial = false;
        self.boundary_due = false;
        self.batch_due = false;
        if !image.exe.as_os_str().is_empty() {
            self.launcher = if image.launcher_for.is_some() {
                image.pgid
            } else {
                0
            };
            if image.drawer.is_some() || !image.script_shell() {
                self.unresolved = false;
            }
        }
    }

    /// The engine's accepted command mark and resulting shell state, read
    /// under its existing hold. A prompt/completion cancels only the pending
    /// shell command, not a launcher still waiting to exec.
    pub(crate) fn note_shell(&mut self, signed_c: bool, executing: bool) {
        if signed_c {
            self.unresolved = true;
            self.boundary_due = true;
        }
        if !executing {
            self.unresolved = false;
            self.boundary_due = false;
        }
    }
}

/// A dev row's tail: the present ordinal and the process clock now.
pub(crate) fn dev_tail() -> String {
    format!(
        "present={} at_ns={}",
        crate::metrics::frames_presented(),
        crate::metrics::now_ns()
    )
}

/// THE I1 TEST SEAMS: what a watched session's timeline held inside each
/// slice's term-lock hold, and the mutant that publishes after the hold
/// (`spawn_tests`' I1 bind and its control). Only a session a test registered
/// is touched.
#[cfg(test)]
pub(crate) mod hold_hook {
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// One hold: the slice's start offset in its batch and the token the
    /// timeline held then.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct Hold {
        pub(crate) off: usize,
        pub(crate) bytes: Vec<u8>,
        pub(crate) token: Option<u64>,
    }

    /// The controls' mutants a watched session runs.
    #[derive(Clone, Copy, Debug, Default)]
    pub(crate) struct Knobs {
        /// Publish the holder image after the hold of its slice, not before.
        pub(crate) publish_after_hold: bool,
        /// The byte arm without its cut after an OSC 133 C.
        pub(crate) no_c_cut: bool,
    }

    struct Watch {
        knobs: Knobs,
        holds: Vec<Hold>,
    }

    static WATCHED: Mutex<Option<HashMap<u64, Watch>>> = Mutex::new(None);

    /// Record `session`'s holds from now on, running `knobs`.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix reader tests watch a session")
    )]
    pub(crate) fn watch(session: u64, knobs: Knobs) {
        WATCHED
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(
                session,
                Watch {
                    knobs,
                    holds: Vec::new(),
                },
            );
    }

    fn knobs(session: u64) -> Knobs {
        WATCHED
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|m| m.get(&session))
            .map(|w| w.knobs)
            .unwrap_or_default()
    }

    /// Whether `session` runs the no-C-cut mutant.
    pub(crate) fn no_c_cut(session: u64) -> bool {
        knobs(session).no_c_cut
    }

    /// Stop watching `session`; its holds.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix reader tests watch a session")
    )]
    pub(crate) fn unwatch(session: u64) -> Vec<Hold> {
        WATCHED
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
            .and_then(|m| m.remove(&session))
            .map(|w| w.holds)
            .unwrap_or_default()
    }

    /// Whether `session` runs the publish-after-hold mutant.
    pub(crate) fn publish_after_hold(session: u64) -> bool {
        knobs(session).publish_after_hold
    }

    /// Inside a hold: record what `token` reads, for a watched session.
    pub(crate) fn at_hold(
        session: u64,
        off: usize,
        bytes: &[u8],
        token: impl FnOnce() -> Option<u64>,
    ) {
        let mut watched = WATCHED.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(w) = watched.as_mut().and_then(|m| m.get_mut(&session)) {
            w.holds.push(Hold {
                off,
                bytes: bytes.to_vec(),
                token: token(),
            });
        }
    }
}

/// The byte arm's C cut, timed (`benches/c_cut_cost.rs`, `bench-support`
/// only): a corpus through a fresh engine in the gather's batches and the
/// parse stage's idle slices, once as a reader without the arm slices it and
/// once as the arm does (its scan of each batch, then a slice ending right
/// after each complete OSC 133 C). The engine and grid are `verify perf
/// throughput`'s (`Terminal::process`, 24x80); the term lock, the prefetch
/// and the re-reads a signed C owes are not in it.
#[cfg(feature = "bench-support")]
pub(crate) mod cut_cost {
    use super::ByteArm;

    /// The parse stage's idle slice width (`spawn::ingest_chunk_width`).
    const SLICE: usize = 16 * 1024;

    /// One pass: MB/s and the cuts the arm made.
    fn pass(corpus: &[u8], batch: usize, cut: bool) -> (f64, usize) {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        let mut arm = ByteArm::default();
        let mut cuts = 0;
        let t0 = std::time::Instant::now();
        for b in corpus.chunks(batch.max(1)) {
            if cut {
                let ByteArm { scan, ends, .. } = &mut arm;
                scan.ends(b, ends);
                cuts += ends.len();
            }
            let mut off = 0;
            while off < b.len() {
                let limit = if cut {
                    arm.next_cut(off).unwrap_or(b.len())
                } else {
                    b.len()
                };
                let end = off + (limit - off).min(SLICE);
                term.process(std::hint::black_box(&b[off..end]));
                off = end;
            }
        }
        let secs = t0.elapsed().as_secs_f64().max(f64::MIN_POSITIVE);
        std::hint::black_box(term.cursor().col);
        (corpus.len() as f64 / 1e6 / secs, cuts)
    }

    /// Median MB/s of `runs` passes without the cut and with it (alternating,
    /// after one untimed pass of each), and the cuts one pass made.
    pub(crate) fn c_cut_cost(corpus: &[u8], batch: usize, runs: usize) -> (f64, f64, usize) {
        let _ = pass(corpus, batch, false);
        let _ = pass(corpus, batch, true);
        let (mut off, mut on, mut cuts) = (Vec::new(), Vec::new(), 0);
        for _ in 0..runs.max(1) {
            off.push(pass(corpus, batch, false).0);
            let (mbps, made) = pass(corpus, batch, true);
            on.push(mbps);
            cuts = made;
        }
        let median = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        (median(&mut off), median(&mut on), cuts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atpkg::image::ProgramTag;
    use atpkg::vendor_direct::Version;

    fn image(pgid: i32, start: u64, exe: &str, program: &str, source: ImageSource) -> HolderImage {
        HolderImage::of(
            pgid,
            Some(start),
            Some(PathBuf::from(exe)),
            ImageBuild {
                program: ProgramTag::new(program),
                build: 0,
                source,
            },
            0,
        )
    }

    /// I5: represented image descriptions, pid reuse and another executable
    /// refresh the token; an unreadable start makes none. A same-path,
    /// same-build exec is deliberately not claimed to be distinguishable.
    #[test]
    fn the_token_names_one_image() {
        let build = ImageBuild::unknown(ProgramTag::new("claude"));
        let t = token_of(100, 5, Path::new("/s/claude"), &build);
        assert_eq!(t, token_of(100, 5, Path::new("/s/claude"), &build));
        assert_ne!(
            t,
            token_of(100, 6, Path::new("/s/claude"), &build),
            "pid reuse"
        );
        assert_ne!(
            t,
            token_of(100, 5, Path::new("/s/other"), &build),
            "another path"
        );
        assert_ne!(t, token_of(101, 5, Path::new("/s/claude"), &build));
        let launcher = ImageBuild {
            source: ImageSource::Launcher,
            ..build.clone()
        };
        assert_ne!(t, token_of(100, 5, Path::new("/s/claude"), &launcher));
        let blind = HolderImage::of(
            100,
            None,
            Some(PathBuf::from("/s/claude")),
            ImageBuild::unknown(ProgramTag::new("claude")),
            0,
        );
        assert_eq!(blind.token, None, "no start, no token");
        let zero = HolderImage::of(
            100,
            Some(0),
            Some(PathBuf::from("/s/claude")),
            ImageBuild::unknown(None),
            0,
        );
        assert_eq!(zero.token, None, "a hash with start = 0 is never formed");
    }

    /// Drawer and launcher by source: a shim names the drawer it will exec,
    /// never one it is.
    #[test]
    fn a_launcher_is_never_a_drawer() {
        let store = image(
            1,
            1,
            "/p/store/claude/1/bin/claude",
            "claude",
            ImageSource::Store,
        );
        assert_eq!(store.drawer, Some(CardProgram::Claude));
        assert_eq!(store.launcher_for, None);
        let shim = image(1, 1, "/bin/sh", "claude", ImageSource::Launcher);
        assert_eq!(shim.drawer, None);
        assert_eq!(shim.launcher_for, Some(CardProgram::Claude));
        let muse = image(
            1,
            1,
            "/x/muse-bin-1.4.10-R6427.1",
            "muse-bin-1.4.10-R6427.1",
            ImageSource::Unknown,
        );
        assert_eq!(muse.drawer, Some(CardProgram::Muse));
        let zsh = image(1, 1, "/bin/zsh", "zsh", ImageSource::Unknown);
        assert!(!zsh.card_relevant());
        let codex = image(1, 1, "/x/codex", "codex", ImageSource::Unknown);
        assert!(!codex.card_relevant(), "codex draws no card");
    }

    /// The publication rule: entering and leaving a drawer publish, a second
    /// offer of the same image does not, and edges between irrelevant images
    /// touch nothing.
    #[test]
    fn publication_is_by_relevance_and_deduplicated_by_token() {
        let shell = image(10, 1, "/bin/zsh", "zsh", ImageSource::Unknown);
        let ls = image(20, 2, "/bin/ls", "ls", ImageSource::Unknown);
        let claude = image(
            30,
            3,
            "/p/store/claude/1/bin/claude",
            "claude",
            ImageSource::Store,
        );
        let shim = image(30, 3, "/bin/sh", "claude", ImageSource::Launcher);
        assert_eq!(decide(None, &shell), Offer::Irrelevant);
        assert_eq!(decide(Some(&shell), &ls), Offer::Irrelevant);
        assert_eq!(decide(Some(&shell), &claude), Offer::Published, "entering");
        assert_eq!(decide(Some(&claude), &claude.clone()), Offer::Same);
        assert_eq!(decide(Some(&claude), &shell), Offer::Published, "leaving");
        assert_eq!(decide(Some(&shell), &shim), Offer::Published, "a launcher");
        assert_eq!(decide(Some(&shim), &claude), Offer::Published, "its exec");
        let untracked = HolderImage::unread(30);
        assert_eq!(
            decide(Some(&claude), &untracked),
            Offer::Published,
            "a gone holder leaves"
        );
        assert_eq!(
            decide(Some(&untracked), &untracked.clone()),
            Offer::Irrelevant
        );

        // The interpreter is unchanged, but another shim's classification
        // must replace the old launcher and get its own snapshot token.
        let agy_shim = image(30, 3, "/bin/sh", "agy", ImageSource::Launcher);
        assert_ne!(shim.token, agy_shim.token);
        assert_eq!(decide(Some(&shim), &agy_shim), Offer::Published);
        let mut changed = claude.clone();
        changed.build.build = Version::new(2, 1, 304).unwrap().build_id();
        changed.token = Some(token_of(30, 3, &changed.exe, &changed.build));
        assert_eq!(decide(Some(&claude), &changed), Offer::Published);
        assert!(changed.matches_card(3, "2.1.304"));
        assert!(!changed.matches_card(4, "2.1.304"), "another drawer");
        assert!(!changed.matches_card(3, "2.1.303"), "another build");
        assert!(!claude.matches_card(3, "2.1.304"), "unknown build");
    }

    /// The production selector with a captured reader: initial direct exec,
    /// an exec later than four slices, cancellation, and zero steady-drawer
    /// probes. All order/count controls are deterministic, with no real IO.
    #[test]
    fn the_byte_arm_keeps_an_unresolved_command_and_stops_at_a_drawer() {
        use std::cell::Cell;
        let shell = image(10, 1, "/bin/zsh", "zsh", ImageSource::Unknown);
        let drawer = image(
            10,
            1,
            "/store/claude/1/bin/claude",
            "claude",
            ImageSource::Store,
        );
        let calls = Cell::new(0usize);
        let read = |image: &HolderImage| {
            calls.set(calls.get() + 1);
            image.clone()
        };
        let observe = |arm: &mut ByteArm, image: &HolderImage| {
            let image = arm.read_due(image.pgid, |_| read(image));
            if let Some(image) = &image {
                arm.note_read(image);
            }
            image
        };
        let mut arm = ByteArm::default();
        arm.begin_batch();
        assert_eq!(
            observe(&mut arm, &drawer),
            Some(drawer.clone()),
            "initial -e"
        );
        for _ in 0..1_000 {
            arm.begin_batch();
            assert!(observe(&mut arm, &drawer).is_none());
        }
        assert_eq!(calls.get(), 1, "no settled drawer probes");

        // A signed C owes a re-read inside this same batch, then one per
        // later batch while its shell remains unresolved.
        arm.note_shell(true, true);
        assert_eq!(observe(&mut arm, &shell), Some(shell.clone()));
        for _ in 0..8 {
            arm.begin_batch();
            assert!(observe(&mut arm, &shell).is_some());
            assert!(observe(&mut arm, &shell).is_none(), "once per batch");
        }
        arm.begin_batch();
        assert_eq!(
            observe(&mut arm, &drawer),
            Some(drawer.clone()),
            "late exec"
        );
        let resolved = calls.get();
        for _ in 0..1_000 {
            arm.begin_batch();
            assert!(observe(&mut arm, &drawer).is_none());
        }
        assert_eq!(calls.get(), resolved);
        arm.note_shell(true, true);
        arm.note_shell(false, false);
        arm.begin_batch();
        assert!(observe(&mut arm, &shell).is_none(), "signed completion");

        let shim = image(10, 1, "/bin/sh", "claude", ImageSource::Launcher);
        arm.note_read(&shim);
        for _ in 0..8 {
            arm.begin_batch();
            assert!(
                observe(&mut arm, &shim).is_some(),
                "launcher remains unresolved"
            );
            assert!(observe(&mut arm, &shim).is_none());
        }
        arm.begin_batch();
        assert_eq!(observe(&mut arm, &drawer), Some(drawer));
    }

    #[test]
    fn the_inbox_queues_each_session_once() {
        let inbox = EdgeInbox::default();
        assert!(inbox.is_empty());
        inbox.clone().push(7);
        inbox.push(9);
        inbox.push(7);
        assert_eq!(inbox.take(), vec![7, 9]);
        assert!(inbox.is_empty());
    }

    fn cuts(scan: &mut CScan, batch: &[u8]) -> Vec<usize> {
        let mut ends = Vec::new();
        scan.ends(batch, &mut ends);
        ends
    }

    /// The C-cut: right after each complete 133/633 C by either terminator,
    /// never at A/B/D, an unterminated or aborted one, or a lookalike; one
    /// split across batches is cut in the batch that finishes it.
    #[test]
    fn a_slice_ends_right_after_each_complete_c() {
        let mut scan = CScan::default();
        let c = b"\x1b]133;C;id=ab\x07";
        let batch = [&b"ls\r\n"[..], c, b"out", b"\x1b]633;C\x1b\\", b"x"].concat();
        assert_eq!(
            cuts(&mut scan, &batch),
            vec![4 + c.len(), 4 + c.len() + 3 + 9]
        );
        for no in [
            &b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;D;0\x07"[..],
            b"\x1b]133;CX\x07",
            b"\x1b]133;C;id=\x18x",
            b"\x1b]1337;C\x07",
            b"\x1b[31m\x1b]0;133;C\x07",
        ] {
            assert_eq!(
                cuts(&mut CScan::default(), no),
                Vec::<usize>::new(),
                "{no:?}"
            );
        }
        // Split across batches: the cut is in the second, where it ends.
        let mut scan = CScan::default();
        assert!(cuts(&mut scan, b"abc\x1b]133;C;id=0").is_empty());
        assert_eq!(cuts(&mut scan, b"123\x07next"), vec![4]);
        // Split inside the head.
        let mut scan = CScan::default();
        assert!(cuts(&mut scan, b"abc\x1b]13").is_empty());
        assert_eq!(cuts(&mut scan, b"3;C\x07"), vec![4]);
        // A carry that was not a C leaves the next batch's own cuts.
        let mut scan = CScan::default();
        assert!(cuts(&mut scan, b"\x1b]13").is_empty());
        assert_eq!(cuts(&mut scan, b"4;x\x07\x1b]133;C\x07"), vec![12]);
        // Split right after the ESC: the `]` the search finds is the next
        // batch's first byte.
        let mut scan = CScan::default();
        assert!(cuts(&mut scan, b"abc\x1b").is_empty());
        assert_eq!(cuts(&mut scan, b"]133;C\x07z"), vec![7]);
        // A `]` that opens no OSC, and colour runs, leave the C after them.
        let noisy = [&b"a[1] \x1b[31mred\x1b[0m ]]"[..], c].concat();
        assert_eq!(cuts(&mut CScan::default(), &noisy), vec![noisy.len()]);
        // Found at every alignment of the block scan.
        for pad in 0..70 {
            let batch = [vec![b'.'; pad], c.to_vec()].concat();
            assert_eq!(
                cuts(&mut CScan::default(), &batch),
                vec![pad + c.len()],
                "pad {pad}"
            );
        }
        let arm = ByteArm {
            ends: vec![2, 7, 9],
            ..ByteArm::default()
        };
        assert_eq!(arm.next_cut(0), Some(2));
        assert_eq!(arm.next_cut(2), Some(7));
        assert_eq!(arm.next_cut(8), Some(9));
        assert_eq!(arm.next_cut(9), None);
        assert_eq!(arm.next_cut(usize::MAX), None);
    }
}
