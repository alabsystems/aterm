// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The foreground process image a session's PTY holds (its holder image), the launch card's
//! gate on it, and the byte-driven arm that reads it at each OSC 133/633 `C`.

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
    /// The card drawer `program` names (`aterm_phase::program_of`: `claude`, `claude-code`,
    /// `agy`, `muse`, `muse-bin-<version>`), or `None`.
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

    /// What this drawer's banner says about its build, as aterm-phase declares it beside the
    /// reader.
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

/// The card's gate for the current holder image (card engine §3.8): the drawer's token, the card
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
    /// An image of unknown build (brew, a dev link, an ALab build without its record) of a
    /// program whose banner prints its build.
    Registry(atpkg::vendor_direct::Version),
    /// A program whose banner is not known to print its build: the fence alone decides.
    ProvenanceOnly,
    /// No build to hold the banner to.
    None,
}

/// The process group each holder-fence mark's drawer image leads (card engine §3.7). When the
/// process watch hears a stopped drawer's leader exit (`exec_edges::ExecEdges::take_gone`), the
/// parse stage retires its mark (`Terminal::retire_holder_mark`), so dead stopped drawers never
/// push a live one's mark out past [`aterm_core::terminal::MAX_HOLDER_MARKS`]. A cut to a drawer
/// image (`spawn::note_cut`) and a restore's adoption (`spawn::adopt_restored_holder`) note the
/// group of each mark they lay; the session timeline keeps the book for a reattached reader.
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
    #[cfg(any(unix, test))]
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

/// One reading of a foreground group leader's image (card engine §3.4).
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

    /// The build policy (card engine §3.8) this drawer's banner is held to; `None` for an
    /// image that draws no card.
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

pub(crate) fn kept_exe(exe: &Path) -> &Path {
    #[cfg(target_os = "linux")]
    let exe = atpkg::image::strip_deleted(exe);
    exe
}

/// Whether `exe` is a script shell's image ([`atpkg::image::SCRIPT_SHELLS`]), whose script is
/// its `argv[1]`.
pub(crate) fn is_script_shell(exe: &Path) -> bool {
    exe.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| atpkg::image::SCRIPT_SHELLS.contains(&n))
}

/// A token as a timeline row prints it: 16 hex digits, or `-`.
pub(crate) fn token_word(token: Option<u64>) -> String {
    token.map_or_else(|| "-".to_string(), |t| format!("{t:016x}"))
}

/// The snapshot token (card engine I5): FNV-1a/64 of the pid, process start, executable and
/// classification.
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

/// Read `pgid`'s holder image (the shipping `SessionFactory::edge_image`), on the gather thread
/// at an edge or on the parse stage for the byte arm's re-read. A `last` image of the same
/// process is kept, re-stamped.
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
pub(crate) fn start_us_of(pgid: i32) -> Option<u64> {
    let birth = aterm_uds::process::read_process_birth(pgid)?;
    birth
        .seconds
        .checked_mul(1_000_000)?
        .checked_add(birth.microseconds)
}

/// `pgid`'s leader's start in microseconds, or `None` (gone, another user's).
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn start_us_of(pgid: i32) -> Option<u64> {
    aterm_uds::process::read_process_start(pgid).map(|s| s.boot.0 / 1_000)
}

