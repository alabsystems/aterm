// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Foreground process-image inspection and title/prompt tracking for a session's PTY.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use atpkg::image::{ImageBuild, ImageRoots, ImageSource};

/// Whether the dev timeline rows are recorded: debug builds and an optimized `dev-seams` QA build.
pub(crate) const DEV_ROWS: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// How many batches the dev `card-seen` probe looks for one drawer token's card before it gives up.
pub(crate) const CARD_PROBE_BATCHES: u16 = 256;

/// Whether this process runs the dev `card-seen` probe (`SessionFactory::card_probe`), read
/// through [`aterm_types::dev_seam!`], which compiles to `None` exactly where [`DEV_ROWS`] is off.
pub(crate) fn card_probe_requested() -> bool {
    aterm_types::dev_seam!("ATERM_DEV_CARD_PROBE").is_some_and(|v| v == "1")
}

const C_MAX: usize = 512;

/// A program that draws a launch card the harness decorates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CardProgram {
    Claude,
    Agy,
    Muse,
}

impl CardProgram {
    /// The card drawer `program` names (`aterm_phase::program_of`: `claude`, `claude-code`, `agy`, `muse`, `muse-bin-<version>`), or `None`.
    pub(crate) fn of(program: &str) -> Option<Self> {
        Self::from_program(aterm_phase::program_of(program)?)
    }

    /// The card drawer aterm-phase's `program` is, or `None`.
    pub(crate) const fn from_program(program: aterm_phase::Program) -> Option<Self> {
        match program {
            aterm_phase::Program::Claude => Some(Self::Claude),
            aterm_phase::Program::Agy => Some(Self::Agy),
            aterm_phase::Program::Muse => Some(Self::Muse),
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
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

    /// Which card reader reads this drawer's card (`claude_card::CardReading::rows_kind`).
    pub(crate) const fn rows_kind(self) -> usize {
        match self {
            Self::Claude => 3,
            Self::Agy => 4,
            Self::Muse => 1,
        }
    }

    /// What this drawer's banner says about its build, as aterm-phase declares it beside the reader.
    pub(crate) const fn banner_build(self) -> aterm_phase::BannerBuild {
        match self {
            Self::Claude => aterm_phase::turn::LAUNCH_CARD_BUILD,
            Self::Agy => aterm_phase::agy::LAUNCH_CARD_BUILD,
            Self::Muse => aterm_phase::muse::LAUNCH_CARD_BUILD,
        }
    }

    /// The rows this drawer's reader reads of a card from its banner down.
    pub(crate) const fn card_rows(self) -> usize {
        match self {
            Self::Claude => aterm_phase::turn::LAUNCH_CARD_ROWS,
            Self::Agy => aterm_phase::agy::LAUNCH_CARD_ROWS,
            Self::Muse => aterm_phase::muse::LAUNCH_CARD_ROWS,
        }
    }

    /// The gate this drawer's card is held to under `token`, its banner's build held to `build`.
    pub(crate) const fn gate(self, token: u64, build: GateBuild) -> CardGate {
        CardGate {
            token,
            kind: self.rows_kind(),
            rows: self.card_rows(),
            build,
        }
    }
}

/// THE CARD'S GATE for the current holder image (card engine §3.8): the drawer's token, the card
/// reader its card must come from, the rows that reader reads, and the build policy the card's
/// banner is held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CardGate {
    pub(crate) token: u64,
    /// The reader ([`CardProgram::rows_kind`]).
    pub(crate) kind: usize,
    /// The rows the fence must admit from the banner down ([`CardProgram::card_rows`]).
    pub(crate) rows: usize,
    pub(crate) build: GateBuild,
}

impl CardGate {
    /// The repaint key's term for the gate (`App::claude_footer_fp`).
    pub(crate) const fn key(&self) -> u64 {
        let (policy, build) = match self.build {
            GateBuild::Exact(b) => (1, b.build_id()),
            GateBuild::Registry(b) => (2, b.build_id()),
            GateBuild::ProvenanceOnly => (3, 0),
            GateBuild::None => (4, 0),
        };
        self.token
            ^ build.rotate_left(17)
            ^ ((self.kind as u64) << 8 | (self.rows as u64) << 16 | policy).rotate_left(41)
    }
}

/// What a card's banner build is held to ([`HolderImage::gate_build`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GateBuild {
    /// The image's own build.
    Exact(atpkg::vendor_direct::Version),
    /// An image of unknown build (brew, a dev link, an ALab build without its record) of a program whose banner prints its build.
    Registry(atpkg::vendor_direct::Version),
    /// A program whose banner is not known to print its build: the fence alone decides.
    ProvenanceOnly,
    /// No build to hold the banner to.
    None,
}

/// The process group each holder-fence mark's drawer image leads (card engine §3.7). Marks are
/// retired by liveness: when the process watch hears a stopped drawer's leader exit
/// (`exec_edges::ExecEdges::take_gone`), the parse stage retires its mark
/// (`Terminal::retire_holder_mark`), so dead stopped drawers never push a live one's mark out
/// past [`aterm_core::terminal::MAX_HOLDER_MARKS`]. A cut to a drawer image (`spawn::note_cut`)
/// and a restore's adoption (`spawn::adopt_restored_holder`) note the group of each mark they
/// lay; the session timeline keeps the book, so a reattached reader still has it.
#[derive(Debug, Default)]
pub(crate) struct DrawerGroups(Vec<(i32, u64)>);

