// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The BUNDLED TERMINAL FONT STACK (non-macOS), and Iosevka Term (everywhere).
//!
//! Owner decision (2026-09-24): the fonts a stock Linux ships are poor and
//! incomplete — `fc-match monospace` answers DejaVu Sans Mono, and nothing on a
//! Debian-family default install covers `⏵` U+23F5 (Claude Code's `⏵⏵ bypass
//! permissions on`), `⏺`, `⎿` or most of Misc Technical — so aterm SHIPS its own
//! stack there, with **JetBrains Mono** as the primary. The chain a default
//! Linux window resolves through is:
//!
//! ```text
//! JetBrains Mono (real Bold / Italic / BoldItalic faces)      primary
//!  → DejaVu Sans Mono (embedded)                               broad tier, first
//!  → system per-script / CJK faces                             broad tier
//!  → Noto Sans Symbols 2 → Noto Sans Math (embedded)           symbol tier, first
//!  → system symbol faces                                       symbol tier
//!  → colour emoji → Symbols Nerd Font (embedded, runtime tier)
//!  → procedural Synthetic → .notdef
//! ```
//!
//! # Windows: Cascadia Mono PL
//!
//! On Windows the primary is **Cascadia Mono PL** 2407.24, the official STATIC
//! release (SIL OFL 1.1; Regular / Bold / Italic / BoldItalic), compiled in on
//! Windows only (`cfg(windows)` on its `include_bytes!`) so a Linux binary
//! does not carry its ~2.4 MB. Chosen 2026-09-30 when the owner asked for "a
//! nicer, better looking font for Windows aterm": a render farm drew 22 faces
//! through aterm on a 96 DPI display and three of four judges picked Cascadia
//! Mono — measured crisper under aterm's grayscale raster, 50.7% of ink pixels
//! at >= 80% coverage against 40.5% for JetBrains Mono — and two of them this
//! exact cut:
//!
//! * a REAL cursive italic. The `C:\Windows\Fonts\CascadiaMono.ttf` Windows 11
//!   ships is one variable file with a `wght` axis and no italic, so its italic
//!   is the synthetic shear;
//! * the PL glyphs (U+E0A0..E0A3, U+E0B0..E0D4). A face without U+E0A0 does
//!   not get tofu on Windows; it gets a WRONG glyph, because `seguisym.ttf`
//!   maps that private code point to Microsoft's own symbol, and every
//!   oh-my-posh / Starship branch segment showed it. That reason has since
//!   gone twice over: Segoe UI Symbol now yields those points to the embedded
//!   Symbols Nerd Font (`FOREIGN_PRIVATE_USE_FACES` in the crate root), and so
//!   do these faces themselves ([`yields_private_use`]) — drawn for a taller
//!   line than the cell, their own PL glyphs painted into the row above;
//! * the same face on every Windows, Windows 10 included, which ships no
//!   Cascadia at all.
//!
//! Two things the default gives up, both on purpose. It draws NO programming
//! ligatures: the Mono cut is Cascadia Code without them (`=> != >= -> ===`
//! stay as typed, measured on renders 2026-09-30), where the JetBrains Mono
//! default before it drew them; `font_family = "JetBrains Mono"` brings them
//! back, and the Cascadia CODE PL cut of the same release is the face to
//! bundle if the default should ever ligate. And its cell is 9x21 at the
//! default 16 px, not JetBrains Mono's 10x21, so a window of the same width
//! holds more columns.
//!
//! JetBrains Mono stays compiled in on Windows, selectable by name, but it is
//! NOT a chain face there: the broad tier is DejaVu Sans Mono alone, as on
//! Linux ([`broad_fallback_ids`] says why):
//!
//! ```text
//! Cascadia Mono PL (real Bold / Italic / BoldItalic faces)    primary
//!                       (its private-use glyphs yield to the Symbols Nerd Font)
//!  → DejaVu Sans Mono (embedded)                               broad tier, first
//!  → system per-script / CJK faces → Arial                     broad tier
//!  → Noto Sans Symbols 2 → Noto Sans Math (embedded)           symbol tier, first
//!  → Segoe UI Symbol                                           symbol tier
//!  → Segoe UI Emoji → Symbols Nerd Font (embedded, runtime tier)
//!  → procedural Synthetic → .notdef
//! ```
//!
//! Its cell is built on the vendor's line with ONE correction: the descent
//! reaches the family's deepest Latin letter, and a pixel past it for the
//! hinter ([`letter_descent_floor`]), so a Romanian `ș` or a Latvian `ķ` keeps
//! its comma and, at the default 16 px, a row of `j` keeps a clear pixel row
//! above the caron of an `Š` on the next row.
//!
//! # Iosevka Term, by name, everywhere
//!
//! **Iosevka Term** 34.9.0 (SIL OFL 1.1; Regular / Bold / Italic /
//! BoldItalic; owner request 2026-10-14, "add https://typeof.net/Iosevka/ to
//! aterm and try it") is compiled in on EVERY platform the stack feature is
//! on, macOS included ([`IOSEVKA_ACTIVE`]), and is nobody's default and no
//! chain face: `font_family = "Iosevka Term"` selects it, and an installed
//! Iosevka Term wins that name as JetBrains Mono's does. The Term cut is the
//! one for terminals: strictly monospaced, its arrows and geometric shapes one
//! cell wide. Its cell is narrow — a 0.5 em advance on a 1.25 em line, 8x20
//! at 16 px where JetBrains Mono is 10x21 — so a window of the same width
//! holds a quarter more columns.
//!
//! What ships is a SUBSET of the official unhinted release (aterm hints with
//! the autohinter, or CoreText on macOS, so the font's own bytecode would be
//! dead weight): 5,015 code points — Latin, Greek, Cyrillic, Armenian,
//! punctuation, arrows, math, Misc Technical, box drawing, blocks, Braille,
//! legacy computing, Powerline — and HarfBuzz's default layout features
//! (ligatures, marks, fractions), not Iosevka's stylistic sets and character
//! variants: about 0.6 MB a face where the whole is 8.4 to 8.9 MB.
//! `publish/DECISIONS.md` holds the source digests, the command and the
//! measured sizes. Iosevka declares no Reserved Font Name, so the subset keeps
//! its name.
//!
//! Every face here is `include_bytes!`'d, so the stack needs no I/O and works
//! identically on a stripped container and a full desktop.
//!
//! # Identity
//!
//! A bundled face has no path. It is named by a VIRTUAL identity under the
//! `bundled:` scheme (the `display:` scheme's twin), which is what
//! [`crate::resolve_config_font`], [`crate::font_catalog::resolve_and_admit`],
//! [`crate::primary_font_candidate_paths`] and a renderer's `primary_path`
//! carry. The unconfigured default IS the identity, so it costs no directory
//! scan and is the same face on every machine. Each identity spells the face's
//! own `name`-table family (name ID 1) plus its style, as a user would type it:
//! `bundled:Cascadia Mono PL Bold Italic`, `bundled:JetBrains Mono Bold`.
//!
//! A user who TYPES the family name — `font_family = "JetBrains Mono"` — gets an
//! INSTALLED JetBrains Mono when one resolves (exactly what the name meant
//! before the stack existed, so the variable `JetBrainsMono[wght].ttf` keeps its
//! `font_weight` / `wght` axis) and the bundled face only when none does
//! ([`crate::resolve_family_or_bundled`]). The same holds for `Cascadia Mono
//! PL`. `Cascadia Mono` (no PL) is a different family name, and it keeps naming
//! the system's variable face. Every consumer of a resolved identity reads its
//! bytes through [`crate::read_resolved_font`], never as a file.
//!
//! # Why not macOS
//!
//! The macOS default (SF Mono, W9-instantiated, plus Apple Symbols / STIX Two
//! Math in the symbol tier) is unchanged by the decision, so on macOS nothing
//! here would ever be selected by default — and a universal binary would carry
//! the ~2.8 MB twice. [`ACTIVE`] is therefore `false` there and every function
//! below answers "no bundled face" but for Iosevka Term's four identities,
//! reducing each default call site exactly to its pre-stack behaviour.

/// Whether the bundled stack — the defaults and the chain faces — is compiled
/// into this build. Iosevka Term has its own switch ([`IOSEVKA_ACTIVE`]).
pub const ACTIVE: bool = cfg!(all(
    feature = "bundled-font-stack",
    not(target_os = "macos")
));

/// Whether this build compiles in the Cascadia Mono PL faces: the stack is
/// active AND the target is Windows. Everywhere else the `CASCADIA_*`
/// identities below resolve to nothing.
pub const CASCADIA_ACTIVE: bool = cfg!(all(feature = "bundled-font-stack", windows));

/// The virtual identity scheme for bundled faces.
pub const SCHEME: &str = "bundled:";

/// JetBrains Mono's family name, as a user types it (name ID 1).
pub const JETBRAINS_FAMILY: &str = "JetBrains Mono";
/// The bundled JetBrains Mono's virtual identity (regular face).
pub const JETBRAINS_ID: &str = "bundled:JetBrains Mono";
/// The bundled JetBrains Mono's BOLD face identity.
pub const JETBRAINS_BOLD_ID: &str = "bundled:JetBrains Mono Bold";
/// The bundled JetBrains Mono's ITALIC face identity.
pub const JETBRAINS_ITALIC_ID: &str = "bundled:JetBrains Mono Italic";
/// The bundled JetBrains Mono's BOLD-ITALIC face identity.
pub const JETBRAINS_BOLD_ITALIC_ID: &str = "bundled:JetBrains Mono Bold Italic";

/// Cascadia Mono PL's family name, as a user types it (name ID 1 of all four
/// static 2407.24 faces). Compiled in on Windows only ([`CASCADIA_ACTIVE`]).
pub const CASCADIA_FAMILY: &str = "Cascadia Mono PL";
/// The bundled Cascadia Mono PL's virtual identity (regular face).
pub const CASCADIA_ID: &str = "bundled:Cascadia Mono PL";
/// The bundled Cascadia Mono PL's BOLD face identity.
pub const CASCADIA_BOLD_ID: &str = "bundled:Cascadia Mono PL Bold";
/// The bundled Cascadia Mono PL's ITALIC face identity.
pub const CASCADIA_ITALIC_ID: &str = "bundled:Cascadia Mono PL Italic";
/// The bundled Cascadia Mono PL's BOLD-ITALIC face identity.
pub const CASCADIA_BOLD_ITALIC_ID: &str = "bundled:Cascadia Mono PL Bold Italic";

/// Whether this build compiles in the Iosevka Term faces: wherever the stack
/// feature is on, macOS INCLUDED — unlike the rest of the stack it is never a
/// default and never a chain face, only a family a user names
/// (`font_family = "Iosevka Term"`), so it means the same face on every
/// machine ([`IOSEVKA_FAMILY`]).
pub const IOSEVKA_ACTIVE: bool = cfg!(feature = "bundled-font-stack");

/// Iosevka Term's family name, as a user types it (name ID 1 of all four
/// faces). Compiled in on every platform ([`IOSEVKA_ACTIVE`]).
pub const IOSEVKA_FAMILY: &str = "Iosevka Term";
/// The bundled Iosevka Term's virtual identity (regular face).
pub const IOSEVKA_ID: &str = "bundled:Iosevka Term";
/// The bundled Iosevka Term's BOLD face identity.
pub const IOSEVKA_BOLD_ID: &str = "bundled:Iosevka Term Bold";
/// The bundled Iosevka Term's ITALIC face identity.
pub const IOSEVKA_ITALIC_ID: &str = "bundled:Iosevka Term Italic";
/// The bundled Iosevka Term's BOLD-ITALIC face identity.
pub const IOSEVKA_BOLD_ITALIC_ID: &str = "bundled:Iosevka Term Bold Italic";