/// No start record wired here: every image is untracked.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
pub(crate) fn start_us_of(_pgid: i32) -> Option<u64> {
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

/// The managed prefix's [`ImageRoots`], resolved once and again only when `aterm.toml` or the
/// prefix directory changes.
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

/// The publication rule: what offering `offered` over the session's `current` holder image does.
pub(crate) fn decide(current: Option<&HolderImage>, offered: &HolderImage) -> Offer {
    if !offered.card_relevant() && !current.is_some_and(HolderImage::card_relevant) {
        return Offer::Irrelevant;
    }
    if current.is_some_and(|c| c.same_image(offered)) {
        return Offer::Same;
    }
    Offer::Published
}

/// The edged inbox (card engine §3.5): the sessions with a published edge the sweep has not
/// seen.
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

/// A block's start, an OSC 133/633 `C`, as a [`MarkScan`] mark.
pub(crate) const MARK_C: u8 = 1;

/// A block's end, an OSC 133/633 `D`, as a [`MarkScan`] mark.
pub(crate) const MARK_D: u8 = 2;

/// The C-cut scan (card engine §3.6): where each OSC 133/633 `C` in the PTY bytes ends.
pub(crate) type CScan = MarkScan<MARK_C>;

/// Where each OSC 133/633 mark among `MARKS` ([`MARK_C`], [`MARK_D`], or both) in the PTY bytes
/// ends, an unfinished one carried into the next batch.
#[derive(Debug, Default)]
pub(crate) struct MarkScan<const MARKS: u8> {
    carry: Vec<u8>,
}

impl<const MARKS: u8> MarkScan<MARKS> {
    /// Clear `ends` and fill it with the cut offsets in `batch`, ascending, each in
    /// `1..=batch.len()`.
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
            match osc_mark_end(batch, esc, MARKS) {
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
        let next = match osc_mark_end(&joined, 0, MARKS) {
            OscEnd::C { end, resume } => {
                if end > carried {
                    ends.push(end - carried);
                }
                resume
            }
            OscEnd::Open => {
                debug_assert_eq!(take, batch.len(), "an open mark fits its bound");
                self.carry = joined;
                return None;
            }
            OscEnd::NotC => 1,
        };
        Some(next.saturating_sub(carried))
    }
}

/// What [`osc_mark_end`] found at an ESC.
#[derive(Debug, PartialEq, Eq)]
enum OscEnd {
    /// A complete OSC 133/633 mark of those scanned for (a `C`, else a `D`
    /// where one is), dispatched by the engine once `end` bytes are parsed:
    /// one past its BEL, its ST's `\`, or the ESC that broke it off.
    C { end: usize, resume: usize },
    /// A prefix of one, cut by the end of the bytes.
    Open,
    /// Anything else.
    NotC,
}

/// What the OSC at `esc` is: a complete mark among `marks`, an open prefix of one, or neither.
fn osc_mark_end(bytes: &[u8], esc: usize, marks: u8) -> OscEnd {
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
                    Part::Code(code) => is_mark_code(code, marks),
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
                Part::Code(code) if is_mark_code(code, marks) => part = Part::Rest,
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
                    if code.is_empty() && !is_mark(b, marks) {
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

/// Whether `letter` opens a mark among `marks`.
fn is_mark(letter: u8, marks: u8) -> bool {
    match letter {
        b'C' => marks & MARK_C != 0,
        b'D' => marks & MARK_D != 0,
        _ => false,
    }
}

/// Whether `code`, an OSC 133/633 second parameter, is a mark among `marks`: the engine reads
/// the mark by its first letter.
fn is_mark_code(code: &[u8], marks: u8) -> bool {
    code.first().is_some_and(|&b| is_mark(b, marks)) && std::str::from_utf8(code).is_ok()
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

/// The byte-driven arm's state in one PTY reader's parse stage (card engine §3.6).
#[derive(Debug)]
pub(crate) struct ByteArm {
    /// One initial observation also covers an already-running or direct -e
    /// holder, where the foreground group never changes.
    initial: bool,
    /// The signed command is still an unresolved shell, which can exec after
    /// any number of output batches.
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

    /// A gather read or re-read satisfies this batch's observation even if publication was
    /// irrelevant.
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

    /// The engine's accepted command mark and resulting shell state, read under its existing
    /// hold.
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

/// I1's test seams: what a watched session's timeline held inside each slice's term-lock
/// hold, and the controls' mutants it runs.
#[cfg(test)]
#[path = "holder_image_hold_hook_tests.rs"]
pub(crate) mod hold_hook;

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

    /// Median MB/s of `runs` passes without the arm and with it (alternating, after one untimed
    /// pass of each), and the cuts and image reads one pass with it made.
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
#[path = "holder_image_tests.rs"]
mod tests;