impl DrawerGroups {
    /// `led` (a group, and the token of the drawer image leading it) holds a mark, and `marks` are
    /// every mark the fence holds now: the groups of marks gone are forgotten.
    pub(crate) fn note(&mut self, led: Option<(i32, u64)>, marks: &[u64]) {
        if let Some((pgid, token)) = led.filter(|&(pgid, _)| pgid > 1) {
            self.0.retain(|&(p, t)| p != pgid && t != token);
            self.0.push((pgid, token));
        }
        self.0.retain(|(_, t)| marks.contains(t));
    }

    /// The mark whose drawer led `pid`'s group, if any.
    pub(crate) fn led_by(&self, pid: i32) -> Option<u64> {
        self.0.iter().find(|&&(p, _)| p == pid).map(|&(_, t)| t)
    }
}

/// What one term-lock hold's holder cuts leave [`DrawerGroups`] to note, applied once the hold
/// ends, so the timeline's lock is never taken under the terminal's.
#[derive(Debug, Default)]
pub(crate) struct GroupNotes {
    led: Vec<(i32, u64)>,
    marks: Option<Vec<u64>>,
}

impl GroupNotes {
    /// A cut whose incoming drawer leads `led` left the fence holding `marks`.
    pub(crate) fn cut(&mut self, led: Option<(i32, u64)>, marks: impl Iterator<Item = u64>) {
        self.led.extend(led);
        self.marks = Some(marks.collect());
    }

    /// Note every cut booked, in order, into `groups`.
    pub(crate) fn apply(self, groups: &mut DrawerGroups) {
        let Some(marks) = self.marks else { return };
        for led in self.led {
            groups.note(Some(led), &marks);
        }
        groups.note(None, &marks);
    }

    /// Whether any cut was booked.
    pub(crate) const fn is_empty(&self) -> bool {
        self.marks.is_none()
    }
}

/// How the holder's image changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EdgeKind {
    /// The foreground group changed.
    Group,
    /// The group's image changed in place.
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
    /// A hash of pid, process start, executable and classified build ([`token_of`]).
    pub(crate) token: Option<u64>,
    /// The group (its leader's pid) the image was read for.
    pub(crate) pgid: i32,
    /// The leader's start, in microseconds on the platform's start clock.
    pub(crate) start_us: u64,
    /// The executable the kernel mapped, canonical.
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
    /// The image of `pgid` from its parts.
    pub(crate) fn of(
        pgid: i32,
        start_us: Option<u64>,
        exe: Option<PathBuf>,
        build: ImageBuild,
        read_ns: u64,
    ) -> Self {
        #[cfg(target_os = "linux")]
        let exe = exe.map(|e| kept_exe(&e).to_path_buf());
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

    /// An image nothing could be read of: the inert arm the test App's factory carries
    /// (`unread`), and what a read of a gone group gives.
    pub(crate) fn unread(pgid: i32) -> Self {
        Self::of(pgid, None, None, ImageBuild::unknown(None), 0)
    }

    /// Whether an edge into (or out of) this image is the card's business.
    pub(crate) const fn card_relevant(&self) -> bool {
        self.drawer.is_some() || self.launcher_for.is_some()
    }

    /// Whether the represented image descriptions agree.
    pub(crate) fn same_image(&self, other: &Self) -> bool {
        self.token == other.token
            && self.pgid == other.pgid
            && self.start_us == other.start_us
            && self.exe == other.exe
            && self.build == other.build
    }

    fn is_process(&self, pgid: i32, start_us: Option<u64>, exe: &Path) -> bool {
        self.token.is_some()
            && self.pgid == pgid
            && start_us == Some(self.start_us)
            && *self.exe == *kept_exe(exe)
    }

    fn script_shell(&self) -> bool {
        is_script_shell(&self.exe)
    }

    /// THE BUILD POLICY (card engine §3.8).
    pub(crate) fn gate_build(
        &self,
        registry: impl FnOnce() -> Option<atpkg::vendor_direct::Version>,
    ) -> Option<GateBuild> {
        let drawer = self.drawer?;
        Some(match drawer.banner_build() {
            aterm_phase::BannerBuild::ProvenanceOnly => GateBuild::ProvenanceOnly,
            aterm_phase::BannerBuild::Exact => match (self.build.source, self.build.version()) {
                (
                    ImageSource::Store | ImageSource::StoreRecord | ImageSource::Native,
                    Some(own),
                ) => GateBuild::Exact(own),
                _ => registry().map_or(GateBuild::None, GateBuild::Registry),
            },
        })
    }

    /// [`CardGate`] for this image: a drawer with a token, its build held to [`Self::gate_build`].
    pub(crate) fn gate(
        &self,
        registry: impl FnOnce() -> Option<atpkg::vendor_direct::Version>,
    ) -> Option<CardGate> {
        let drawer = self.drawer?;
        Some(drawer.gate(self.token?, self.gate_build(registry)?))
    }

    /// A matching family and known version on the screen.
    pub(crate) fn matches_card(&self, rows_kind: usize, version: &str) -> bool {
        let Some(expected_kind) = self.drawer.map(CardProgram::rows_kind) else {
            return false;
        };
        rows_kind == expected_kind
            && self.build.version().is_some_and(|expected| {
                atpkg::vendor_direct::Version::parse(version) == Some(expected)
            })
    }

    /// `holder-image`'s payload: `token= pgid= program= build= source= drawer= launcher_for=`.
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

fn kept_exe(exe: &Path) -> &Path {
    #[cfg(target_os = "linux")]
    let exe = atpkg::image::strip_deleted(exe);
    exe
}

fn is_script_shell(exe: &Path) -> bool {
    exe.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| atpkg::image::SCRIPT_SHELLS.contains(&n))
}