/// The bundled PRIMARY's family name, as a user types it: Cascadia Mono PL on
/// Windows, JetBrains Mono on every other non-macOS target.
#[cfg(windows)]
pub const PRIMARY_FAMILY: &str = CASCADIA_FAMILY;
/// The bundled PRIMARY's family name, as a user types it.
#[cfg(not(windows))]
pub const PRIMARY_FAMILY: &str = JETBRAINS_FAMILY;
/// The bundled primary's virtual identity (regular face).
#[cfg(windows)]
pub const PRIMARY_ID: &str = CASCADIA_ID;
/// The bundled primary's virtual identity (regular face).
#[cfg(not(windows))]
pub const PRIMARY_ID: &str = JETBRAINS_ID;
/// The bundled primary's BOLD face identity.
#[cfg(windows)]
pub const PRIMARY_BOLD_ID: &str = CASCADIA_BOLD_ID;
/// The bundled primary's BOLD face identity.
#[cfg(not(windows))]
pub const PRIMARY_BOLD_ID: &str = JETBRAINS_BOLD_ID;
/// The bundled primary's ITALIC face identity.
#[cfg(windows)]
pub const PRIMARY_ITALIC_ID: &str = CASCADIA_ITALIC_ID;
/// The bundled primary's ITALIC face identity.
#[cfg(not(windows))]
pub const PRIMARY_ITALIC_ID: &str = JETBRAINS_ITALIC_ID;
/// The bundled primary's BOLD-ITALIC face identity.
#[cfg(windows)]
pub const PRIMARY_BOLD_ITALIC_ID: &str = CASCADIA_BOLD_ITALIC_ID;
/// The bundled primary's BOLD-ITALIC face identity.
#[cfg(not(windows))]
pub const PRIMARY_BOLD_ITALIC_ID: &str = JETBRAINS_BOLD_ITALIC_ID;

/// The embedded DejaVu Sans Mono, as a BROAD-tier chain entry.
pub const DEJAVU_ID: &str = "bundled:DejaVu Sans Mono";
/// Noto Sans Symbols 2, as a SYMBOL-tier chain entry.
pub const NOTO_SYMBOLS2_ID: &str = "bundled:Noto Sans Symbols 2";
/// Noto Sans Math, as a SYMBOL-tier chain entry.
pub const NOTO_MATH_ID: &str = "bundled:Noto Sans Math";

/// Every identity this module can name, in declaration order. A build answers
/// bytes for the subset [`face_for_id`] serves.
const ALL_IDS: &[&str] = &[
    CASCADIA_ID,
    CASCADIA_BOLD_ID,
    CASCADIA_ITALIC_ID,
    CASCADIA_BOLD_ITALIC_ID,
    JETBRAINS_ID,
    JETBRAINS_BOLD_ID,
    JETBRAINS_ITALIC_ID,
    JETBRAINS_BOLD_ITALIC_ID,
    IOSEVKA_ID,
    IOSEVKA_BOLD_ID,
    IOSEVKA_ITALIC_ID,
    IOSEVKA_BOLD_ITALIC_ID,
    DEJAVU_ID,
    NOTO_SYMBOLS2_ID,
    NOTO_MATH_ID,
];

#[cfg(feature = "bundled-font-stack")]
mod bytes {
    #[cfg(not(target_os = "macos"))]
    pub(super) static JBM_REGULAR: &[u8] =
        include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");
    #[cfg(not(target_os = "macos"))]
    pub(super) static JBM_BOLD: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Bold.ttf");
    #[cfg(not(target_os = "macos"))]
    pub(super) static JBM_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/JetBrainsMono-Italic.ttf");
    #[cfg(not(target_os = "macos"))]
    pub(super) static JBM_BOLD_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/JetBrainsMono-BoldItalic.ttf");
    #[cfg(not(target_os = "macos"))]
    pub(super) static NOTO_SYMBOLS2: &[u8] =
        include_bytes!("../assets/bundled/NotoSansSymbols2-Regular.ttf");
    #[cfg(not(target_os = "macos"))]
    pub(super) static NOTO_MATH: &[u8] =
        include_bytes!("../assets/bundled/NotoSansMath-Regular.ttf");
    // Every platform, macOS included: the opt-in Iosevka Term (a subset,
    // `IOSEVKA_FAMILY`), selected only by name.
    pub(super) static IOSEVKA_REGULAR: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-Regular.ttf");
    pub(super) static IOSEVKA_BOLD: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-Bold.ttf");
    pub(super) static IOSEVKA_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-Italic.ttf");
    pub(super) static IOSEVKA_BOLD_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-BoldItalic.ttf");
    // Windows only: the Windows primary. A Linux binary never selects it, so
    // it does not carry the ~2.4 MB.
    #[cfg(windows)]
    pub(super) static CASCADIA_REGULAR: &[u8] =
        include_bytes!("../assets/bundled/CascadiaMonoPL-Regular.ttf");
    #[cfg(windows)]
    pub(super) static CASCADIA_BOLD: &[u8] =
        include_bytes!("../assets/bundled/CascadiaMonoPL-Bold.ttf");
    #[cfg(windows)]
    pub(super) static CASCADIA_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/CascadiaMonoPL-Italic.ttf");
    #[cfg(windows)]
    pub(super) static CASCADIA_BOLD_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/CascadiaMonoPL-BoldItalic.ttf");
}

/// The bytes behind one virtual identity, or `None` for anything that is not a
/// bundled identity in THIS build (on macOS: the Iosevka Term faces alone).
#[cfg(feature = "bundled-font-stack")]
#[must_use]
pub fn face_for_id(id: &str) -> Option<&'static [u8]> {
    match id {
        #[cfg(windows)]
        CASCADIA_ID => Some(bytes::CASCADIA_REGULAR),
        #[cfg(windows)]
        CASCADIA_BOLD_ID => Some(bytes::CASCADIA_BOLD),
        #[cfg(windows)]
        CASCADIA_ITALIC_ID => Some(bytes::CASCADIA_ITALIC),
        #[cfg(windows)]
        CASCADIA_BOLD_ITALIC_ID => Some(bytes::CASCADIA_BOLD_ITALIC),
        #[cfg(not(target_os = "macos"))]
        JETBRAINS_ID => Some(bytes::JBM_REGULAR),
        #[cfg(not(target_os = "macos"))]
        JETBRAINS_BOLD_ID => Some(bytes::JBM_BOLD),
        #[cfg(not(target_os = "macos"))]
        JETBRAINS_ITALIC_ID => Some(bytes::JBM_ITALIC),
        #[cfg(not(target_os = "macos"))]
        JETBRAINS_BOLD_ITALIC_ID => Some(bytes::JBM_BOLD_ITALIC),
        #[cfg(not(target_os = "macos"))]
        DEJAVU_ID => Some(crate::embedded_font()),
        #[cfg(not(target_os = "macos"))]
        NOTO_SYMBOLS2_ID => Some(bytes::NOTO_SYMBOLS2),
        #[cfg(not(target_os = "macos"))]
        NOTO_MATH_ID => Some(bytes::NOTO_MATH),
        IOSEVKA_ID => Some(bytes::IOSEVKA_REGULAR),
        IOSEVKA_BOLD_ID => Some(bytes::IOSEVKA_BOLD),
        IOSEVKA_ITALIC_ID => Some(bytes::IOSEVKA_ITALIC),
        IOSEVKA_BOLD_ITALIC_ID => Some(bytes::IOSEVKA_BOLD_ITALIC),
        _ => None,
    }
}

/// The bytes behind one virtual identity: none, the stack being compiled out.
#[cfg(not(feature = "bundled-font-stack"))]
#[must_use]
pub fn face_for_id(_id: &str) -> Option<&'static [u8]> {
    None
}

/// Whether `path` is a bundled virtual identity in this build.
#[must_use]
pub fn is_bundled_id(path: &str) -> bool {
    face_for_id(path).is_some()
}

/// Whether `family` is spelled as a bundled virtual IDENTITY (`bundled:…`)
/// rather than a family name — the spelling that always means the compiled-in
/// face, never an installed one.
#[must_use]
pub fn is_identity(family: &str) -> bool {
    family.trim().starts_with(SCHEME)
}

/// Resolve a config-authored FAMILY request to a bundled face: its virtual
/// identity and bytes. Matches the identity itself (`bundled:JetBrains Mono`)
/// and the family NAME a user types, case- and separator-insensitively
/// (`JetBrains Mono`, `jetbrains-mono`, `JetBrainsMono`), plus the three styled
/// spellings (`JetBrains Mono Bold` / `Italic` / `Bold Italic`) so the
/// `font_family_bold`/`_italic`/`_bold_italic` keys can name them too — and, on
/// Windows, the same four spellings of `Cascadia Mono PL`. The fallback-only
/// faces (DejaVu, Noto) resolve by identity only: a family name such as
/// "DejaVu Sans Mono" keeps meaning the INSTALLED face, as before. So does
/// `Cascadia Mono`, which names the system's variable face, not this cut.
#[must_use]
pub fn face_for_family(family: &str) -> Option<(&'static str, &'static [u8])> {
    let trimmed = family.trim();
    if let Some(&id) = ALL_IDS.iter().find(|&&id| trimmed == id) {
        return face_for_id(id).map(|bytes| (id, bytes));
    }
    let id = match normalize(trimmed).as_str() {
        "jetbrainsmono" | "jetbrainsmonoregular" => JETBRAINS_ID,
        "jetbrainsmonobold" => JETBRAINS_BOLD_ID,
        "jetbrainsmonoitalic" => JETBRAINS_ITALIC_ID,
        "jetbrainsmonobolditalic" => JETBRAINS_BOLD_ITALIC_ID,
        "iosevkaterm" | "iosevkatermregular" => IOSEVKA_ID,
        "iosevkatermbold" => IOSEVKA_BOLD_ID,
        "iosevkatermitalic" => IOSEVKA_ITALIC_ID,
        "iosevkatermbolditalic" => IOSEVKA_BOLD_ITALIC_ID,
        #[cfg(windows)]
        "cascadiamonopl" | "cascadiamonoplregular" => CASCADIA_ID,
        #[cfg(windows)]
        "cascadiamonoplbold" => CASCADIA_BOLD_ID,
        #[cfg(windows)]
        "cascadiamonoplitalic" => CASCADIA_ITALIC_ID,
        #[cfg(windows)]
        "cascadiamonoplbolditalic" => CASCADIA_BOLD_ITALIC_ID,
        _ => return None,
    };
    face_for_id(id).map(|bytes| (id, bytes))
}

/// The real `[bold, italic, bold-italic]` faces of a bundled primary, keyed by
/// the primary's identity — what `Renderer::ensure_styled_faces` fills its
/// slots from instead of deriving sibling FILE paths. Empty for any other
/// primary. Both bundled primaries carry all three: the default one, and
/// JetBrains Mono chosen by name on Windows.
#[must_use]
pub fn styled_siblings(primary: &str) -> [Option<&'static [u8]>; 3] {
    let ids = match primary {
        JETBRAINS_ID => [
            JETBRAINS_BOLD_ID,
            JETBRAINS_ITALIC_ID,
            JETBRAINS_BOLD_ITALIC_ID,
        ],
        #[cfg(windows)]
        CASCADIA_ID => [
            CASCADIA_BOLD_ID,
            CASCADIA_ITALIC_ID,
            CASCADIA_BOLD_ITALIC_ID,
        ],
        IOSEVKA_ID => [IOSEVKA_BOLD_ID, IOSEVKA_ITALIC_ID, IOSEVKA_BOLD_ITALIC_ID],
        _ => return [None; 3],
    };
    ids.map(face_for_id)
}

