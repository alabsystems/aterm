// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The std-only half of the font oracle: the corpus line format, the FNV-1a
//! digest every answer is written with, and the table of faces the corpus
//! records. `probe.rs` (the rustybuzz half) and `tests/font_oracle/recorder`
//! include it by `#[path]`, and so do `aterm-font`'s replay (TF.3 of
//! docs/AUDIT-third-party-2026-10-15.md) and aterm-render's CoreText emoji
//! replay (TF.6); `reader.rs`, the reader half, and `shaper.rs`, aterm-font's
//! shaping half, read it as `super::corpus`.
//!
//! So this file names only `std`.

use std::fmt::Write as _;

/// FNV-1a 64 over a byte stream: dependency-free and the same on every host.
pub(crate) struct Fnv(u64);

impl Fnv {
    pub(crate) fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub(crate) fn u16(&mut self, v: u16) {
        self.bytes(&v.to_le_bytes());
    }

    pub(crate) fn i16(&mut self, v: i16) {
        self.bytes(&v.to_le_bytes());
    }

    pub(crate) fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }

    pub(crate) fn i32(&mut self, v: i32) {
        self.bytes(&v.to_le_bytes());
    }

    /// The exact bits, so `-0.0` and `0.0` differ and nothing rounds.
    pub(crate) fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }

    pub(crate) fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// The key a face's answers are filed under: the FNV-1a 64 of the whole font
/// file. A system font that is absent, or that an OS update replaced, has no
/// lines under its current key, so its old answers are skipped, never failed.
pub(crate) fn file_key(bytes: &[u8]) -> String {
    let mut h = Fnv::new();
    h.bytes(bytes);
    h.hex()
}