/// A token as a timeline row prints it: 16 hex digits, or `-`.
pub(crate) fn token_word(token: Option<u64>) -> String {
    token.map_or_else(|| "-".to_string(), |t| format!("{t:016x}"))
}

/// THE SNAPSHOT TOKEN (I5): FNV-1a/64 of the pid, process start, executable and classification.
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

/// A holder image read (`SessionFactory::edge_image`).
pub(crate) type ReadImage = fn(i32, Option<&HolderImage>) -> HolderImage;

/// The test App's inert `SessionFactory::edge_image` (and ConPTY's): reads nothing.
#[cfg(any(test, feature = "bench-support", not(unix)))]
pub(crate) fn unread(pgid: i32, _last: Option<&HolderImage>) -> HolderImage {
    HolderImage::unread(pgid)
}

/// THE READ (shipping `SessionFactory::edge_image`), on the gather thread at an edge, or on the parse stage for the byte arm's re-read.
pub(crate) fn read(pgid: i32, last: Option<&HolderImage>) -> HolderImage {
    let read_ns = crate::metrics::now_ns();
    let Some(pid) = u32::try_from(pgid).ok().filter(|p| *p > 1) else {
        return HolderImage::unread(pgid);
    };
    let Some(exe) = aterm_sysprobe::process_path(pid) else {
        return HolderImage::of(pgid, None, None, ImageBuild::unknown(None), read_ns);
    };
    let start_us = start_us_of(pgid);
    if let Some(last) = last.filter(|l| l.is_process(pgid, start_us, &exe)) {
        return HolderImage {
            read_ns,
            ..last.clone()
        };
    }
    let argv1 = is_script_shell(atpkg::image::strip_deleted(&exe))
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

struct RootsCache {
    config: Stamp,
    prefix: Stamp,
    roots: Option<Arc<ImageRoots>>,
}

static ROOTS: Mutex<Option<RootsCache>> = Mutex::new(None);

/// The managed prefix's [`ImageRoots`], resolved once and again only when `aterm.toml` or the prefix directory changes.
pub(crate) fn roots() -> Option<Arc<ImageRoots>> {
    let config = atpkg::config::config_path().and_then(|p| stamp(&p));
    let cached = ROOTS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|c| (c.config, c.prefix, c.roots.clone()));
    if let Some((was_config, was_prefix, roots)) = cached
        && was_config == config
        && was_prefix == roots.as_ref().and_then(|r| stamp(&r.layout().prefix))
    {
        return roots;
    }
    let roots = atpkg::store::resolve_configured().map(|layout| Arc::new(ImageRoots::new(&layout)));
    let prefix = roots.as_ref().and_then(|r| stamp(&r.layout().prefix));
    *ROOTS.lock().unwrap_or_else(|p| p.into_inner()) = Some(RootsCache {
        config,
        prefix,
        roots: roots.clone(),
    });
    roots
}

/// What the timeline made of an offered image (`SessionTimeline::offer_holder_image`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    /// Published: it is the session's holder image now.
    Published,
    /// The image already published: nothing moved.
    Same,
    /// Neither it nor the image it would replace is the card's business.
    Irrelevant,
}

/// THE PUBLICATION RULE, pure.
pub(crate) fn decide(current: Option<&HolderImage>, offered: &HolderImage) -> Offer {
    if !offered.card_relevant() && !current.is_some_and(HolderImage::card_relevant) {
        return Offer::Irrelevant;
    }
    if current.is_some_and(|c| c.same_image(offered)) {
        return Offer::Same;
    }
    Offer::Published
}

/// THE EDGED INBOX (§3.5).
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

/// THE C-CUT SCAN (§3.6).
#[derive(Debug, Default)]
pub(crate) struct CScan {
    carry: Vec<u8>,
}

impl CScan {
    /// Clear `ends` and fill it with the cut offsets in `batch`, ascending, each in `1..=batch.len()`.
    pub(crate) fn ends(&mut self, batch: &[u8], ends: &mut Vec<usize>) {
        ends.clear();
        let mut at = 0;
        if !self.carry.is_empty() {
            match self.finish_carry(batch, ends) {
                Some(next) => at = next,
                None => return,
            }
        }
        while let Some(esc) = find_osc(batch, at) {
            match osc_c_end(batch, esc) {
                OscEnd::C { end, resume } => {
                    ends.push(end);
                    at = resume;
                }
                OscEnd::Open => {
                    self.carry.extend_from_slice(&batch[esc..]);
                    return;
                }
                OscEnd::NotC => at = esc + 1,
            }
        }
        if batch.len() > at && batch.last() == Some(&0x1b) {
            self.carry.push(0x1b);
        }
    }

    fn finish_carry(&mut self, batch: &[u8], ends: &mut Vec<usize>) -> Option<usize> {
        let mut joined = std::mem::take(&mut self.carry);
        let carried = joined.len();
        let take = batch.len().min(C_MAX);
        joined.extend_from_slice(&batch[..take]);
        let next = match osc_c_end(&joined, 0) {
            OscEnd::C { end, resume } => {
                if end > carried {
                    ends.push(end - carried);
                }
                resume
            }
            OscEnd::Open => {
                debug_assert_eq!(take, batch.len(), "an open C fits its bound");
                self.carry = joined;
                return None;
            }
            OscEnd::NotC => 1,
        };
        Some(next.saturating_sub(carried))
    }
}