/// The four-face families a bundled PRIMARY can come from, each in
/// `[regular, bold, italic, bold-italic]` order: the two defaults, and Iosevka
/// Term, which only a user's choice makes the primary.
const PRIMARY_FAMILIES: [[&str; 4]; 3] = [
    [
        CASCADIA_ID,
        CASCADIA_BOLD_ID,
        CASCADIA_ITALIC_ID,
        CASCADIA_BOLD_ITALIC_ID,
    ],
    [
        JETBRAINS_ID,
        JETBRAINS_BOLD_ID,
        JETBRAINS_ITALIC_ID,
        JETBRAINS_BOLD_ITALIC_ID,
    ],
    [
        IOSEVKA_ID,
        IOSEVKA_BOLD_ID,
        IOSEVKA_ITALIC_ID,
        IOSEVKA_BOLD_ITALIC_ID,
    ],
];

/// THE CELL HOLDS THE FACE'S OWN LETTERS: when `bytes` is one of a bundled
/// primary family's faces, how far the deepest LATIN LETTER of the whole
/// family (all four faces: a bold cell is drawn in the regular face's cell)
/// reaches below the baseline, per em and NEGATIVE (the descent convention);
/// `None` for every other face. Where this is deeper than the face's own line
/// descent, the renderer's cell descent is this plus one pixel for the hinter
/// (`letter_floored_descent` in the crate root); a family whose letters keep
/// inside its line is untouched.
///
/// MEASURED 2026-09-30 over every letter of U+0041..U+024F (Basic Latin,
/// Latin-1, Latin Extended-A and -B) in each face's `glyf` boxes:
///
/// * Cascadia Mono PL 2407.24: the comma-below and cedilla letters — Romanian
///   `ș ț Ș Ț`, Latvian `ķ ļ ņ ŗ Ģ Ķ Ļ Ņ Ŗ` — reach 568 of 2048 units (Regular,
///   Italic) and 608 (Bold, Bold Italic), past a line descent of 480. On the
///   vendor's line the 16 px cell (9x19, baseline 15) cut their commas off (a
///   regular `ș` drew 13 of its 14 rows, a bold one 13 of 15) and a `j`
///   reached the cell's last row, so it fused with the caron of an `Š` drawn
///   on the next row: the hinter lifts Cascadia's capitals from 11.09 px to
///   12 at 16 px, and their accents ink the row above the cell. On this floor
///   the cell is 9x21 at 16 px, same baseline: every comma whole, at every
///   size from 8 to 64 px (no row of 1/8 coverage or more past the cell), and
///   one clear row between a `j` and the accents under it at 16 px. That row
///   is not there at every size — 0 at 10, 11, 15, 18, 20, 22, 24 and 33 px,
///   where they touch, and the hinted accents overlap the descenders by a
///   row at 9 and 14 px — as the hinter's rounding falls.
/// * JetBrains Mono 2.304: 240 of 1000, inside its 300 descent. No change.
/// * Iosevka Term 34.9.0 (measured 2026-10-14, all four faces): 256 of 1000,
///   inside its 285 descent (ascent 965, no lineGap: a 1.25 em line). No
///   change.
///
/// Only the bundled families, whose bytes this module owns and which were
/// measured. An installed face — the system's variable Cascadia Mono has the
/// same 480 descent — keeps its vendor line, as it always drew.
#[must_use]
pub fn letter_descent_floor(bytes: &[u8]) -> Option<f32> {
    static MEMO: [std::sync::OnceLock<Option<f32>>; PRIMARY_FAMILIES.len()] =
        [const { std::sync::OnceLock::new() }; PRIMARY_FAMILIES.len()];
    let family = PRIMARY_FAMILIES.iter().position(|ids| {
        ids.iter()
            .any(|id| face_for_id(id).is_some_and(|face| same_face(face, bytes)))
    })?;
    *MEMO[family].get_or_init(|| deepest_latin_letter(&PRIMARY_FAMILIES[family]))
}

/// Whether `bytes` ARE the bundled `face`: the same allocation, or the same
/// contents (a renderer may hold its own copy of the bytes, through the face
/// store or a host push). The length check keeps a different face from
/// costing a comparison.
fn same_face(face: &[u8], bytes: &[u8]) -> bool {
    face.len() == bytes.len() && (std::ptr::eq(face.as_ptr(), bytes.as_ptr()) || face == bytes)
}

/// The bundled faces whose PRIVATE-USE glyphs YIELD ([`yields_private_use`]):
/// the four Cascadia Mono PL faces, `[regular, bold, italic, bold-italic]`.
const PRIVATE_USE_YIELDING_IDS: [&str; 4] = PRIMARY_FAMILIES[0];

/// THE ICONS COME FROM ONE FACE: whether `bytes` are a bundled face that
/// leaves every PRIVATE-USE code point the embedded Symbols Nerd Font maps to
/// that face, instead of drawing it itself. True for the four Cascadia Mono PL
/// faces (compiled in on Windows only, so `false` everywhere else), whichever
/// slot holds them — the primary, an injected bold face, a styled slot. The
/// renderer reads it once per loaded face and applies it per code point
/// (`Renderer::primary_unicode_gid` and its styled twins), only while the
/// runtime tier that answers from the Nerd face is open.
///
/// MEASURED 2026-10-01 on the 2407.24 static faces and the embedded Symbols
/// Nerd Font Mono, cmaps read with WPF's `GlyphTypeface`, renders by aterm
/// 0.101.0 at the default 16 px:
///
/// * each of the four faces maps 38 private code points — the Powerline
///   U+E0A0..E0A3 and Powerline-extra U+E0B0..E0C8, U+E0CA, U+E0CC..E0D2,
///   U+E0D4 — and the Nerd face maps every one of them;
/// * those glyphs are drawn for a taller line than the face's own: the
///   trapezoid U+E0D2 reaches 0.159 em above the 0.928 em ascent and down to
///   the 0.234 em descent, 1.32 em in all. In the 9x21 cell (baseline row 15)
///   it is 22 px tall, rows -3..18: three rows painted into the row above and
///   rows 19..20 left showing the next segment's colour. The flame U+E0C0 and
///   the branch U+E0A0 did the same, on GPU and CPU alike. Under JetBrains
///   Mono the same points kept inside the cell.
///
/// Yielding sends them to the face every other Nerd Font icon on Windows is
/// drawn from (`FOREIGN_PRIVATE_USE_FACES` in the crate root routes Segoe UI
/// Symbol's and Arial's there too), through the fallback FIT: the ink is
/// fitted to the cell box, kept level with its own centre and clamped to the
/// row band. The separators U+E0B0..E0BF never reach a face: the procedural
/// tier draws them first, cell-exact. What the fit gives the rest, measured
/// at 16 px and pinned by `the_yielded_powerline_glyphs_draw_inside_their_cell_at_16_px`:
/// the branch and the lock keep their height (17 and 15 of 21 rows), while
/// the 1-em-wide Powerline-extra flames and trapezoids shrink to the 9-px
/// cell by their width — 9 rows, centred, no longer the full-height
/// separators the face's own glyphs were meant to be, exactly as they drew
/// under JetBrains Mono.
///
/// BY THE BYTES, like [`letter_descent_floor`], because a face can arrive
/// without its identity (`Renderer::set_primary_font` takes bytes; the host's
/// bold and styled pushes too). A file with the same bytes IS this face, and
/// the floor already gives it the same cell. Any OTHER face keeps every glyph
/// it maps: the system's variable Cascadia Mono, a Nerd-patched font a user
/// configures, another PL build, JetBrains Mono.
#[must_use]
pub fn yields_private_use(bytes: &[u8]) -> bool {
    PRIVATE_USE_YIELDING_IDS
        .iter()
        .any(|id| face_for_id(id).is_some_and(|face| same_face(face, bytes)))
}

/// The deepest `glyf` box bottom of any Latin LETTER across `ids`' faces, per
/// em (NEGATIVE below the baseline). `None` when no face parses.
fn deepest_latin_letter(ids: &[&str; 4]) -> Option<f32> {
    let mut deepest: Option<f32> = None;
    for face in ids
        .iter()
        .filter_map(|id| face_for_id(id))
        .filter_map(|bytes| ttf_parser::Face::parse(bytes, 0).ok())
    {
        let upem = f32::from(face.units_per_em());
        if upem <= 0.0 {
            continue;
        }
        for ch in ('A'..='z')
            .chain('\u{C0}'..='\u{24F}')
            .filter(|c| c.is_alphabetic())
        {
            if let Some(b) = face
                .glyph_index(ch)
                .and_then(|g| face.glyph_bounding_box(g))
            {
                let y = f32::from(b.y_min) / upem;
                deepest = Some(deepest.map_or(y, |d| d.min(y)));
            }
        }
    }
    deepest
}

/// The DEFAULT primary candidate: the bundled primary's identity (Cascadia
/// Mono PL on Windows, JetBrains Mono elsewhere), placed ahead of the built-in
/// system `FONT_CANDIDATES` (so a system DejaVu Sans Mono — or, on Windows, the
/// system's variable Cascadia Mono — no longer wins by default) and behind any
/// configured family (so a user's choice still wins). Empty when the stack is
/// inactive.
#[must_use]
pub fn default_primary_candidates() -> &'static [&'static str] {
    if ACTIVE { &[PRIMARY_ID] } else { &[] }
}

/// The bundled BROAD-tier chain entries, placed after the user's configured
/// `fallback_fonts` and ahead of every system discovery path. Additive: the
/// scan keeps going past them to the system per-script faces and backstop.
///
/// DejaVu Sans Mono alone, on Windows too. JetBrains Mono ahead of it there
/// was tried and measured worse (2026-09-30, renders at the default 16 px):
/// its arrows and keycaps fill or overhang its 0.6 em advance (`⇔` 0.76 em,
/// `⌫` 0.82 em), wider than Cascadia Mono PL's 9 px cell, so the symbol fit
/// shrinks each by its own hinted raster's fringe: `⇐` drew 7x8 px beside a
/// 9x10 `⇒`, `⇔ ⌘ ⌫` shrank to blobs and `∴ ∵` spread their dots. DejaVu's
/// arrows (0.538 em of ink) fit the cell unscaled, mirrored pairs draw alike,
/// and `⇦ ⇨` come from one face. What JetBrains Mono drew better (`✗` as a
/// plain cross, a filled `⚠`) is a style, not a defect.
#[must_use]
pub fn broad_fallback_ids() -> &'static [&'static str] {
    if ACTIVE { &[DEJAVU_ID] } else { &[] }
}

/// The bundled SYMBOL-tier chain entries, in order, placed after a configured
/// `symbol_font` and ahead of the system symbol faces. Additive.
#[must_use]
pub fn symbol_fallback_ids() -> &'static [&'static str] {
    if ACTIVE {
        &[NOTO_SYMBOLS2_ID, NOTO_MATH_ID]
    } else {
        &[]
    }
}

/// The FILE NAMES of the system faces the bundled symbol tier already
/// carries — the same upstream Noto files, compiled in.
const SUPERSEDED_SYSTEM_FILES: &[&str] =
    &["NotoSansSymbols2-Regular.ttf", "NotoSansMath-Regular.ttf"];