/// Where a face file lives. `Repo` paths are relative to aterm-render's
/// manifest directory and are always present; `System` paths are the host's
/// own fonts, which are never committed (their licences forbid it).
#[derive(Clone, Copy)]
pub(crate) enum Origin {
    Repo(&'static str),
    System(&'static str),
}

/// What the corpus records for a face. A face can carry several roles.
pub(crate) const READ: u8 = 1;
/// A face aterm can draw as the primary: the run shaper shapes it whenever
/// `should_run_shaping` lets it (the full case set at the instance aterm
/// draws it at; the reduced set at its other reachable instances).
pub(crate) const PRIMARY: u8 = 1 << 1;
/// A styled sibling (bold, italic, bold italic) of a primary, set by
/// [`FaceFile::of`]: the reduced case set, under the feature lists its
/// primary's gate reaches (the renderer gates on the primary's `GSUB`).
pub(crate) const STYLED: u8 = 1 << 2;
/// The colour face `shape_cluster` shapes emoji clusters on.
pub(crate) const EMOJI: u8 = 1 << 3;
/// A face the Linux/Windows grid-fitting path draws (the hinting corpus), or
/// one that stands in for an outline format it draws: the committed CFF
/// fixture on every host, SF India's CFF2 variable face on a Mac (TF.4), and
/// on Linux Noto Sans CJK, the autohinter's CJK style over CFF outlines.
pub(crate) const HINT: u8 = 1 << 4;

pub(crate) struct FaceFile {
    pub(crate) label: &'static str,
    pub(crate) origin: Origin,
    /// Collection indices recorded (`&[0]` for a single-face file).
    pub(crate) indices: &'static [u32],
    pub(crate) roles: u8,
    /// A styled sibling's primary, by label; listed before it in [`FACES`].
    pub(crate) primary: Option<&'static str>,
}

impl FaceFile {
    /// This face as a styled sibling of the face labelled `primary`.
    const fn of(self, primary: &'static str) -> Self {
        Self {
            roles: self.roles | STYLED,
            primary: Some(primary),
            ..self
        }
    }
}

const fn repo(label: &'static str, path: &'static str, roles: u8) -> FaceFile {
    FaceFile {
        label,
        origin: Origin::Repo(path),
        indices: &[0],
        roles,
        primary: None,
    }
}

const fn sys(
    label: &'static str,
    path: &'static str,
    indices: &'static [u32],
    roles: u8,
) -> FaceFile {
    FaceFile {
        label,
        origin: Origin::System(path),
        indices,
        roles,
        primary: None,
    }
}

/// Every face the oracle records: the bundled faces, the committed fixtures,
/// and the system faces aterm loads by path (`FONT_CANDIDATES`,
/// `FALLBACK_CANDIDATES`, `SYMBOL_FALLBACK_CANDIDATES`,
/// `COLOR_EMOJI_CANDIDATES` in `src/lib.rs`) plus the script faces the macOS
/// coverage scan reaches for Arabic, Hebrew and Devanagari.
pub(crate) const FACES: &[FaceFile] = &[
    // Bundled: the Linux/Windows default stack and the selectable families.
    repo(
        "JetBrainsMono-Regular",
        "assets/bundled/JetBrainsMono-Regular.ttf",
        READ | PRIMARY | HINT,
    ),
    repo(
        "JetBrainsMono-Bold",
        "assets/bundled/JetBrainsMono-Bold.ttf",
        READ | HINT,
    )
    .of("JetBrainsMono-Regular"),
    repo(
        "JetBrainsMono-Italic",
        "assets/bundled/JetBrainsMono-Italic.ttf",
        READ | HINT,
    )
    .of("JetBrainsMono-Regular"),
    repo(
        "JetBrainsMono-BoldItalic",
        "assets/bundled/JetBrainsMono-BoldItalic.ttf",
        READ | HINT,
    )
    .of("JetBrainsMono-Regular"),
    repo(
        "CascadiaMonoPL-Regular",
        "assets/bundled/CascadiaMonoPL-Regular.ttf",
        READ | PRIMARY | HINT,
    ),
    repo(
        "CascadiaMonoPL-Bold",
        "assets/bundled/CascadiaMonoPL-Bold.ttf",
        READ | HINT,
    )
    .of("CascadiaMonoPL-Regular"),
    repo(
        "CascadiaMonoPL-Italic",
        "assets/bundled/CascadiaMonoPL-Italic.ttf",
        READ | HINT,
    )
    .of("CascadiaMonoPL-Regular"),
    repo(
        "CascadiaMonoPL-BoldItalic",
        "assets/bundled/CascadiaMonoPL-BoldItalic.ttf",
        READ | HINT,
    )
    .of("CascadiaMonoPL-Regular"),
    repo(
        "IosevkaTerm-Regular",
        "assets/bundled/IosevkaTerm-Regular.ttf",
        READ | PRIMARY | HINT,
    ),
    repo(
        "IosevkaTerm-Bold",
        "assets/bundled/IosevkaTerm-Bold.ttf",
        READ | HINT,
    )
    .of("IosevkaTerm-Regular"),
    repo(
        "IosevkaTerm-Italic",
        "assets/bundled/IosevkaTerm-Italic.ttf",
        READ | HINT,
    )
    .of("IosevkaTerm-Regular"),
    repo(
        "IosevkaTerm-BoldItalic",
        "assets/bundled/IosevkaTerm-BoldItalic.ttf",
        READ | HINT,
    )
    .of("IosevkaTerm-Regular"),
    // The Windows default since 2026-10-16, Iosevka Term Semibold: subsets of
    // the SemiBold and ExtraBold cuts and their italics, without `calt`, so
    // the run shaper reaches them only under a configured `frac`, `numr` or
    // `dnom`. Their hinting answers were recorded on Windows 2026-10-16,
    // where the recorder answered every committed face's lines as recorded
    // on macOS.
    repo(
        "IosevkaTerm-SemiBold",
        "assets/bundled/IosevkaTerm-SemiBold.ttf",
        READ | PRIMARY | HINT,
    ),
    repo(
        "IosevkaTerm-ExtraBold",
        "assets/bundled/IosevkaTerm-ExtraBold.ttf",
        READ | HINT,
    )
    .of("IosevkaTerm-SemiBold"),
    repo(
        "IosevkaTerm-SemiBoldItalic",
        "assets/bundled/IosevkaTerm-SemiBoldItalic.ttf",
        READ | HINT,
    )
    .of("IosevkaTerm-SemiBold"),
    repo(
        "IosevkaTerm-ExtraBoldItalic",
        "assets/bundled/IosevkaTerm-ExtraBoldItalic.ttf",
        READ | HINT,
    )
    .of("IosevkaTerm-SemiBold"),
    repo(
        "DejaVuSansMono",
        "assets/DejaVuSansMono.ttf",
        READ | PRIMARY | HINT,
    ),
    repo(
        "NotoSansSymbols2-Regular",
        "assets/bundled/NotoSansSymbols2-Regular.ttf",
        READ | HINT,
    ),
    repo(
        "NotoSansMath-Regular",
        "assets/bundled/NotoSansMath-Regular.ttf",
        READ | HINT,
    ),
    repo(
        "SymbolsNerdFontMono-Regular",
        "assets/SymbolsNerdFontMono-Regular.ttf",
        READ | HINT,
    ),
    // The display faces (`assets/game`): `font_family = "display:<id>"`
    // makes one the primary (`display_face_mix_for_family`). Cinzel is the
    // committed variable face; Chewy has no GSUB, so the gate never shapes it.
    repo(
        "Cinzel-var",
        "assets/game/Cinzel-var.ttf",
        READ | PRIMARY | HINT,
    ),
    repo(
        "Chewy-Regular",
        "assets/game/Chewy-Regular.ttf",
        READ | PRIMARY,
    ),
    repo(
        "LuckiestGuy-Regular",
        "assets/game/LuckiestGuy-Regular.ttf",
        READ | PRIMARY,
    ),
    repo("Monocraft", "assets/game/Monocraft.ttf", READ | PRIMARY),
    // Test fixtures: the ligature suites' JetBrains Mono, and the COLR v1 test
    // fonts (no system face on macOS carries COLR).
    repo(
        "fixture-jetbrains-mono",
        "tests/fixtures/jetbrains-mono.ttf",
        READ | PRIMARY,
    ),
    repo("fixture-colr_1", "tests/fixtures/colr_1.ttf", READ),
    repo(
        "fixture-colr_1_variable",
        "tests/fixtures/colr_1_variable.ttf",
        READ,
    ),
    // The one committed CFF face (OFL), so every host replays CFF outlines
    // and their grid fit.
    repo(
        "fixture-NotoSansSunuwar",
        "tests/fixtures/NotoSansSunuwar-Regular.otf",
        READ | HINT,
    ),
    // macOS: the primary candidates and their styled siblings.
    sys(
        "SFNSMono",
        "/System/Library/Fonts/SFNSMono.ttf",
        &[0],
        READ | PRIMARY,
    ),
    sys(
        "SFNSMonoItalic",
        "/System/Library/Fonts/SFNSMonoItalic.ttf",
        &[0],
        READ,
    )
    .of("SFNSMono"),
    sys(
        "Menlo",
        "/System/Library/Fonts/Menlo.ttc",
        &[0],
        READ | PRIMARY,
    ),
    sys(
        "Menlo-styled",
        "/System/Library/Fonts/Menlo.ttc",
        &[1, 2, 3],
        READ,
    )
    .of("Menlo"),
    sys(
        "Monaco",
        "/System/Library/Fonts/Monaco.ttf",
        &[0],
        READ | PRIMARY,
    ),
    sys(
        "AndaleMono",
        "/System/Library/Fonts/Supplemental/Andale Mono.ttf",
        &[0],
        READ | PRIMARY,
    ),
    sys(
        "CourierNew",
        "/System/Library/Fonts/Supplemental/Courier New.ttf",
        &[0],
        READ | PRIMARY,
    ),
    // `styled_sibling_paths` finds Courier New's styled faces as separate
    // files (`<stem> Bold`, `<stem> Italic`, `<stem> Bold Italic`).
    sys(
        "CourierNew-Bold",
        "/System/Library/Fonts/Supplemental/Courier New Bold.ttf",
        &[0],
        READ,
    )
    .of("CourierNew"),
    sys(
        "CourierNew-Italic",
        "/System/Library/Fonts/Supplemental/Courier New Italic.ttf",
        &[0],
        READ,
    )
    .of("CourierNew"),
    sys(
        "CourierNew-BoldItalic",
        "/System/Library/Fonts/Supplemental/Courier New Bold Italic.ttf",
        &[0],
        READ,
    )
    .of("CourierNew"),
    // macOS: the colour face and the fallback, symbol and script faces.
    sys(
        "AppleColorEmoji",
        "/System/Library/Fonts/Apple Color Emoji.ttc",
        &[0],
        READ | EMOJI,
    ),
    sys(
        "HiraginoSansGB",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        &[0],
        READ,
    ),
    sys(
        "ArialUnicode",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        &[0],
        READ,
    ),
    sys(
        "AppleSymbols",
        "/System/Library/Fonts/Apple Symbols.ttf",
        &[0],
        READ,
    ),
    sys(
        "STIXTwoMath",
        "/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
        &[0],
        READ,
    ),
    sys("SFHebrew", "/System/Library/Fonts/SFHebrew.ttf", &[0], READ),
    sys("SFArabic", "/System/Library/Fonts/SFArabic.ttf", &[0], READ),
    sys("GeezaPro", "/System/Library/Fonts/GeezaPro.ttc", &[0], READ),
    sys("Kohinoor", "/System/Library/Fonts/Kohinoor.ttc", &[0], READ),
    // The SF India collection: one variable CFF2 face per Indic script, the
    // only CFF2 outlines a Mac ships (TF.3: `aterm-font`'s CFF2 interpreter
    // is held to them, at every instance `reader_instances` picks).
    sys(
        "SFIndia",
        "/System/Library/Fonts/SFIndia.ttc",
        &[0, 1, 2, 3, 4, 5, 6, 7, 8],
        READ | HINT,
    ),
    // Linux and Windows: the colour faces, the CJK face aterm's Linux fallback
    // chain loads (CFF), and a CFF2 candidate, recorded when the corpus is
    // blessed on a host that has them. An absent path records nothing.
    //
    // Recorded in the Linux lane (Ubuntu 24.04, L-FONTS of
    // docs/AUDIT-third-party-2026-10-15.md, 2026-10-19): Noto Color Emoji
    // 2.047 (`fonts-noto-color-emoji`) is the CBDT build, with `GSUB` and no
    // `GPOS` or `COLR`, and its `shape` lines came from the recorder's
    // `shape` mode on rustybuzz. Every emoji case shapes to one glyph with no
    // offset (VS15 adds a zero-advance glyph), so there is no mark for
    // `GPOS` MarkBase, or rustybuzz's fallback placement, to position, and
    // aterm-font's shaper carves neither. `fonts-noto-cjk` 1:20230817
    // installs static CFF collections only (Noto Sans CJK Regular is ten
    // faces, five regions' Sans and Sans Mono); Ubuntu ships no variable CJK
    // build, so the CFF2 path below is absent there (Noto ships it as
    // `*-VF.otf.ttc`; the `*-VF.ttf.ttc` build is TrueType).
    // Segoe UI Emoji waits on a bless on Windows.
    sys(
        "NotoColorEmoji",
        "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
        &[0],
        READ | EMOJI,
    ),
    sys(
        "NotoSansCJK-Regular",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        &[0],
        READ | HINT,
    ),
    sys(
        "NotoSansCJK-VF-CFF2",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-VF.otf.ttc",
        &[0],
        READ,
    ),
    sys(
        "SegoeUIEmoji",
        "C:\\Windows\\Fonts\\seguiemj.ttf",
        &[0],
        READ | EMOJI,
    ),
    sys(
        "CascadiaMono-win",
        "C:\\Windows\\Fonts\\CascadiaMono.ttf",
        &[0],
        READ | PRIMARY,
    ),
];

/// The clusters `Renderer::shape_cluster` shapes on the colour face: the
/// `emoji.*` case of every [`EMOJI`] face's `shape` lines, with no features
/// and no coordinates. `tests/font_oracle.rs` answers them with aterm-font's
/// shaper, and the recorder's `shape` mode records them with rustybuzz on a
/// host whose colour face no bless has recorded yet.
pub(crate) const EMOJI_CASES: &[(&str, &str)] = &[
    ("emoji.grin", "\u{1F600}"),
    ("emoji.heart-vs16", "\u{2764}\u{FE0F}"),
    ("emoji.smile-vs15", "\u{263A}\u{FE0E}"),
    (
        "emoji.family",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
    ),
    ("emoji.technologist", "\u{1F469}\u{200D}\u{1F4BB}"),
    ("emoji.rainbow-flag", "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}"),
    ("emoji.heart-fire", "\u{2764}\u{FE0F}\u{200D}\u{1F525}"),
    (
        "emoji.holding-hands",
        "\u{1F9D1}\u{200D}\u{1F91D}\u{200D}\u{1F9D1}",
    ),
    ("emoji.thumbs-medium", "\u{1F44D}\u{1F3FD}"),
    ("emoji.wave-dark", "\u{1F44B}\u{1F3FF}"),
    (
        "emoji.red-hair-light",
        "\u{1F9D1}\u{1F3FB}\u{200D}\u{1F9B0}",
    ),
    (
        "emoji.couple-tones",
        "\u{1F469}\u{1F3FE}\u{200D}\u{1F91D}\u{200D}\u{1F468}\u{1F3FB}",
    ),
    ("emoji.flag-us", "\u{1F1FA}\u{1F1F8}"),
    ("emoji.flag-jp", "\u{1F1EF}\u{1F1F5}"),
    ("emoji.flag-invalid", "\u{1F1E6}\u{1F1E6}"),
    (
        "emoji.flag-england",
        "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
    ),
    ("emoji.keycap-1", "1\u{FE0F}\u{20E3}"),
    ("emoji.keycap-hash", "#\u{FE0F}\u{20E3}"),
    ("emoji.keycap-bare", "*\u{20E3}"),
    ("emoji.melting", "\u{1FAE0}"),
    ("emoji.ascii", "A"),
];

/// The faces whose `read` answers live in `reader-gvar-alloc.txt`, not
/// `reader.txt`: each has glyphs with more than 32 `gvar` tuples, which
/// ttf-parser 0.25.1 draws only under its `gvar-alloc` feature. aterm's Linux
/// build turned that on (winit's `wayland-csd-adwaita` -> sctk-adwaita ->
/// ab_glyph, until LW.5 retired the three), its macOS, Windows and wasm builds
/// did not, and without it such a
/// glyph has no outline at any instance. `tests/font_oracle.rs` reads
/// ttf-parser as `-p aterm-render` resolves it (no `gvar-alloc`), so these
/// faces are recorded by the recorder, which builds it with the feature; a
/// replay of `reader.txt` there shows no `FACES` face answers differently
/// with it. Measured 2026-10-16 over every face in `/System/Library/Fonts`,
/// `/Library/Fonts` and this crate's assets: these four are the only ones.
// Read by the recorder and aterm-font's replay; `tests/font_oracle.rs` never
// records these faces.
#[allow(dead_code)]
pub(crate) const GVAR_ALLOC_FACES: &[FaceFile] = &[
    // SF Pro: 2,590 of its 2,954 glyphs.
    sys("SFNS", "/System/Library/Fonts/SFNS.ttf", &[0], READ),
    // 268, 43 and 64 glyphs; one composite in #2 has a component that does.
    sys(
        "ADTNumeric",
        "/System/Library/Fonts/ADTNumeric.ttc",
        &[2, 3, 7],
        READ,
    ),
];

/// A variation instance: user-space `(axis tag, value)` pairs, applied in order.
pub(crate) type Coords = Vec<([u8; 4], f32)>;
/// Shaping features: `(tag, value)` pairs, each over the whole run.
pub(crate) type Features = Vec<([u8; 4], u32)>;

pub(crate) fn tag_str(tag: [u8; 4]) -> String {
    tag.iter().map(|&b| char::from(b)).collect()
}

pub(crate) fn parse_tag(s: &str) -> Option<[u8; 4]> {
    let b = s.as_bytes();
    (b.len() == 4).then(|| [b[0], b[1], b[2], b[3]])
}

/// `-` for the default instance (no coordinates set at all), else
/// `wght=700,wdth=100` with each value in Rust's shortest round-trip spelling.
pub(crate) fn fmt_coords(coords: &[([u8; 4], f32)]) -> String {
    if coords.is_empty() {
        return "-".to_string();
    }
    let parts: Vec<String> = coords
        .iter()
        .map(|&(t, v)| format!("{}={v:?}", tag_str(t)))
        .collect();
    parts.join(",")
}

pub(crate) fn parse_coords(s: &str) -> Option<Coords> {
    if s == "-" {
        return Some(Vec::new());
    }
    s.split(',')
        .map(|part| {
            let (t, v) = part.split_once('=')?;
            Some((parse_tag(t)?, v.parse().ok()?))
        })
        .collect()
}

/// `-` for no features, else `liga=1,calt=1`.
pub(crate) fn fmt_features(features: &[([u8; 4], u32)]) -> String {
    if features.is_empty() {
        return "-".to_string();
    }
    let parts: Vec<String> = features
        .iter()
        .map(|&(t, v)| format!("{}={v}", tag_str(t)))
        .collect();
    parts.join(",")
}

pub(crate) fn parse_features(s: &str) -> Option<Features> {
    if s == "-" {
        return Some(Vec::new());
    }
    s.split(',')
        .map(|part| {
            let (t, v) = part.split_once('=')?;
            Some((parse_tag(t)?, v.parse().ok()?))
        })
        .collect()
}

/// Printable ASCII other than `\` stays itself; everything else (space,
/// controls, every non-ASCII scalar) is `\u{hex}`, so a line never carries a
/// tab, a newline or a bidi-reordered run an editor would display out of order.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_graphic() && ch != '\\' {
            out.push(ch);
        } else {
            let _ = write!(out, "\\u{{{:x}}}", u32::from(ch));
        }
    }
    out
}