/// What [`osc_c_end`] found at an ESC.
#[derive(Debug, PartialEq, Eq)]
enum OscEnd {
    /// A complete OSC 133/633 `C`, dispatched by the engine once `end` bytes
    /// are parsed: one past its BEL, its ST's `\`, or the ESC that broke it
    /// off.
    C { end: usize, resume: usize },
    /// A prefix of one, cut by the end of the bytes.
    Open,
    /// Anything else.
    NotC,
}

fn osc_c_end(bytes: &[u8], esc: usize) -> OscEnd {
    enum Part {
        /// The number: whether a `+` or a digit came, and its value.
        Number {
            started: bool,
            digits: bool,
            value: u32,
        },
        /// The second parameter's bytes so far.
        Code(Vec<u8>),
        /// Past it, to the terminator.
        Rest,
    }
    match bytes.get(esc + 1) {
        Some(b']') => {}
        Some(_) => return OscEnd::NotC,
        None => return OscEnd::Open,
    }
    let limit = esc + C_MAX;
    let mut part = Part::Number {
        started: false,
        digits: false,
        value: 0,
    };
    let mut i = esc + 2;
    loop {
        if i >= limit {
            return OscEnd::NotC;
        }
        let Some(&b) = bytes.get(i) else {
            return OscEnd::Open;
        };
        match b {
            0x07 | 0x1b => {
                let is_c = match &part {
                    Part::Number { .. } => false,
                    Part::Code(code) => is_c_code(code),
                    Part::Rest => true,
                };
                if !is_c {
                    return OscEnd::NotC;
                }
                return match (b, bytes.get(i + 1)) {
                    (0x07, _) => OscEnd::C {
                        end: i + 1,
                        resume: i + 1,
                    },
                    (_, Some(b'\\')) => OscEnd::C {
                        end: i + 2,
                        resume: i + 2,
                    },
                    _ => OscEnd::C {
                        end: i + 1,
                        resume: i,
                    },
                };
            }
            0x18 | 0x1a => return OscEnd::NotC,
            0x00..=0x1f => {}
            b';' => match &part {
                Part::Number {
                    digits: true,
                    value: 133 | 633,
                    ..
                } => part = Part::Code(Vec::new()),
                Part::Number { .. } => return OscEnd::NotC,
                Part::Code(code) if is_c_code(code) => part = Part::Rest,
                Part::Code(_) => return OscEnd::NotC,
                Part::Rest => {}
            },
            _ => match &mut part {
                Part::Number {
                    started,
                    digits,
                    value,
                } => {
                    if b == b'+' && !*started {
                        *started = true;
                    } else if let Some(v) = b
                        .is_ascii_digit()
                        .then(|| value.checked_mul(10)?.checked_add(u32::from(b - b'0')))
                        .flatten()
                    {
                        (*started, *digits, *value) = (true, true, v);
                    } else {
                        return OscEnd::NotC;
                    }
                }
                Part::Code(code) => {
                    if code.is_empty() && b != b'C' {
                        return OscEnd::NotC;
                    }
                    code.push(b);
                }
                Part::Rest => {}
            },
        }
        i += 1;
    }
}

fn is_c_code(code: &[u8]) -> bool {
    code.first() == Some(&b'C') && std::str::from_utf8(code).is_ok()
}

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
    /// The image last read or published here, which a re-read of its group
    /// keeps while its process is unchanged ([`read`]).
    last: Option<Arc<HolderImage>>,
    /// The C-cut scan and this batch's cuts.
    pub(crate) scan: CScan,
    pub(crate) ends: Vec<usize>,
    /// The reader hears the kernel's `exec`s (card engine §3.3): the arm makes
    /// its initial read alone, with no C cut and no re-read, since `NOTE_EXEC`
    /// orders every later one.
    kernel: bool,
}

impl Default for ByteArm {
    fn default() -> Self {
        Self {
            initial: true,
            unresolved: false,
            boundary_due: false,
            batch_due: false,
            launcher: 0,
            last: None,
            scan: CScan::default(),
            ends: Vec::new(),
            kernel: false,
        }
    }
}

impl ByteArm {
    /// The arm of a reader starting on `timeline`.
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

    /// This arm, for a reader that hears the kernel's `exec`s: its initial read alone.
    pub(crate) fn kernel_edges(self) -> Self {
        Self {
            kernel: true,
            launcher: 0,
            unresolved: false,
            ..self
        }
    }

    /// Whether the batch's C cuts are scanned (not under kernel edges).
    pub(crate) const fn scans(&self) -> bool {
        !self.kernel
    }

    /// The image this arm last noted for `pgid`, if that is the group it last noted.
    pub(crate) fn last_of(&self, pgid: i32) -> Option<&HolderImage> {
        self.last.as_deref().filter(|l| l.pgid == pgid)
    }

    /// The first cut after `off` in this batch, or `None`.
    pub(crate) fn next_cut(&self, off: usize) -> Option<usize> {
        self.ends
            .get(self.ends.partition_point(|&e| e <= off))
            .copied()
    }

    /// Allow one ordinary read in this batch.
    pub(crate) const fn begin_batch(&mut self) {
        self.batch_due = true;
    }