/// Whether a built-in SYSTEM candidate `path` names a face the bundled stack
/// already serves ahead of it, by file name. Such an entry can only ever
/// answer a code point the bundled copy answered first, so the discovery list
/// drops it — and with it the font-tree walk that relocating a missing copy
/// used to start on every launch of a host that does not install it.
#[must_use]
pub fn supersedes_system_face(path: &str) -> bool {
    ACTIVE
        && std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| SUPERSEDED_SYSTEM_FILES.contains(&n))
}

/// Families the bundled stack makes selectable by NAME (for `list-fonts`): the
/// primary, on Windows JetBrains Mono beside it, and Iosevka Term everywhere
/// (macOS included, where it is the one).
#[must_use]
pub fn selectable_families() -> &'static [&'static str] {
    if !IOSEVKA_ACTIVE {
        return &[];
    }
    #[cfg(windows)]
    {
        &[CASCADIA_FAMILY, JETBRAINS_FAMILY, IOSEVKA_FAMILY]
    }
    #[cfg(target_os = "macos")]
    {
        &[IOSEVKA_FAMILY]
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        &[JETBRAINS_FAMILY, IOSEVKA_FAMILY]
    }
}

/// The resolver's family normalization (lowercase, no whitespace / `-` / `_`).
fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-' && *ch != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_names_resolve_to_the_bundled_faces_only_when_active() {
        for spelling in [
            "JetBrains Mono",
            "jetbrains-mono",
            "JetBrainsMono",
            " JETBRAINS MONO ",
        ] {
            let hit = face_for_family(spelling);
            assert_eq!(hit.is_some(), ACTIVE, "{spelling:?}");
            if let Some((id, _)) = hit {
                assert_eq!(id, JETBRAINS_ID);
            }
        }
        assert_eq!(
            face_for_family("JetBrains Mono Bold Italic").map(|(id, _)| id),
            ACTIVE.then_some(JETBRAINS_BOLD_ITALIC_ID)
        );
        // Cascadia Mono PL resolves by name, in all four styles, exactly where
        // it is compiled in (Windows), and nowhere else.
        for (spelling, id) in [
            ("Cascadia Mono PL", CASCADIA_ID),
            ("cascadia-mono-pl", CASCADIA_ID),
            ("CascadiaMonoPL", CASCADIA_ID),
            (" CASCADIA MONO PL REGULAR ", CASCADIA_ID),
            ("Cascadia Mono PL Bold", CASCADIA_BOLD_ID),
            ("Cascadia Mono PL Italic", CASCADIA_ITALIC_ID),
            ("Cascadia Mono PL Bold Italic", CASCADIA_BOLD_ITALIC_ID),
            (CASCADIA_ID, CASCADIA_ID),
        ] {
            assert_eq!(
                face_for_family(spelling).map(|(id, _)| id),
                CASCADIA_ACTIVE.then_some(id),
                "{spelling:?}"
            );
        }
        // Iosevka Term resolves by name, in all four styles, wherever the stack
        // feature is on — macOS included.
        for (spelling, id) in [
            ("Iosevka Term", IOSEVKA_ID),
            ("iosevka-term", IOSEVKA_ID),
            ("IosevkaTerm", IOSEVKA_ID),
            (" IOSEVKA TERM REGULAR ", IOSEVKA_ID),
            ("Iosevka Term Bold", IOSEVKA_BOLD_ID),
            ("Iosevka Term Italic", IOSEVKA_ITALIC_ID),
            ("Iosevka Term Bold Italic", IOSEVKA_BOLD_ITALIC_ID),
            (IOSEVKA_ID, IOSEVKA_ID),
        ] {
            assert_eq!(
                face_for_family(spelling).map(|(id, _)| id),
                IOSEVKA_ACTIVE.then_some(id),
                "{spelling:?}"
            );
        }
        // An installed family keeps meaning the installed face — including
        // `Cascadia Mono`, the system's variable cut, which is not this one,
        // and `Iosevka` (not Term), whose arrows are wide.
        assert!(face_for_family("Iosevka").is_none());
        assert!(face_for_family("DejaVu Sans Mono").is_none());
        assert!(face_for_family("JetBrains Mono NL").is_none());
        assert!(face_for_family("Cascadia Mono").is_none());
        assert!(face_for_family("Cascadia Code").is_none());
        assert!(face_for_family("Cascadia Code PL").is_none());
        assert!(face_for_family("").is_none());
    }

    /// The per-platform PRIMARY aliases name the face the decision picked:
    /// Cascadia Mono PL on Windows, JetBrains Mono elsewhere.
    #[test]
    fn the_primary_aliases_follow_the_platform() {
        if cfg!(windows) {
            assert_eq!(
                [PRIMARY_FAMILY, PRIMARY_ID, PRIMARY_BOLD_ID],
                [CASCADIA_FAMILY, CASCADIA_ID, CASCADIA_BOLD_ID]
            );
            assert_eq!(
                [PRIMARY_ITALIC_ID, PRIMARY_BOLD_ITALIC_ID],
                [CASCADIA_ITALIC_ID, CASCADIA_BOLD_ITALIC_ID]
            );
        } else {
            assert_eq!(
                [PRIMARY_FAMILY, PRIMARY_ID, PRIMARY_BOLD_ID],
                [JETBRAINS_FAMILY, JETBRAINS_ID, JETBRAINS_BOLD_ID]
            );
            assert_eq!(
                [PRIMARY_ITALIC_ID, PRIMARY_BOLD_ITALIC_ID],
                [JETBRAINS_ITALIC_ID, JETBRAINS_BOLD_ITALIC_ID]
            );
        }
        // Linux carries no Cascadia bytes at all.
        assert_eq!(CASCADIA_ACTIVE, ACTIVE && cfg!(windows));
        assert_eq!(face_for_id(CASCADIA_ID).is_some(), CASCADIA_ACTIVE);
        let order = selectable_families();
        if !IOSEVKA_ACTIVE {
            assert!(order.is_empty());
        } else if cfg!(windows) {
            assert_eq!(order, [CASCADIA_FAMILY, JETBRAINS_FAMILY, IOSEVKA_FAMILY]);
        } else if cfg!(target_os = "macos") {
            assert_eq!(order, [IOSEVKA_FAMILY]);
        } else {
            assert_eq!(order, [JETBRAINS_FAMILY, IOSEVKA_FAMILY]);
        }
        // Iosevka Term is no platform's default and no chain face.
        assert!(!default_primary_candidates().contains(&IOSEVKA_ID));
        assert!(!broad_fallback_ids().contains(&IOSEVKA_ID));
        assert!(!symbol_fallback_ids().contains(&IOSEVKA_ID));
        // The broad tier is DejaVu Sans Mono alone on every platform: JetBrains
        // Mono is selectable on Windows, never a chain face there.
        let broad: &[&str] = if ACTIVE { &[DEJAVU_ID] } else { &[] };
        assert_eq!(broad_fallback_ids(), broad);
    }

    /// THE LETTER FLOOR is measured from the family's own outlines, answers
    /// for each of its four faces, and is a property of the BUNDLED bytes only.
    #[test]
    fn the_letter_floor_is_the_familys_deepest_latin_letter() {
        let per_em = |id: &str| letter_descent_floor(face_for_id(id).unwrap());
        if IOSEVKA_ACTIVE {
            // Iosevka Term: 256 of 1000 (the comma-below letters), inside
            // its 285 descent — the floor exists and changes nothing.
            for id in PRIMARY_FAMILIES[2] {
                assert_eq!(per_em(id), Some(-0.256), "{id}");
            }
        }
        if !ACTIVE {
            #[cfg(feature = "embedded-font")]
            assert!(letter_descent_floor(crate::embedded_font()).is_none());
            return;
        }
        // JetBrains Mono: 240 of 1000 (the comma-below letters), inside its
        // 300 descent — the floor exists and changes nothing.
        for id in PRIMARY_FAMILIES[1] {
            assert_eq!(per_em(id), Some(-0.24), "{id}");
        }
        if CASCADIA_ACTIVE {
            // Cascadia Mono PL: the BOLD faces' 608 of 2048 governs all four.
            for id in PRIMARY_FAMILIES[0] {
                assert_eq!(per_em(id), Some(-608.0 / 2048.0), "{id}");
            }
        }
        // A copy of the bytes is the same face; a chain face, another face.
        let copy = face_for_id(PRIMARY_ID).unwrap().to_vec();
        assert_eq!(
            letter_descent_floor(&copy),
            letter_descent_floor(face_for_id(PRIMARY_ID).unwrap())
        );
        assert!(letter_descent_floor(face_for_id(DEJAVU_ID).unwrap()).is_none());
        assert!(letter_descent_floor(face_for_id(NOTO_MATH_ID).unwrap()).is_none());
    }

    /// Every identity this build serves parses, and each four-face family is a
    /// real monospace family whose files ARE the Regular / Bold / Italic /
    /// Bold Italic their identities claim — by the faces' own OS/2 weight and
    /// italic bit, and by their `name`-table family (name ID 1).
    #[test]
    fn every_bundled_face_parses_and_each_family_is_a_real_monospace_family() {
        let served: Vec<&str> = ALL_IDS
            .iter()
            .copied()
            .filter(|id| face_for_id(id).is_some())
            .collect();
        let expected_count = usize::from(ACTIVE) * 7
            + usize::from(CASCADIA_ACTIVE) * 4
            + usize::from(IOSEVKA_ACTIVE) * 4;
        assert_eq!(served.len(), expected_count, "{served:?}");
        for id in &served {
            let bytes = face_for_id(id).expect("filtered to served ids");
            let face = ttf_parser::Face::parse(bytes, 0).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(face.number_of_glyphs() > 100, "{id}");
        }
        let mut families = Vec::new();
        if ACTIVE {
            families.push((JETBRAINS_FAMILY, PRIMARY_FAMILIES[1]));
        }
        if IOSEVKA_ACTIVE {
            families.push((IOSEVKA_FAMILY, PRIMARY_FAMILIES[2]));
        }
        if CASCADIA_ACTIVE {
            families.push((
                CASCADIA_FAMILY,
                [
                    CASCADIA_ID,
                    CASCADIA_BOLD_ID,
                    CASCADIA_ITALIC_ID,
                    CASCADIA_BOLD_ITALIC_ID,
                ],
            ));
        }
        for (family, ids) in families {
            let weights: Vec<(u16, bool)> = ids
                .iter()
                .map(|id| {
                    let f = ttf_parser::Face::parse(face_for_id(id).unwrap(), 0).unwrap();
                    assert!(f.is_monospaced(), "{id} must be monospaced");
                    assert!(
                        f.variation_axes().into_iter().next().is_none(),
                        "{id} must be a STATIC face"
                    );
                    let names = f.names();
                    let name = (0..names.len()).find_map(|i| {
                        let n = names.get(i)?;
                        (n.name_id == ttf_parser::name_id::FAMILY)
                            .then(|| n.to_string())
                            .flatten()
                    });
                    assert_eq!(name.as_deref(), Some(family), "{id}: name ID 1");
                    (f.weight().to_number(), f.is_italic())
                })
                .collect();
            assert_eq!(
                weights,
                [(400, false), (700, false), (400, true), (700, true)],
                "{family}"
            );
        }
    }

    // ---- the stack, bound to the real renderer ------------------------------

    use crate::{FaceId, GlyphClass, Renderer, Theme};

    /// The default renderer a GUI window builds when nothing is configured,
    /// sealed exactly as the backend worker seals it.
    fn default_sealed() -> Option<Renderer> {
        if !ACTIVE {
            eprintln!("SKIP: bundled stack inactive");
            return None;
        }
        let mut r = Renderer::from_system(18.0, Theme::default()).expect("a default face");
        r.seal_admitted_font_sources();
        Some(r)
    }

    /// Where `ch` resolves in the sealed default chain, as a readable label:
    /// the bundled identity or system path of the face that draws it.
    fn resolved_face(r: &mut Renderer, ch: char) -> String {
        let key = r.glyph_key(ch);
        match key.source {
            FaceId::Primary if key.glyph_class == GlyphClass::MonoGid => {
                format!("primary:{}", r.primary_source_path().unwrap_or("-"))
            }
            FaceId::Primary => "NOTDEF".to_string(),
            FaceId::Fallback => format!(
                "fallback:{}",
                r.fallback_pick
                    .get(&ch)
                    .and_then(|&i| r.fallback_chain.get(i))
                    .and_then(|f| f.path.clone())
                    .unwrap_or_default()
            ),
            FaceId::SymbolFallback => format!(
                "symbol:{}",
                r.symbol_chain_pick(ch)
                    .and_then(|i| r.symbol_chain.get(i))
                    .and_then(|f| f.path.clone())
                    .unwrap_or_default()
            ),
            other => format!("{other:?}"),
        }
    }

    /// DEFAULT SELECTION: with nothing configured, the primary is the bundled
    /// platform face — Cascadia Mono PL on Windows (not the system's variable
    /// `CascadiaMono.ttf`, the first Windows candidate), JetBrains Mono on
    /// Linux (not the system DejaVu Sans Mono `fc-match monospace` answers,
    /// which is the first system candidate and used to win).
    #[test]
    fn the_default_primary_is_the_bundled_platform_face() {
        if !ACTIVE {
            eprintln!("SKIP: bundled stack inactive");
            return;
        }
        let expected = if cfg!(windows) {
            CASCADIA_ID
        } else {
            JETBRAINS_ID
        };
        assert_eq!(PRIMARY_ID, expected);
        let r = Renderer::from_system(18.0, Theme::default()).expect("a default face");
        assert_eq!(r.primary_source_path(), Some(expected));
        let (bytes, _) = r.chrome_primary_face().expect("primary bytes");
        assert_eq!(&bytes[..], face_for_id(expected).unwrap());
        // The candidate list the off-thread config worker walks agrees.
        let candidates = crate::primary_font_candidate_paths();
        assert_eq!(candidates.first().map(String::as_str), Some(expected));
    }

    /// A CONFIGURED family still wins over the bundled default, by name or by
    /// path; and every bundled family a build makes selectable resolves by
    /// NAME through every resolver (startup, config admission, the catalogue
    /// worker) — on Windows both Cascadia Mono PL and JetBrains Mono.
    #[test]
    fn a_configured_font_still_wins_and_the_bundled_families_resolve_by_name() {
        if !ACTIVE {
            return;
        }
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/DejaVuSansMono.ttf");
        let r = Renderer::from_system_with_family(Some(path), 18.0, Theme::default()).unwrap();
        assert_eq!(
            r.primary_source_path(),
            Some(path),
            "a configured path wins"
        );

        // (family, regular id, bold id, cell at the 16 px probe). JetBrains
        // Mono's own line is 1.32 em (ascent 1020 + descent 300 per 1000 upem,
        // no lineGap) and its advance 0.6 em: ceil(21.12) x round(9.6). Cascadia
        // Mono PL's line at 16 px is its 1900 / 2048 ascent (14.84 px) over the
        // depth of its deepest letter, 608 / 2048 (4.75 px), plus the hinter's
        // pixel — not the vendor's 480 (`letter_descent_floor`) — no lineGap,
        // and its advance 1200 / 2048 em: ceil(20.59) x round(9.375). The live
        // renderer ROUNDS the line (`cell_h_baseline`), so a window draws
        // JetBrains Mono 10x21 and Cascadia Mono PL 9x21 at 16 px.
        let mut families = vec![(JETBRAINS_FAMILY, JETBRAINS_ID, JETBRAINS_BOLD_ID, (10, 22))];
        if CASCADIA_ACTIVE {
            families.push((CASCADIA_FAMILY, CASCADIA_ID, CASCADIA_BOLD_ID, (9, 21)));
        }
        let mut checked = 0usize;
        for (family, id, bold_id, cell) in families {
            assert!(
                selectable_families().contains(&family),
                "{family} is bundled but not listed as selectable"
            );
            // The NAME yields to an installed copy (see
            // `resolve_family_or_bundled`, and the catalogue's fixture test for
            // that half); on a host with none, it is the bundled face everywhere.
            if crate::resolve_font_family(family).is_some() {
                eprintln!("SKIP by-name half: {family} is installed on this host");
                continue;
            }
            for spelling in [family, id] {
                let r = Renderer::from_system_with_family(Some(spelling), 18.0, Theme::default())
                    .unwrap();
                assert_eq!(r.primary_source_path(), Some(id), "{spelling}");
                assert_eq!(crate::resolve_config_font(spelling).as_deref(), Ok(id));
            }
            let batch = crate::font_catalog::resolve_and_admit(&[
                family.to_string(),
                format!("{family} Bold"),
            ]);
            let admitted: Vec<_> = batch
                .entries
                .iter()
                .map(|e| e.result.as_ref().map(|a| a.path.clone()).ok())
                .collect();
            assert_eq!(admitted, [Some(id.to_string()), Some(bold_id.to_string())]);
            assert!(crate::list_fonts().iter().any(|f| f == family), "{family}");
            let info = crate::face_info(family).expect("face_info resolves the bundle");
            assert_eq!(info.path, id);
            assert_eq!((info.cell_width, info.cell_height), cell, "{family}");
            checked += 1;
        }
        eprintln!("{checked} bundled famil(ies) resolved by name");
    }

    /// IOSEVKA TERM BY NAME, on every platform the stack feature is on —
    /// macOS included, where nothing else of the stack is compiled in: the
    /// family name and its identity resolve to the compiled-in faces (an
    /// installed Iosevka Term would win the name), a renderer built on it
    /// fills its three styled slots from the family's REAL Bold, Italic and
    /// Bold Italic faces (no shear, no dilation), `list-fonts` names it, and
    /// its cell at the 16 px probe is its own: a 0.5 em advance and a 1.25
    /// em line, 8x20 — two cells narrower than JetBrains Mono's 10, so a
    /// window holds a quarter more columns. Its private-use glyphs are its
    /// own: its Powerline points (U+E0A0..E0A3, U+E0B0..E0B3) stand inside
    /// its line (-285..965 of 1000), so it does not yield them.
    #[test]
    fn iosevka_term_is_selectable_by_name_on_every_platform() {
        if !IOSEVKA_ACTIVE {
            return;
        }
        assert!(selectable_families().contains(&IOSEVKA_FAMILY));
        assert!(crate::list_fonts().iter().any(|f| f == IOSEVKA_FAMILY));
        for id in PRIMARY_FAMILIES[2] {
            assert!(!yields_private_use(face_for_id(id).unwrap()), "{id}");
        }
        if crate::resolve_font_family(IOSEVKA_FAMILY).is_some() {
            eprintln!("SKIP by-name half: {IOSEVKA_FAMILY} is installed on this host");
            return;
        }
        for spelling in [IOSEVKA_FAMILY, IOSEVKA_ID] {
            assert_eq!(
                crate::resolve_config_font(spelling).as_deref(),
                Ok(IOSEVKA_ID)
            );
            let mut r =
                Renderer::from_system_with_family(Some(spelling), 18.0, Theme::default()).unwrap();
            r.seal_admitted_font_sources();
            assert_eq!(r.primary_source_path(), Some(IOSEVKA_ID), "{spelling}");
            assert_eq!(r.debug_styled_face_indices(), [Some(0); 3]);
            for (slot, id) in r.styled_faces.iter().zip(&PRIMARY_FAMILIES[2][1..]) {
                let face = slot.as_ref().expect("slot filled");
                assert_eq!(&face.bytes[..], face_for_id(id).unwrap(), "{id}");
            }
        }
        let batch = crate::font_catalog::resolve_and_admit(&[
            IOSEVKA_FAMILY.to_string(),
            format!("{IOSEVKA_FAMILY} Bold Italic"),
        ]);
        let admitted: Vec<_> = batch
            .entries
            .iter()
            .map(|e| e.result.as_ref().map(|a| a.path.clone()).ok())
            .collect();
        assert_eq!(
            admitted,
            [
                Some(IOSEVKA_ID.to_string()),
                Some(IOSEVKA_BOLD_ITALIC_ID.to_string())
            ]
        );
        let info = crate::face_info(IOSEVKA_FAMILY).expect("face_info resolves the bundle");
        assert_eq!(info.path, IOSEVKA_ID);
        assert_eq!((info.cell_width, info.cell_height), (8, 20));
    }

    /// The LIVE cell of the default face at the default Windows size (16 px):
    /// what a window actually draws, measured through the renderer the GUI
    /// builds rather than the `show-face` probe above.
    #[test]
    fn the_default_face_draws_its_measured_cell_at_16_px() {
        if !ACTIVE {
            return;
        }
        let r = Renderer::from_system(16.0, Theme::default()).expect("a default face");
        let expected = if cfg!(windows) { (9, 21) } else { (10, 21) };
        assert_eq!(r.cell_size(), expected);
    }

    /// THE DEFAULT CELL HOLDS ITS LETTERS (`letter_descent_floor`). Measured on
    /// the rasters the cell draws, at every size from 8 to 40 px and in all
    /// four styles: every Latin letter that hangs a mark or a stroke below the
    /// baseline ends INSIDE the cell, so the blit's row-band clip cuts nothing
    /// visible off it. And at the default 16 px a row of descenders keeps at
    /// least one clear pixel row above the capital accents of the row under it.
    ///
    /// VISIBLE ink is a row whose strongest pixel reaches 1/8 coverage: the
    /// hinter leaves single-digit fringes (a bold `ķ` inks 5/255 one row
    /// past its 14 px cell) that no eye reads as part of the comma.
    ///
    /// Before the floor, on Windows (Cascadia Mono PL on its vendor line, a
    /// 9x19 cell at 16 px): a regular `ș` ended one row and a bold one two
    /// rows past the cell (coverage 55 and 54/255), and the caron of an `Š`
    /// inked the row a `j` above it ended on.
    #[test]
    fn the_default_cell_holds_its_letters_and_keeps_a_clear_row_between_lines() {
        use crate::{GlyphImage, StyleBits};
        if !ACTIVE {
            return;
        }
        const STYLES: [StyleBits; 4] = [
            StyleBits::REGULAR,
            StyleBits::BOLD,
            StyleBits::ITALIC,
            StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
        ];
        // The VISIBLE ink rows `[top, bottom)` of `ch` in its cell.
        let ink = |r: &mut Renderer, ch: char, style: StyleBits| -> (i32, i32) {
            let baseline = r.baseline();
            let key = r.glyph_key_styled(ch, style);
            let GlyphImage::Mono {
                width,
                height,
                ymin,
                bytes,
                ..
            } = r.glyph_image(key)
            else {
                panic!("{ch} {style:?}: a letter rasterizes as coverage");
            };
            let (w, h) = (*width, *height);
            let seen = |y: &usize| bytes[y * w..(y + 1) * w].iter().any(|&a| a >= 32);
            let first = (0..h).find(seen).expect("a letter has ink");
            let last = (0..h).rev().find(seen).expect("a letter has ink");
            let top = baseline - (ymin + i32::try_from(h).unwrap());
            (
                top + i32::try_from(first).unwrap(),
                top + i32::try_from(last).unwrap() + 1,
            )
        };
        let hanging = "șțȘȚķļņŗģĢĶĻŅŖşţŞŢçÇąęįųgjpqy";
        for px in 8..=40u8 {
            let mut r = Renderer::from_system(f32::from(px), Theme::default()).unwrap();
            let cell_h = i32::try_from(r.cell_size().1).unwrap();
            for ch in hanging.chars() {
                for style in STYLES {
                    let (_, bottom) = ink(&mut r, ch, style);
                    assert!(
                        bottom <= cell_h,
                        "{px} px {ch} {style:?}: ink ends at row {bottom} of a {cell_h}-row cell"
                    );
                }
            }
        }
        // The default 16 px, on the SEALED generation a window draws with.
        let mut r = Renderer::from_system(16.0, Theme::default()).unwrap();
        r.seal_admitted_font_sources();
        let cell_h = i32::try_from(r.cell_size().1).unwrap();
        let (mut lowest, mut highest) = (i32::MIN, i32::MAX);
        for style in STYLES {
            for ch in "gjpqy".chars() {
                lowest = lowest.max(ink(&mut r, ch, style).1);
            }
            for ch in "ŠŽČŘŇĎŤÂÊĚÉÈÅÑÄÖ".chars() {
                highest = highest.min(ink(&mut r, ch, style).0);
            }
        }
        // A descender of one row ends at `lowest`; the accent of the row under
        // it starts at `cell_h + highest`.
        assert!(
            cell_h + highest - lowest >= 1,
            "16 px: descenders end at row {lowest} of {cell_h}, the next row's accents start \
             at its row {highest}: no clear row between them"
        );
    }

    /// BOLD / ITALIC RESOLUTION: the bundled primary's three styled slots are
    /// filled from its REAL compiled-in faces — never a sibling FILE lookup,
    /// never the synthetic dilation/shear.
    #[test]
    fn the_bundled_primary_resolves_real_bold_italic_and_bold_italic_faces() {
        let Some(r) = default_sealed() else { return };
        assert_eq!(r.debug_styled_face_indices(), [Some(0); 3]);
        let styles: Vec<(u16, bool)> = r
            .styled_faces
            .iter()
            .map(|slot| {
                let face = slot.as_ref().expect("slot filled");
                let parsed = ttf_parser::Face::parse(&face.bytes, 0).unwrap();
                (parsed.weight().to_number(), parsed.is_italic())
            })
            .collect();
        assert_eq!(styles, [(700, false), (400, true), (700, true)]);
        // And they are THIS build's primary family's own files (Cascadia Mono
        // PL on Windows: its real cursive italic, not a shear of the upright).
        for (slot, id) in
            r.styled_faces
                .iter()
                .zip([PRIMARY_BOLD_ID, PRIMARY_ITALIC_ID, PRIMARY_BOLD_ITALIC_ID])
        {
            let face = slot.as_ref().expect("slot filled");
            assert_eq!(&face.bytes[..], face_for_id(id).unwrap(), "{id}");
        }
    }

    /// JetBrains Mono chosen BY NAME on Windows, where it is not the default,
    /// still gets its own real styled faces.
    #[test]
    fn a_named_jetbrains_mono_on_windows_resolves_its_own_styled_faces() {
        if !CASCADIA_ACTIVE || crate::resolve_font_family(JETBRAINS_FAMILY).is_some() {
            return;
        }
        let mut r =
            Renderer::from_system_with_family(Some(JETBRAINS_FAMILY), 18.0, Theme::default())
                .unwrap();
        r.seal_admitted_font_sources();
        assert_eq!(r.primary_source_path(), Some(JETBRAINS_ID));
        assert_eq!(r.debug_styled_face_indices(), [Some(0); 3]);
        for (slot, id) in r.styled_faces.iter().zip([
            JETBRAINS_BOLD_ID,
            JETBRAINS_ITALIC_ID,
            JETBRAINS_BOLD_ITALIC_ID,
        ]) {
            let face = slot.as_ref().expect("slot filled");
            assert_eq!(&face.bytes[..], face_for_id(id).unwrap(), "{id}");
        }
    }

    /// FALLBACK ORDER over the owner's measured test set, on the SEALED default
    /// generation a window publishes. Each expectation is the first face in the
    /// declared order whose cmap carries the code point — on Linux JetBrains
    /// Mono → DejaVu Sans Mono → Noto Sans Symbols 2 → Noto Sans Math → …, on
    /// Windows Cascadia Mono PL → DejaVu Sans Mono → the same two Noto faces →
    /// … — and NOTHING in the set may end as `.notdef`.
    #[test]
    fn the_test_set_resolves_in_the_declared_order_and_never_to_notdef() {
        let Some(mut r) = default_sealed() else {
            return;
        };
        let primary = format!("primary:{PRIMARY_ID}");
        let dejavu = format!("fallback:{DEJAVU_ID}");
        let sym2 = format!("symbol:{NOTO_SYMBOLS2_ID}");
        let math = format!("symbol:{NOTO_MATH_ID}");
        // WINDOWS, measured from the cmaps (2026-09-30): Cascadia Mono PL
        // carries ◐ but none of ✗ ⚠ ⇒ ⇐ ⌘ ➜ ∴ ✔ ✘ ↵ ★ ⚙ ✻, all of which
        // DejaVu Sans Mono — the broad tier's one bundled face — carries.
        #[cfg(windows)]
        let platform: &[(char, &str)] = &[
            ('▶', &primary),
            ('◀', &primary),
            ('●', &primary),
            ('✓', &primary),
            ('❯', &primary),
            ('→', &primary),
            ('≡', &primary),
            ('∞', &primary),
            ('≈', &primary),
            ('◐', &primary),
            ('✗', &dejavu),
            ('⚠', &dejavu),
            ('⇒', &dejavu),
            ('⇐', &dejavu),
            ('⌘', &dejavu),
            ('➜', &dejavu),
            ('∴', &dejavu),
            ('✔', &dejavu),
            ('✘', &dejavu),
            ('↵', &dejavu),
            ('★', &dejavu),
            ('⚙', &dejavu),
            ('✻', &dejavu),
        ];
        #[cfg(not(windows))]
        let platform: &[(char, &str)] = &[
            // JetBrains Mono's own glyphs.
            ('▶', &primary),
            ('◀', &primary),
            ('●', &primary),
            ('✓', &primary),
            ('✗', &primary),
            ('❯', &primary),
            ('→', &primary),
            ('⚠', &primary),
            ('≡', &primary),
            ('∞', &primary),
            ('≈', &primary),
            // What JetBrains Mono lacks and DejaVu Sans Mono carries.
            ('◐', &dejavu),
            ('✔', &dejavu),
            ('✘', &dejavu),
            ('↵', &dejavu),
            ('★', &dejavu),
            ('⚙', &dejavu),
        ];
        let shared: &[(char, &str)] = &[
            // Media / technical: Noto Sans Symbols 2 — a REAL glyph now beats
            // the synthetic tier that drew these before the stack shipped.
            ('⏴', &sym2),
            ('⏵', &sym2),
            ('⏶', &sym2),
            ('⏷', &sym2),
            ('⏺', &sym2),
            ('⏸', &sym2),
            // Operators only the math face carries.
            ('⊨', &math),
            ('⨁', &math),
            ('⩽', &math),
            ('⟹', &math),
            ('ℒ', &math),
        ];
        let mut wrong = Vec::new();
        for &(ch, want) in platform.iter().chain(shared) {
            let got = resolved_face(&mut r, ch);
            if got != want {
                wrong.push(format!("{ch} U+{:04X}: want {want}, got {got}", ch as u32));
            }
        }
        assert!(
            wrong.is_empty(),
            "chain order violated:\n{}",
            wrong.join("\n")
        );
        // The rest of the set: braille is procedural, ⏩ is emoji-default (the
        // colour face's, or the synthesis without one — never a bundled text
        // face), and whatever carries ⎿ it must not be tofu.
        for ch in ['⎿', '⏩', '⠀', '⣿'] {
            let got = resolved_face(&mut r, ch);
            assert_ne!(got, "NOTDEF", "{ch} U+{:04X} is tofu", ch as u32);
            assert!(
                !got.starts_with("fallback:bundled") && !got.starts_with("symbol:bundled"),
                "{ch}: an incidental bundled pick ({got})"
            );
        }
    }

    /// THE WINDOWS PROMPT LINE: each Powerline / Nerd Font code point a shell
    /// prompt draws resolves to the face that owns it on the sealed default
    /// chain — the separators U+E0B0..E0BF to the procedural tier, and
    /// everything else to the embedded Symbols Nerd Font: the branch,
    /// line-number, lock and column-number glyphs U+E0A0..E0A3 and the
    /// Powerline-extra flames, pixels and trapezoids U+E0C0..E0D4, which
    /// Cascadia Mono PL maps itself but yields (`yields_private_use`; its own
    /// glyphs paint into the row above), the common Nerd Font prompt set, and
    /// the points Segoe UI Symbol and Arial also map (U+E000, U+E200, U+F013,
    /// U+F301) to an unrelated glyph (`FOREIGN_PRIVATE_USE_FACES` in the crate
    /// root).
    #[test]
    #[cfg(all(windows, feature = "embedded-symbols"))]
    fn the_windows_prompt_line_resolves_each_icon_to_the_face_that_owns_it() {
        let Some(mut r) = default_sealed() else {
            return;
        };
        for ch in [
            '\u{E0B0}', '\u{E0B1}', '\u{E0B2}', '\u{E0B3}', '\u{E0B4}', '\u{E0B6}',
        ] {
            assert_eq!(
                r.glyph_key(ch).source,
                FaceId::Procedural,
                "U+{:04X}",
                ch as u32
            );
        }
        for ch in [
            '\u{E0A0}', '\u{E0A1}', '\u{E0A2}', '\u{E0A3}', '\u{E0C0}', '\u{E0C4}', '\u{E0D2}',
            '\u{F418}', '\u{F07C}', '\u{E718}', '\u{E7A8}', '\u{F0E7}', '\u{F120}', '\u{F308}',
            '\u{E5FF}', '\u{F113}', '\u{E000}', '\u{E200}', '\u{F013}', '\u{F301}',
        ] {
            assert_embedded_nerd_lane(&mut r, ch);
        }
    }

    /// `ch` resolves to the runtime tier's I/O-free EMBEDDED lane — the
    /// compiled-in Symbols Nerd Font — in every style: the regular key names
    /// [`FaceId::RuntimeFallback`], the decision the rasterizer recovers it
    /// from is the embedded face, and the bold / italic / bold-italic keys are
    /// that same face's (never a styled sibling of the primary).
    #[cfg(all(windows, feature = "embedded-symbols"))]
    fn assert_embedded_nerd_lane(r: &mut Renderer, ch: char) {
        use crate::StyleBits;
        assert_eq!(
            r.glyph_key(ch).source,
            FaceId::RuntimeFallback,
            "U+{:04X} ({})",
            ch as u32,
            resolved_face(r, ch)
        );
        let slot = r
            .runtime_fallback
            .embedded_decisions
            .get(&ch)
            .copied()
            .flatten()
            .expect("a sealed generation records the embedded lane");
        assert_eq!(
            r.runtime_fallback.faces[slot].path,
            crate::RuntimeFallback::EMBEDDED_SYMBOLS_PATH,
            "U+{:04X}",
            ch as u32
        );
        for style in [
            StyleBits::BOLD,
            StyleBits::ITALIC,
            StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
        ] {
            assert_eq!(
                r.glyph_key_styled(ch, style).source,
                FaceId::RuntimeFallback,
                "U+{:04X} {style:?}",
                ch as u32
            );
        }
    }

    /// The bundled faces whose private-use glyphs YIELD are exactly the four
    /// Cascadia Mono PL faces, recognised by their bytes wherever those live
    /// (a renderer may hold a copy); every other bundled face keeps its own.
    /// Off Windows no Cascadia bytes exist, so nothing yields there and the
    /// Linux default (JetBrains Mono) is untouched.
    #[test]
    fn exactly_the_cascadia_mono_pl_faces_yield_their_private_use() {
        let mut yielding = Vec::new();
        for &id in ALL_IDS {
            let Some(bytes) = face_for_id(id) else {
                continue;
            };
            let yields = yields_private_use(bytes);
            // A second allocation, so the contents decide, not the pointer.
            let copy = bytes.to_vec();
            assert_eq!(
                yields,
                yields_private_use(&copy),
                "{id}: a copy of the bytes is the same face"
            );
            if yields {
                yielding.push(id);
            }
        }
        let expected: &[&str] = if CASCADIA_ACTIVE {
            &PRIVATE_USE_YIELDING_IDS
        } else {
            &[]
        };
        assert_eq!(yielding, expected);
        #[cfg(feature = "embedded-font")]
        assert!(!yields_private_use(crate::embedded_font()));
        #[cfg(feature = "embedded-symbols")]
        assert!(!yields_private_use(crate::embedded_symbols_font()));
        // A face of the same length that is not the face does not yield.
        if let Some(bytes) = face_for_id(CASCADIA_ID) {
            let mut other = bytes.to_vec();
            let last = other.len() - 1;
            other[last] ^= 0xFF;
            assert!(!yields_private_use(&other));
        }
    }

    /// The Cascadia Mono PL private-use set the yield moves, measured: each of
    /// the four faces maps the same 38 points (U+E0A0..E0A3, U+E0B0..E0C8,
    /// U+E0CA, U+E0CC..E0D2, U+E0D4), the embedded Nerd Font maps every one —
    /// so the yield costs no point its glyph — and on the sealed default chain
    /// every one of them that is not a procedural separator now draws from
    /// the Nerd Font, in every style.
    #[test]
    #[cfg(all(windows, feature = "embedded-symbols"))]
    fn every_private_use_point_the_default_face_maps_draws_from_the_nerd_font() {
        let Some(mut r) = default_sealed() else {
            return;
        };
        let mut expected: Vec<char> = ('\u{E0A0}'..='\u{E0A3}')
            .chain('\u{E0B0}'..='\u{E0C8}')
            .chain(['\u{E0CA}'])
            .chain('\u{E0CC}'..='\u{E0D2}')
            .chain(['\u{E0D4}'])
            .collect();
        expected.sort_unstable();
        for id in PRIVATE_USE_YIELDING_IDS {
            let face = ttf_parser::Face::parse(face_for_id(id).unwrap(), 0).unwrap();
            let mut mapped = Vec::new();
            for sub in face.tables().cmap.unwrap().subtables {
                if sub.is_unicode() {
                    sub.codepoints(|cp| {
                        if let Some(ch) = char::from_u32(cp)
                            && crate::font_chain::is_private_use(ch)
                            && face.glyph_index(ch).is_some_and(|g| g.0 != 0)
                        {
                            mapped.push(ch);
                        }
                    });
                }
            }
            mapped.sort_unstable();
            mapped.dedup();
            assert_eq!(mapped, expected, "{id}: its private-use set moved");
        }
        let mut drawn = 0usize;
        for &ch in &expected {
            assert!(crate::embedded_symbols_map(ch), "U+{:04X}", ch as u32);
            if crate::procedural::covers(ch) {
                assert_eq!(r.glyph_key(ch).source, FaceId::Procedural);
                continue;
            }
            assert_embedded_nerd_lane(&mut r, ch);
            drawn += 1;
        }
        assert_eq!(drawn, 22, "U+E0A0..E0A3 and the 18 Powerline-extra points");
    }

    /// THE DEFECT, pinned on pixels at the default 16 px (9x21 cell, baseline
    /// row 15). Cascadia Mono PL's own trapezoid U+E0D2 was 22 px tall,
    /// rows -3..18: three rows painted into the row above, rows 19..20 left
    /// showing the next segment's colour. The yielded glyphs come from the
    /// embedded Nerd Font through the fallback fit, and every pixel of their
    /// raster lies in the cell box — GPU and CPU draw these same bytes at this
    /// same placement. The NEGATIVE CONTROL is the same default face with the
    /// runtime tier closed, where nothing yields: its own U+E0D2 still pokes
    /// out of the cell, so the test sees the defect it was written for.
    #[test]
    #[cfg(all(windows, feature = "embedded-symbols"))]
    fn the_yielded_powerline_glyphs_draw_inside_their_cell_at_16_px() {
        if !CASCADIA_ACTIVE {
            return;
        }
        // `(left, top, right, bottom)` of the raster box and of its ink
        // (coverage > 0), cell-relative, half-open.
        fn boxes(r: &mut Renderer, ch: char) -> ([i32; 4], [i32; 4]) {
            let baseline = r.baseline();
            let key = r.glyph_key(ch);
            let crate::GlyphImage::Mono {
                width,
                height,
                xmin,
                ymin,
                bytes,
                ..
            } = r.glyph_image(key)
            else {
                panic!("U+{:04X}: a coverage raster", ch as u32);
            };
            let (w, h) = (*width, *height);
            let (xmin, top) = (*xmin, baseline - (*ymin + i32::try_from(h).unwrap()));
            let rows: Vec<usize> = (0..h)
                .filter(|y| bytes[y * w..(y + 1) * w].iter().any(|&a| a > 0))
                .collect();
            let cols: Vec<usize> = (0..w)
                .filter(|x| (0..h).any(|y| bytes[y * w + x] > 0))
                .collect();
            let i = |v: usize| i32::try_from(v).unwrap();
            (
                [xmin, top, xmin + i(w), top + i(h)],
                [
                    xmin + i(cols[0]),
                    top + i(rows[0]),
                    xmin + i(*cols.last().unwrap()) + 1,
                    top + i(*rows.last().unwrap()) + 1,
                ],
            )
        }
        let mut r = Renderer::from_system(16.0, Theme::default()).expect("a default face");
        r.seal_admitted_font_sources();
        assert_eq!(r.cell_size(), (9, 21));
        assert_eq!(r.baseline(), 15);
        // The ink boxes the Nerd lane gives, MEASURED and pinned. The branch and
        // the lock keep their full height (17 and 15 rows). The flame and the
        // trapezoid do NOT reach the cell's bottom, and are not meant to by
        // this lane: the fit sizes a symbol by its ink about its ADVANCE
        // CENTRE (`fallback_ink_fit_scale`). Both have a 1-em advance; the
        // flame's ink fills it and the trapezoid's sits to one side of its
        // centre, so each shrinks to the 9-px cell by its WIDTH, to 9 rows
        // centred in the band — as they drew under JetBrains Mono, which
        // never carried them. Inside the cell is the whole claim here.
        let expected = [
            ('\u{E0A0}', [0, 2, 8, 19]),
            ('\u{E0A2}', [0, 3, 9, 18]),
            ('\u{E0C0}', [0, 6, 8, 15]),
            ('\u{E0D2}', [0, 6, 5, 15]),
        ];
        for (ch, want) in expected {
            assert_eq!(r.glyph_key(ch).source, FaceId::RuntimeFallback);
            let (raster, ink) = boxes(&mut r, ch);
            eprintln!("U+{:04X}: raster {raster:?} ink {ink:?}", ch as u32);
            assert!(
                raster[0] >= 0 && raster[1] >= 0 && raster[2] <= 9 && raster[3] <= 21,
                "U+{:04X}: raster {raster:?} leaves the 9x21 cell",
                ch as u32
            );
            assert_eq!(ink, want, "U+{:04X}: the measured ink box moved", ch as u32);
        }
        // CONTROL: the runtime tier closed, so the primary keeps its own glyph —
        // the defect as measured on the renders: rows -3..18, and a column
        // either side of the 9-px cell too.
        let mut closed = Renderer::from_system(16.0, Theme::default()).expect("a default face");
        closed.set_runtime_font_discovery(false);
        closed.seal_admitted_font_sources();
        assert_eq!(closed.glyph_key('\u{E0D2}').source, FaceId::Primary);
        let (_, ink) = boxes(&mut closed, '\u{E0D2}');
        eprintln!("control U+E0D2 (Cascadia Mono PL's own): ink {ink:?}");
        assert_eq!(
            ink,
            [-1, -3, 10, 19],
            "the face's own U+E0D2 moved: this control may control nothing"
        );
    }

    /// A face that is NOT the bundled Cascadia Mono PL keeps every private-use
    /// glyph it maps, whoever configures it — here JetBrains Mono, which maps
    /// the branch U+E0A0 itself, pushed as bytes the way a host pushes a
    /// user's font. And the yield follows the FACE, not the renderer: swapping
    /// to it (`set_primary_font`) stops the yield, swapping back to a copy of
    /// the Cascadia bytes restores it. A bundled Cascadia face injected as the
    /// BOLD or a STYLED face of such a primary yields too: the bold / italic
    /// cell keeps the primary's routing instead of drawing Cascadia's glyph.
    #[test]
    #[cfg(all(windows, feature = "embedded-symbols"))]
    fn a_face_that_is_not_the_bundled_cascadia_keeps_its_own_private_use_glyphs() {
        use crate::{GlyphClass, StyleBits};
        if !CASCADIA_ACTIVE {
            return;
        }
        let branch = '\u{E0A0}';
        let jbm = face_for_id(JETBRAINS_ID).unwrap();
        let cascadia = face_for_id(CASCADIA_ID).unwrap().to_vec();
        let mut r = Renderer::from_bytes(jbm, 16.0, Theme::default()).unwrap();
        r.seal_admitted_font_sources();
        let key = r.glyph_key(branch);
        assert_eq!(
            (key.source, key.glyph_class),
            (FaceId::Primary, GlyphClass::MonoGid),
            "JetBrains Mono maps U+E0A0 and draws it"
        );
        // Cascadia Mono PL's own Bold and Italic, injected beside it.
        r.set_bold_font(face_for_id(CASCADIA_BOLD_ID).unwrap())
            .unwrap();
        r.set_styled_font_bytes(1, face_for_id(CASCADIA_ITALIC_ID).unwrap())
            .unwrap();
        let bold = r.glyph_key_styled(branch, StyleBits::BOLD);
        assert_eq!(
            bold.source,
            FaceId::Primary,
            "a bold branch is JetBrains Mono's, not the injected Cascadia bold"
        );
        assert_eq!(
            r.glyph_key_styled('a', StyleBits::BOLD).source,
            FaceId::BoldPrimary,
            "the injected bold face still draws its letters"
        );
        assert_eq!(r.styled_face_slot(branch, StyleBits::ITALIC), None);
        assert_eq!(
            r.styled_face_slot('a', StyleBits::ITALIC),
            Some((1, StyleBits::REGULAR))
        );
        // The face, not the renderer: Cascadia's bytes (a copy) yield.
        r.set_primary_font(&cascadia).unwrap();
        assert_eq!(r.glyph_key(branch).source, FaceId::RuntimeFallback);
        r.set_primary_font(jbm).unwrap();
        assert_eq!(r.glyph_key(branch).source, FaceId::Primary);
    }

    /// THE RUNTIME SWITCH: with the runtime tier closed (the chrome and
    /// Settings renderers) nothing could draw a yielded point, so the default
    /// face keeps drawing its own — never `.notdef`. Opening the tier again
    /// re-resolves every memo, styled ones included, onto the runtime tier
    /// (`set_runtime_font_discovery` drops the styled memo with the rest).
    #[test]
    #[cfg(all(windows, feature = "embedded-symbols"))]
    fn the_yield_waits_for_the_runtime_tier_and_follows_it_open() {
        use crate::{GlyphClass, StyleBits};
        if !CASCADIA_ACTIVE {
            return;
        }
        let branch = '\u{E0A0}';
        let mut r = Renderer::from_system(16.0, Theme::default()).unwrap();
        r.set_runtime_font_discovery(false);
        r.seal_admitted_font_sources();
        let key = r.glyph_key(branch);
        assert_eq!(
            (key.source, key.glyph_class),
            (FaceId::Primary, GlyphClass::MonoGid)
        );
        assert_eq!(
            r.glyph_key_styled(branch, StyleBits::BOLD).source,
            FaceId::Primary
        );
        assert!(r.set_runtime_font_discovery(true), "the chain widened");
        assert_eq!(r.glyph_key(branch).source, FaceId::RuntimeFallback);
        assert_eq!(
            r.glyph_key_styled(branch, StyleBits::BOLD).source,
            FaceId::RuntimeFallback,
            "the styled memo of the closed chain survived the widening"
        );
    }

    /// A YIELDED POINT BREAKS THE SHAPING RUN. Cascadia Mono PL advertises
    /// `calt`, so its rows are shaped, and its GSUB substitutes most of its
    /// private-use glyphs inside a run (measured 2026-10-01 under the shaper's
    /// features: each of the 38 points doubled between letters, 68 of the 76
    /// private-use cells came back changed, in each of the four faces). The
    /// CONTROL shapes `a`, branch, lock, `b` on the face itself and sees the
    /// branch's column changed. Had that cell joined a run, the planner would
    /// have drawn the changed gid from the face (`ColumnGlyph::Ligated`), past
    /// the yield: Cascadia's own branch again. The planner breaks the run at a
    /// yielded point instead, so those columns stay per-cell and draw from the
    /// Nerd Font: on the default face, and in a BOLD run shaped on an injected
    /// Cascadia bold beside a JetBrains Mono primary, whose own glyphs do not
    /// yield (the run's face follows its style).
    #[test]
    #[cfg(all(windows, feature = "embedded-symbols"))]
    fn a_yielded_point_breaks_the_shaping_run() {
        use crate::ColumnGlyph;
        use crate::ligature_shaping::{RowShaper, ShapedRun, build_feature_list};
        use aterm_core::terminal::Terminal;
        if !CASCADIA_ACTIVE {
            return;
        }
        let text = "a\u{E0A0}\u{E0A2}b";
        let cols: u16 = 8;
        // CONTROL: on the face itself, a run holding the branch and the lock
        // comes back with both columns changed.
        let features = build_feature_list(&[], true);
        let face = rustybuzz::Face::from_slice(face_for_id(CASCADIA_ID).unwrap(), 0).unwrap();
        let chars: Vec<char> = text.chars().collect();
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        let shaped = RowShaper::new(face, &features).shape(text, &chars, false, &mut buffer);
        let Some(ShapedRun::PerColumn(gids)) = shaped else {
            panic!("the control: Cascadia Mono PL changes nothing in {text:?} ({shaped:?})");
        };
        assert!(
            gids[1].is_some(),
            "the control: the branch is not substituted in a run ({gids:?})"
        );

        let plan_of = |r: &mut Renderer, sgr: &str| {
            let mut term = Terminal::new(1, cols);
            term.process(format!("\x1b[?25l{sgr}{text}").as_bytes());
            let input = term.cell_frame(1, usize::from(cols));
            let mut plan = Vec::new();
            r.row_glyph_plan(&input, 0, &[], &mut plan);
            plan
        };
        let per_cell = [ColumnGlyph::PerCell, ColumnGlyph::PerCell];
        let mut r = Renderer::from_system(16.0, Theme::default()).unwrap();
        r.seal_admitted_font_sources();
        assert!(
            r.has_ligature_features,
            "the default face's rows are shaped"
        );
        let plan = plan_of(&mut r, "");
        assert_eq!(plan[1..3], per_cell, "the default face: {plan:?}");
        for ch in ['\u{E0A0}', '\u{E0A2}'] {
            assert_eq!(r.glyph_key(ch).source, FaceId::RuntimeFallback);
        }

        let jbm = face_for_id(JETBRAINS_ID).unwrap();
        let mut r = Renderer::from_bytes(jbm, 16.0, Theme::default()).unwrap();
        r.seal_admitted_font_sources();
        r.set_bold_font(face_for_id(CASCADIA_BOLD_ID).unwrap())
            .unwrap();
        assert!(r.has_ligature_features, "JetBrains Mono's rows are shaped");
        let plan = plan_of(&mut r, "\x1b[1m");
        let shaped_on = r.run_face_pick(crate::StyleBits::BOLD);
        assert!(
            matches!(shaped_on, crate::FacePick::InjectedBold { .. }),
            "a bold run shapes on the injected Cascadia bold, not {shaped_on:?}"
        );
        assert_eq!(
            plan[1..3],
            per_cell,
            "a bold run on the injected Cascadia bold: {plan:?}"
        );
    }

    /// The bundled faces lead their tiers but never TAKE what the host's own
    /// faces own: no emoji-default and no private-use point is ever picked
    /// from a bundled chain face (the colour face and the Symbols Nerd Font
    /// own those).
    #[test]
    fn a_bundled_chain_face_never_takes_an_emoji_default_or_private_use_point() {
        let Some(mut r) = default_sealed() else {
            return;
        };
        let mut checked = 0usize;
        // Every bundled CHAIN face this build places.
        for &id in broad_fallback_ids().iter().chain(symbol_fallback_ids()) {
            let face = ttf_parser::Face::parse(face_for_id(id).unwrap(), 0).unwrap();
            let mut incidental = Vec::new();
            for sub in face.tables().cmap.unwrap().subtables {
                if sub.is_unicode() {
                    sub.codepoints(|cp| {
                        if let Some(ch) = char::from_u32(cp)
                            && (aterm_grapheme::is_emoji_presentation(ch)
                                || crate::font_chain::is_private_use(ch))
                        {
                            incidental.push(ch);
                        }
                    });
                }
            }
            for ch in incidental {
                // The PRIMARY answering is not a chain pick: a primary face's
                // own glyph always wins, bundled or installed, as it always has.
                let got = resolved_face(&mut r, ch);
                assert!(
                    got.starts_with("primary:") || !got.contains(SCHEME),
                    "U+{:04X} taken by {got}",
                    ch as u32
                );
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "no bundled face maps an incidental point; nothing proven"
        );
    }

    /// THE STARTUP / SETTINGS READ PATH: every consumer of a RESOLVED identity
    /// (`resolve_config_font`'s answer) reads it through `read_resolved_font`,
    /// which serves a bundled identity from compiled-in bytes. The GUI's
    /// startup styled-face admission (`font_family_bold = "JetBrains Mono
    /// Bold"`) and the Settings preview must never hand the identity to the
    /// filesystem, where it fails with ENOENT.
    #[test]
    fn a_resolved_bundled_identity_reads_as_its_compiled_in_bytes() {
        if !ACTIVE {
            return;
        }
        let mut cases = Vec::new();
        if crate::resolve_font_family(JETBRAINS_FAMILY).is_none() {
            cases.extend([
                ("JetBrains Mono", JETBRAINS_ID),
                ("JetBrains Mono Bold", JETBRAINS_BOLD_ID),
                ("JetBrains Mono Italic", JETBRAINS_ITALIC_ID),
                ("JetBrains Mono Bold Italic", JETBRAINS_BOLD_ITALIC_ID),
            ]);
        }
        if CASCADIA_ACTIVE && crate::resolve_font_family(CASCADIA_FAMILY).is_none() {
            cases.extend([
                ("Cascadia Mono PL", CASCADIA_ID),
                ("Cascadia Mono PL Bold", CASCADIA_BOLD_ID),
                ("Cascadia Mono PL Italic", CASCADIA_ITALIC_ID),
                ("Cascadia Mono PL Bold Italic", CASCADIA_BOLD_ITALIC_ID),
            ]);
        }
        for (name, id) in cases {
            let resolved = crate::resolve_config_font(name).expect(name);
            assert_eq!(resolved, id);
            let bytes = crate::read_resolved_font(&resolved).expect(name);
            assert_eq!(&bytes[..], face_for_id(id).unwrap(), "{name}");
            assert!(
                crate::font_file::read_font_file(std::path::Path::new(&resolved)).is_err(),
                "the identity is not a file — reading it as one is the bug"
            );
        }
        // A real file still reads as a file.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/DejaVuSansMono.ttf");
        assert_eq!(
            &crate::read_resolved_font(path).unwrap()[..],
            &std::fs::read(path).unwrap()[..]
        );
    }

    /// THE OWNER'S ORDER puts the embedded Symbols Nerd Font AHEAD of Noto Sans
    /// Symbols 2 / Noto Sans Math. Its private-use icons were never contested;
    /// the six non-private-use points both carry (measured with fontTools:
    /// `⏻ ⏼ ⏽ ⏾ ☰ ⭘`) must not be drawn by a bundled Noto face.
    #[test]
    #[cfg(feature = "embedded-symbols")]
    fn the_nerd_font_precedes_the_bundled_noto_faces_where_both_draw() {
        // The measured set, pinned: a Nerd Font update that adds a text point
        // announces itself here.
        let owned: Vec<char> = (0u32..0x11_0000)
            .filter_map(char::from_u32)
            .filter(|&c| crate::embedded_symbols_own_text_point(c))
            .collect();
        assert_eq!(
            owned.iter().collect::<String>(),
            "⏻⏼⏽⏾☰♥⚡❬❭❮❯❰❱⭘",
            "the embedded Nerd Font's non-private-use coverage moved"
        );
        let Some(mut r) = default_sealed() else {
            return;
        };
        let contested = ['⏻', '⏼', '⏽', '⏾', '☰', '⭘'];
        for ch in contested {
            let noto_has = [NOTO_SYMBOLS2_ID, NOTO_MATH_ID].iter().any(|id| {
                ttf_parser::Face::parse(face_for_id(id).unwrap(), 0)
                    .unwrap()
                    .glyph_index(ch)
                    .is_some_and(|g| g.0 != 0)
            });
            assert!(noto_has, "{ch}: not contested — the test proves nothing");
            let got = resolved_face(&mut r, ch);
            assert!(
                !got.starts_with("symbol:bundled:Noto"),
                "{ch} U+{:04X}: a bundled Noto face took a Nerd Font point ({got})",
                ch as u32
            );
            assert_ne!(got, "NOTDEF", "{ch} is tofu");
        }
        // CONTROL: an uncontested point still comes from Noto Sans Symbols 2.
        assert_eq!(
            resolved_face(&mut r, '⏵'),
            format!("symbol:{NOTO_SYMBOLS2_ID}")
        );
    }
}