pub(crate) fn unescape(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let tail = rest[at..].strip_prefix("\\u{")?;
        let end = tail.find('}')?;
        out.push(char::from_u32(u32::from_str_radix(&tail[..end], 16).ok()?)?);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// An answer longer than this is pinned by digest and length.
pub(crate) const VERBATIM_MAX: usize = 480;

pub(crate) fn pin(answer: String) -> String {
    if answer.len() <= VERBATIM_MAX {
        return answer;
    }
    let mut h = Fnv::new();
    h.bytes(answer.as_bytes());
    format!("long fnv={} len={}", h.hex(), answer.len())
}

/// One corpus line: eight TAB-separated fields, the last of which is the
/// answer. Everything before it is the case's identity and its inputs.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Line {
    /// `shape` (rustybuzz), `read` (ttf-parser), `font` (aterm's `font::Font`)
    /// or `vary` (aterm's `variation` module).
    pub(crate) kind: String,
    pub(crate) label: String,
    /// The face file's [`file_key`].
    pub(crate) key: String,
    pub(crate) index: u32,
    /// [`fmt_coords`].
    pub(crate) inst: String,
    /// [`fmt_features`] for `shape`; `-` otherwise.
    pub(crate) feats: String,
    /// The case id (`lig.arrow`, `ar.marhaba.visual.0`) or the query (`hmtx`).
    pub(crate) case: String,
    /// The shaped text, [`escape`]d; `-` for a query.
    pub(crate) input: String,
    pub(crate) answer: String,
}