    /// Read the initial holder, an unresolved signed command, or a launcher.
    pub(crate) fn read_due(
        &mut self,
        holder: i32,
        read: impl FnOnce(i32, Option<&HolderImage>) -> HolderImage,
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
        let last = self.last.as_deref().filter(|l| l.pgid == holder);
        Some(read(holder, last))
    }

    /// A gather read or re-read satisfies this batch's observation even if publication was irrelevant.
    pub(crate) fn note_read(&mut self, image: &Arc<HolderImage>) {
        self.initial = false;
        self.boundary_due = false;
        self.batch_due = false;
        self.last = Some(Arc::clone(image));
        if !self.kernel && !image.exe.as_os_str().is_empty() {
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

    /// The engine's accepted command mark and resulting shell state, read under its existing hold.
    pub(crate) const fn note_shell(&mut self, signed_c: bool, executing: bool) {
        if self.kernel {
            return;
        }
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

/// THE I1 TEST SEAMS: what a watched session's timeline held inside each slice's term-lock hold, and the mutant that publishes after the hold.
#[cfg(test)]
pub(crate) mod hold_hook {
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// One hold: the slice's start offset in its batch, the token the
    /// timeline held then, and the engine fence's current mark as the
    /// slice's bytes were parsed.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct Hold {
        pub(crate) off: usize,
        pub(crate) bytes: Vec<u8>,
        pub(crate) token: Option<u64>,
        pub(crate) fence: Option<u64>,
    }

    /// The controls' mutants a watched session runs.
    #[derive(Clone, Copy, Debug, Default)]
    pub(crate) struct Knobs {
        /// Publish the holder image after the hold of its slice, not before.
        pub(crate) publish_after_hold: bool,
        /// The byte arm without its cut after an OSC 133 C.
        pub(crate) no_c_cut: bool,
        /// The parse stage drops the process watch's exits, so no stopped drawer's mark retires.
        pub(crate) no_retire: bool,
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

    /// Whether `session` runs the no-retire mutant.
    pub(crate) fn no_retire(session: u64) -> bool {
        knobs(session).no_retire
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

    /// How many holds `session` has recorded so far: a test's `post` stamps
    /// each wake with it, to order the wake against the holds.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix reader tests watch a session")
    )]
    pub(crate) fn holds_so_far(session: u64) -> usize {
        WATCHED
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|m| m.get(&session))
            .map_or(0, |w| w.holds.len())
    }

    type AfterRead = Box<dyn FnOnce() + Send>;

    static AFTER_READ: Mutex<Option<HashMap<u64, AfterRead>>> = Mutex::new(None);

    /// Run `act` in `session`'s gather right after its next read.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix reader tests watch a session")
    )]
    pub(crate) fn on_next_read(session: u64, act: impl FnOnce() + Send + 'static) {
        AFTER_READ
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(session, Box::new(act));
    }

    /// The gather read a batch's first bytes: run what a test armed.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix gather reads a master")
    )]
    pub(crate) fn after_read(session: u64) {
        let act = AFTER_READ
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
            .and_then(|m| m.remove(&session));
        if let Some(act) = act {
            act();
        }
    }

    /// Inside a hold: record what `token` reads and the engine fence's mark,
    /// for a watched session.
    pub(crate) fn at_hold(
        session: u64,
        off: usize,
        bytes: &[u8],
        fence: Option<u64>,
        token: impl FnOnce() -> Option<u64>,
    ) {
        let mut watched = WATCHED.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(w) = watched.as_mut().and_then(|m| m.get_mut(&session)) {
            w.holds.push(Hold {
                off,
                bytes: bytes.to_vec(),
                token: token(),
                fence,
            });
        }
    }
}

/// The byte arm's C cut, timed (`benches/c_cut_cost.rs`, `bench-support` only).
#[cfg(feature = "bench-support")]
pub(crate) mod cut_cost {
    use super::ByteArm;

    const SLICE: usize = 16 * 1024;

    struct Pass {
        mbps: f64,
        cuts: usize,
        reads: usize,
    }

    fn pass(corpus: &[u8], batch: usize, arm: Option<i32>) -> Pass {
        let term = std::sync::Mutex::new(aterm_core::terminal::Terminal::new(24, 80));
        let mut state = ByteArm::default();
        if arm.is_some_and(|h| h > 0) {
            state.note_shell(true, true);
        }
        let (mut cuts, mut reads) = (0, 0);
        let t0 = std::time::Instant::now();
        for b in corpus.chunks(batch.max(1)) {
            if arm.is_some() {
                let ByteArm { scan, ends, .. } = &mut state;
                scan.ends(b, ends);
                cuts += ends.len();
                state.begin_batch();
            }
            let mut off = 0;
            while off < b.len() {
                let cut = arm.and_then(|_| state.next_cut(off));
                let limit = cut.unwrap_or(b.len());
                let end = off + (limit - off).min(SLICE);
                if let Some(holder) = arm.filter(|h| *h > 0)
                    && let Some(image) = state.read_due(holder, super::read)
                {
                    reads += 1;
                    state.note_read(&std::sync::Arc::new(std::hint::black_box(image)));
                }
                crate::term_lock(&term).process(std::hint::black_box(&b[off..end]));
                if arm.is_some_and(|h| h > 0) {
                    state.note_shell(cut == Some(end), true);
                }
                off = end;
            }
        }
        let secs = t0.elapsed().as_secs_f64().max(f64::MIN_POSITIVE);
        std::hint::black_box(crate::term_lock(&term).cursor().col);
        Pass {
            mbps: corpus.len() as f64 / 1e6 / secs,
            cuts,
            reads,
        }
    }