impl Line {
    /// The case's identity: every field but the answer.
    pub(crate) fn id(&self) -> String {
        format!(
            "{}\t{}\t{}#{}\t{}\t{}\t{}\t{}",
            self.kind,
            self.label,
            self.key,
            self.index,
            self.inst,
            self.feats,
            self.case,
            self.input
        )
    }

    pub(crate) fn render(&self) -> String {
        format!("{}\t{}", self.id(), self.answer)
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        let f: Vec<&str> = text.split('\t').collect();
        let [kind, label, keyed, inst, feats, case, input, answer] = f.as_slice() else {
            return None;
        };
        let (key, index) = keyed.split_once('#')?;
        Some(Self {
            kind: (*kind).to_string(),
            label: (*label).to_string(),
            key: key.to_string(),
            index: index.parse().ok()?,
            inst: (*inst).to_string(),
            feats: (*feats).to_string(),
            case: (*case).to_string(),
            input: (*input).to_string(),
            answer: (*answer).to_string(),
        })
    }
}

/// The code points every `face.glyph_index` probe adds to the face's own cmap
/// coverage: ASCII and Latin-1, the normalization and default-ignorable points
/// the shaping corpus feeds, a Powerline glyph, an emoji and the extremes.
pub(crate) fn probe_chars() -> Vec<char> {
    let mut out: Vec<char> = (0u32..=0xFF).filter_map(char::from_u32).collect();
    out.extend(
        [
            0x200B, 0x200D, 0x2011, 0x2044, 0x2060, 0x2126, 0x212B, 0x25CC, 0xE0A0, 0xFE0F, 0xFFFD,
            0x1F600, 0x10FFFF,
        ]
        .into_iter()
        .filter_map(char::from_u32),
    );
    out
}

/// Glyphs per `outline.<start>` / `colr.<start>` block: a moved answer names
/// the 1,024-glyph range it moved in.
pub(crate) const BLOCK: u32 = 1024;