    /// Median MB/s of `runs` passes without the arm and with it (alternating, after one untimed pass of each), and the cuts and image reads one pass with it made.
    pub(crate) fn c_cut_cost(
        corpus: &[u8],
        batch: usize,
        runs: usize,
        holder: i32,
    ) -> (f64, f64, usize, usize) {
        let _ = pass(corpus, batch, None);
        let _ = pass(corpus, batch, Some(holder));
        let (mut off, mut on, mut cuts, mut reads) = (Vec::new(), Vec::new(), 0, 0);
        for _ in 0..runs.max(1) {
            off.push(pass(corpus, batch, None).mbps);
            let made = pass(corpus, batch, Some(holder));
            on.push(made.mbps);
            (cuts, reads) = (made.cuts, made.reads);
        }
        (median(&mut off), median(&mut on), cuts, reads)
    }

    /// The shipping image read of `pid`, timed `reads` times (after one untimed read).
    pub(crate) fn read_cost(pid: i32, reads: usize, kept: bool) -> (f64, f64, f64, String) {
        let first = super::read(pid, None);
        let last = kept.then_some(&first);
        let mut us: Vec<f64> = (0..reads.max(1))
            .map(|_| {
                let t0 = std::time::Instant::now();
                std::hint::black_box(super::read(std::hint::black_box(pid), last));
                t0.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        us.sort_by(f64::total_cmp);
        let at = |q: f64| us[((us.len() - 1) as f64 * q).round() as usize];
        (at(0.50), at(0.95), at(0.99), first.payload())
    }

    fn median(v: &mut [f64]) -> f64 {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atpkg::image::ProgramTag;
    use atpkg::vendor_direct::Version;

    /// The group book holds one token per group and only the fence's marks: an `exec` in a
    /// group's leader re-notes it, and a mark the fence dropped (by a cut or a retirement)
    /// takes its group with it, so the book never outgrows the fence's eight marks.
    #[test]
    fn the_group_book_holds_only_the_fences_marks() {
        let mut groups = DrawerGroups::default();
        groups.note(Some((10, 1)), &[1]);
        groups.note(Some((10, 2)), &[2]);
        assert_eq!(
            (groups.led_by(10), groups.0.len()),
            (Some(2), 1),
            "the exec'd image"
        );
        groups.note(Some((11, 3)), &[2, 3]);
        groups.note(Some((1, 4)), &[2, 3, 4]);
        assert_eq!(groups.led_by(1), None, "no group is led by pid 1");
        for token in 5..40 {
            groups.note(Some((10 + token as i32, token)), &[3, token]);
        }
        assert_eq!(groups.0.len(), 2, "only the marks: {:?}", groups.0);
        assert_eq!((groups.led_by(11), groups.led_by(10)), (Some(3), None));
        let mut notes = GroupNotes::default();
        assert!(notes.is_empty());
        notes.cut(Some((50, 50)), [39, 50].into_iter());
        notes.cut(None, [50].into_iter());
        notes.apply(&mut groups);
        assert_eq!(
            groups.0,
            [(50, 50)],
            "a hold's cuts apply in order, the last fence's marks kept"
        );
    }

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
        let read =
            |exe: &str| HolderImage::of(100, Some(5), Some(PathBuf::from(exe)), build.clone(), 0);
        let unlinked = read("/s/versions/2.1.304 (deleted)");
        #[cfg(target_os = "linux")]
        {
            assert_eq!(unlinked.token, read("/s/versions/2.1.304").token);
            assert_eq!(&*unlinked.exe, Path::new("/s/versions/2.1.304"));
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert_ne!(unlinked.token, read("/s/versions/2.1.304").token);
            assert_eq!(&*unlinked.exe, Path::new("/s/versions/2.1.304 (deleted)"));
        }
    }

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
            let image = arm.read_due(image.pgid, |_, _| read(image));
            if let Some(image) = &image {
                arm.note_read(&Arc::new(image.clone()));
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
        arm.note_read(&Arc::new(shim.clone()));
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
    fn the_arm_gives_its_read_the_groups_last_image() {
        let shell = Arc::new(image(10, 1, "/bin/zsh", "zsh", ImageSource::Unknown));
        let given = std::cell::RefCell::new(Vec::new());
        let read = |pgid: i32, last: Option<&HolderImage>| {
            given.borrow_mut().push(last.cloned());
            HolderImage::unread(pgid)
        };
        let mut arm = ByteArm::default();
        arm.begin_batch();
        assert!(arm.read_due(10, read).is_some());
        arm.note_read(&shell);
        arm.note_shell(true, true);
        assert!(arm.read_due(10, read).is_some(), "the boundary read");
        arm.note_read(&shell);
        arm.note_shell(true, true);
        assert!(arm.read_due(11, read).is_some(), "another group's");
        assert_eq!(
            *given.borrow(),
            vec![None, Some((*shell).clone()), None],
            "nothing noted yet; the group's own last; never another group's"
        );
    }

    #[test]
    // Where a process's start is read ([`start_us_of`]); elsewhere no image is tracked.
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    fn a_reread_of_an_unchanged_process_keeps_its_image() {
        let pid = i32::try_from(std::process::id()).expect("a pid");
        let fresh = read(pid, None);
        assert!(
            fresh.token.is_some(),
            "PRECONDITION: this process is readable"
        );
        assert!(fresh.launcher_for.is_none(), "PRECONDITION: not a launcher");
        let kept = HolderImage {
            build: ImageBuild {
                program: atpkg::image::ProgramTag::new("sh"),
                build: 0,
                source: ImageSource::Launcher,
            },
            launcher_for: Some(CardProgram::Claude),
            read_ns: 0,
            ..fresh.clone()
        };
        let again = read(pid, Some(&kept));
        assert!(again.same_image(&kept), "kept: {again:?}");
        assert!(again.read_ns > 0, "re-stamped with this read");
        for (why, last) in [
            (
                "another start",
                HolderImage {
                    start_us: kept.start_us + 1,
                    ..kept.clone()
                },
            ),
            (
                "another executable",
                HolderImage {
                    exe: Arc::from(Path::new("/bin/zsh")),
                    ..kept.clone()
                },
            ),
            (
                "another group",
                HolderImage {
                    pgid: pid + 1,
                    ..kept.clone()
                },
            ),
            (
                "untracked",
                HolderImage {
                    token: None,
                    ..kept.clone()
                },
            ),
        ] {
            assert!(
                read(pid, Some(&last)).same_image(&fresh),
                "{why}: read afresh"
            );
        }
    }

    #[test]
    fn a_kept_image_reads_the_executable_as_its_token_does() {
        let last = image(10, 1, "/s/versions/2.1.304", "claude", ImageSource::Native);
        let unlinked = Path::new("/s/versions/2.1.304 (deleted)");
        assert!(last.is_process(10, Some(1), Path::new("/s/versions/2.1.304")));
        let same_token = HolderImage::of(
            10,
            Some(1),
            Some(unlinked.to_path_buf()),
            last.build.clone(),
            0,
        )
        .same_image(&last);
        assert_eq!(same_token, cfg!(target_os = "linux"));
        assert_eq!(last.is_process(10, Some(1), unlinked), same_token);
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

    #[test]
    fn a_slice_ends_right_after_each_complete_c() {
        let mut scan = CScan::default();
        let c = b"\x1b]133;C;id=ab\x07";
        let batch = [&b"ls\r\n"[..], c, b"out", b"\x1b]633;C\x1b\\", b"x"].concat();
        assert_eq!(
            cuts(&mut scan, &batch),
            vec![4 + c.len(), 4 + c.len() + 3 + 9]
        );
        assert_eq!(cuts(&mut CScan::default(), b"\x1b]133;CX\x07"), vec![9]);
        for no in [
            &b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;D;0\x07"[..],
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
        let mut scan = CScan::default();
        assert!(cuts(&mut scan, b"abc\x1b]133;C;id=0").is_empty());
        assert_eq!(cuts(&mut scan, b"123\x07next"), vec![4]);
        let mut scan = CScan::default();
        assert!(cuts(&mut scan, b"abc\x1b]13").is_empty());
        assert_eq!(cuts(&mut scan, b"3;C\x07"), vec![4]);
        let mut scan = CScan::default();
        assert!(cuts(&mut scan, b"\x1b]13").is_empty());
        assert_eq!(cuts(&mut scan, b"4;x\x07\x1b]133;C\x07"), vec![12]);
        let mut scan = CScan::default();
        assert!(cuts(&mut scan, b"abc\x1b").is_empty());
        assert_eq!(cuts(&mut scan, b"]133;C\x07z"), vec![7]);
        let noisy = [&b"a[1] \x1b[31mred\x1b[0m ]]"[..], c].concat();
        assert_eq!(cuts(&mut CScan::default(), &noisy), vec![noisy.len()]);
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

    /// THE ORACLE: where the engine's parser dispatches each OSC the engine
    /// takes as a 133/633 `C` (`TerminalHandler::osc_dispatch_inner`'s command
    /// number, `parse_shell_osc`'s first character), byte by byte.
    #[derive(Default)]
    struct EngineCs {
        at: usize,
        dispatched: Vec<usize>,
    }

    impl aterm_parser::ActionSink for EngineCs {
        fn print(&mut self, _: char) {}
        fn execute(&mut self, _: u8) {}
        fn csi_dispatch(
            &mut self,
            _: &aterm_provenance::Provenance<[u16], aterm_provenance::Pty>,
            _: &aterm_provenance::Provenance<[u8], aterm_provenance::Pty>,
            _: u8,
        ) {
        }
        fn osc_dispatch(
            &mut self,
            params: &aterm_provenance::Provenance<[&[u8]], aterm_provenance::Pty>,
        ) {
            let params = params.as_ref();
            let number = params
                .first()
                .and_then(|p| std::str::from_utf8(p).ok())
                .and_then(|s| s.parse::<u32>().ok());
            let code = params
                .get(1)
                .and_then(|p| std::str::from_utf8(p).ok())
                .and_then(|s| s.chars().next());
            if matches!(number, Some(133 | 633)) && code == Some('C') {
                self.dispatched.push(self.at);
            }
        }
    }

    fn engine_cs(stream: &[u8]) -> Vec<(usize, usize)> {
        let mut parser = aterm_parser::Parser::new();
        let mut sink = EngineCs::default();
        for (i, b) in stream.iter().enumerate() {
            sink.at = i;
            parser.advance_fast(std::slice::from_ref(b), &mut sink);
        }
        sink.dispatched
            .into_iter()
            .map(|d| {
                let st = stream[d] == 0x1b && stream.get(d + 1) == Some(&b'\\');
                (d, d + if st { 2 } else { 1 })
            })
            .collect()
    }

    fn scanned(stream: &[u8], batches: &[usize]) -> (Vec<usize>, Vec<usize>) {
        let mut scan = CScan::default();
        let (mut cuts, mut bounds, mut ends) = (Vec::new(), Vec::new(), Vec::new());
        let mut at = 0;
        for &n in batches {
            let batch = &stream[at..(at + n).min(stream.len())];
            scan.ends(batch, &mut ends);
            assert!(
                ends.windows(2).all(|w| w[0] < w[1])
                    && ends.iter().all(|&e| (1..=batch.len()).contains(&e)),
                "cuts ascend within their batch: {ends:?} in {}",
                batch.len()
            );
            cuts.extend(ends.iter().map(|e| at + e));
            at += batch.len();
            bounds.push(at);
            if at == stream.len() {
                break;
            }
        }
        (cuts, bounds)
    }

    fn law(stream: &[u8], batches: &[usize]) -> Vec<String> {
        let cs = engine_cs(stream);
        let (cuts, bounds) = scanned(stream, batches);
        let mut out = Vec::new();
        for &(d, t) in &cs {
            if !cuts.iter().chain(&bounds).any(|&p| d < p && p <= t) {
                out.push(format!("the C dispatched at {d} ends no slice by {t}"));
            }
        }
        for &p in &cuts {
            if !cs.iter().any(|&(d, t)| d < p && p <= t) {
                out.push(format!("a cut at {p} after no C"));
            }
        }
        out
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            usize::try_from(self.next() % n as u64).unwrap_or(0)
        }

        fn pick<'a>(&mut self, of: &[&'a [u8]]) -> &'a [u8] {
            of[self.below(of.len())]
        }
    }

    fn stream(rng: &mut Rng) -> Vec<u8> {
        const TEXT: [&[u8]; 8] = [
            b"ls -la\r\n",
            b"a[1] ]] ",
            b"\x1b[31mred\x1b[0m",
            b"\xe6\x9c\xac\xc5\x9c",
            b"\x1b",
            b"\x1b\x1b",
            b"\x1b[",
            b"\\",
        ];
        const HEADS: [&[u8]; 18] = [
            b"133;C",
            b"633;C",
            b"133;C",
            b"133;CX",
            b"0133;C",
            b"+133;C",
            b"1\x013\x1f3;\x02C",
            b"133;A",
            b"133;D;0",
            b"133;",
            b"133",
            b"1337;C",
            b"+;C",
            b"0;title ]",
            b"7;file://h/p",
            b"133;C\xff",
            b"133;\xe2\x82\xac",
            b"4294967429;C",
        ];
        const BODIES: [&[u8]; 6] = [
            b"",
            b";id=ab12",
            b";id=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            b";cmd=]]",
            b";\xff\x01x",
            b";\x9c",
        ];
        const ENDS: [&[u8]; 8] = [
            b"\x07", b"\x1b\\", b"\x1b[0m", b"\x1b", b"\x18", b"\x1a", b"", b"\x07",
        ];
        let mut out = Vec::new();
        for _ in 0..rng.below(12) + 1 {
            if rng.below(3) == 0 {
                out.extend_from_slice(rng.pick(&TEXT));
            } else {
                out.extend_from_slice(b"\x1b]");
                out.extend_from_slice(rng.pick(&HEADS));
                out.extend_from_slice(rng.pick(&BODIES));
                out.extend_from_slice(rng.pick(&ENDS));
            }
        }
        out
    }

    #[test]
    fn the_scan_cuts_after_every_engine_c_however_the_batches_split() {
        let mut failures = Vec::new();
        for seed in 1..=4_000u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let stream = stream(&mut rng);
            let whole = [stream.len()];
            let mut split = Vec::new();
            let mut left = stream.len();
            while left > 0 {
                let n = (rng.below(12) + 1).min(left);
                split.push(n);
                left -= n;
            }
            let bytes = vec![1; stream.len()];
            for batches in [&whole[..], &split, &bytes] {
                let found = law(&stream, batches);
                if !found.is_empty() && failures.len() < 8 {
                    failures.push(format!("seed {seed} {batches:?} {stream:?}: {found:?}"));
                }
            }
        }
        assert_eq!(failures, Vec::<String>::new());
    }

    #[test]
    fn the_scan_law_rows() {
        let broken = b"\x1b]133;C;id=\x1b]133;C\x1b\\z";
        assert_eq!(law(broken, &[broken.len()]), Vec::<String>::new());
        assert_eq!(
            cuts(&mut CScan::default(), broken),
            vec![12, broken.len() - 1],
            "the first C ends at the ESC that opens the second"
        );
        let mut scan = CScan::default();
        assert_eq!(cuts(&mut scan, b"\x1b]133;C;id=\x1b"), vec![12]);
        assert_eq!(
            cuts(&mut scan, b"]133;C\x07x"),
            vec![7],
            "the carried ESC's OSC"
        );
        let long = [&b"\x1b]133;C;"[..], &[b'x'; C_MAX], b"\x07z"].concat();
        assert!(
            !law(&long, &[long.len()]).is_empty(),
            "a C past the bound is not cut, and the law sees it"
        );
    }
}
