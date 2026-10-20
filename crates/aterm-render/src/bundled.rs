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
//! # Windows: Iosevka Term Semibold
//!
//! On Windows the primary is **Iosevka Term Semibold**: Iosevka Term 34.9.0's
//! SemiBold, with its ExtraBold as the bold face and their two italics (SIL
//! OFL 1.1), subsets compiled in on Windows only ([`IOSEVKA_SEMIBOLD_ACTIVE`])
//! so a Linux or macOS binary does not carry their ~2.3 MB. The GUI draws it
//! at 17 px with a 1.1 line height on Windows (its `FONT_PX` and the config's
//! line-height default), a 9x23 cell on baseline row 17.
//!
//! Chosen 2026-10-16 when the owner asked to "find a better font for Windows
//! for aterm and switch to that font. See if we can use that open source font
//! that we recently included" (Iosevka Term, below), by two judged render
//! farms drawn through aterm at the owner's 96 DPI:
//!
//! * Round 1 judged the bundled Iosevka Term (Regular) at 16 to 18 px against
//!   Cascadia Mono PL at 16 px, and it lost on three counts: its Regular
//!   weight is too light there (ink per cell 18.9 at 17 px against Cascadia's
//!   26.85), on a 1.0 line its accented capitals' marks touched the
//!   descenders of the row above (42 of their pixels at 17 px), and its
//!   `calt` ligatures (`=>` drawn as `⇒`, `!=` as `≠`) were a deal-breaker.
//! * Round 2 answered each: heavier cuts of the same design, a taller line,
//!   no ligatures. SemiBold at 17 px on a 1.1 line (the render named J2-lh11:
//!   cell 9x23, ink per cell 24.12, 49.1% of ink pixels solid and 23% faint,
//!   NO accent pixel touching a descender) scored a mean of 7.5 with no
//!   deal-breaker from any judge, and 8.5 against Cascadia's 6.5 through the
//!   long-time-macOS-user lens, the owner's own profile.
//!
//! What ships answers the ligature verdict in the bytes: the subsets drop
//! `calt` (`publish/DECISIONS.md` has the command), so `=> != === -> <= >=`
//! stay one glyph a column whatever `ligatures` says, while `ccmp`, `locl`,
//! the fraction features and mark positioning stay. Iosevka's own design is
//! otherwise untouched: the same 5,015 code points as the Regular set below.
//!
//! What it gives up, on purpose. Its 23 px row is two pixels taller than
//! Cascadia Mono PL's 21 at 16 px, so a window of the same height holds about
//! 9% fewer rows (the same columns: both cells are 9 px wide). The face
//! carries most of what the chain used to supply: Claude Code's `⏺`,
//! `✗ ⚠ ⇒ ★`, the media and math arrows are Iosevka's own one-cell forms
//! now, narrower than the Noto and DejaVu glyphs Cascadia left them to. Not
//! the spinner and footer dingbats `✢`..`✽` and `⏴`..`⏷`: its own are
//! x-height marks the size of an ASCII `*`, so it leaves them to the chain
//! ([`yields_small_dingbats`]), which draws Cascadia's-era glyphs again. And
//! not its raised default `*`: it draws Iosevka's own hex-low asterisk, on the
//! operator line like Cascadia Mono's, so the spinner's `*` frame stays level
//! ([`lowered_asterisk`]).
//!
//! What it does NOT draw: the 38 characters it maps that the grid gives TWO
//! columns — the emoji-by-default `✅ ❌ ⭐ ⚡ ⏳ ⌚ ⬛ ⬜ …`, `〈 〉`, the trigrams
//! and the digrams — whose glyphs are one column wide, so drawn from the face a `✅`
//! stood at the left of its slot at half its width (a reviewer's live render,
//! 2026-10-16). It leaves them to the chain ([`yields_wide_characters`]),
//! which draws them fitted and centred across both columns from the faces
//! they came from under Cascadia — the emoji among them in colour, from
//! Segoe UI Emoji, since the symbol tier yields an emoji-presentation scalar
//! to the colour face (2026-10-17).
//!
//! Its private-use glyphs are its own, not yielded ([`yields_private_use`]):
//! of the Powerline points it maps (U+E0A0..E0A3, U+E0AE, U+E0AF and
//! U+E0B0..E0BF) the separators U+E0B0..E0BF are drawn by the procedural tier
//! first, and the branch, line-number, lock and column-number glyphs
//! U+E0A0..E0A3 stand inside the 9x23 cell (ink rows 3..19 and columns 0..9,
//! half-open; measured 2026-10-16), where Cascadia's painted three rows into
//! the row above. Every other Nerd Font icon still comes from the embedded
//! Symbols Nerd Font. Its fourteen TWO-CELL glyphs — the long arrows `⟵`..`⟿`,
//! `⟝ ⟞` and `⬳`, a full em wide where the grid gives each one column — are
//! fitted into that column ([`fits_two_cell_glyphs`]), so a `⟹` cannot bury
//! the `│` of a table beside it. `font_family = "Cascadia Mono PL"`,
//! `font_px = 16` and `line_height = 1.0` give back the 9x21 Cascadia cell
//! exactly.
//!
//! ```text
//! Iosevka Term Semibold (SemiBold / ExtraBold / their italics)  primary
//!  → DejaVu Sans Mono (embedded)                               broad tier, first
//!  → system per-script / CJK faces → Arial                     broad tier
//!  → Noto Sans Symbols 2 → Noto Sans Math (embedded)           symbol tier, first
//!  → Segoe UI Symbol                                           symbol tier
//!  → Segoe UI Emoji → Symbols Nerd Font (embedded, runtime tier)
//!  → procedural Synthetic → .notdef
//! ```
//!
//! # Cascadia Mono PL, by name on Windows
//!
//! **Cascadia Mono PL** 2407.24, the official STATIC release (SIL OFL 1.1;
//! Regular / Bold / Italic / BoldItalic), is compiled in on Windows only
//! ([`CASCADIA_ACTIVE`]) and selectable by name. It was the Windows default
//! from 2026-09-30 to 2026-10-16, chosen when the owner asked for "a nicer,
//! better looking font for Windows aterm": a render farm drew 22 faces
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
//! Two things it gave up as the default, both on purpose. It draws NO
//! programming ligatures: the Mono cut is Cascadia Code without them
//! (`=> != >= -> ===` stay as typed, measured on renders 2026-09-30);
//! `font_family = "JetBrains Mono"` brings them back. And its cell is 9x21 at
//! 16 px, not JetBrains Mono's 10x21, so a window of the same width holds more
//! columns.
//!
//! Its private-use glyphs YIELD to the Symbols Nerd Font
//! ([`yields_private_use`]), whatever slot it is in.
//!
//! JetBrains Mono stays compiled in on Windows too, selectable by name, but
//! neither it nor Cascadia Mono PL is a chain face there: the broad tier is
//! DejaVu Sans Mono alone, as on Linux ([`broad_fallback_ids`] says why).
//!
//! Its cell is built on the vendor's line with ONE correction: the descent
//! reaches the family's deepest Latin letter, and a pixel past it for the
//! hinter ([`letter_descent_floor`]), so a Romanian `ș` or a Latvian `ķ` keeps
//! its comma and, at 16 px, a row of `j` keeps a clear pixel row above the
//! caron of an `Š` on the next row.
//!
//! # Iosevka Term, by name, everywhere
//!
//! **Iosevka Term** 34.9.0 (SIL OFL 1.1; Regular / Bold / Italic /
//! BoldItalic; owner request 2026-10-14, "add https://typeof.net/Iosevka/ to
//! aterm and try it") is compiled in on EVERY platform the `embedded-font` feature is
//! on, macOS included ([`IOSEVKA_ACTIVE`]), and is nobody's default and no
//! chain face: `font_family = "Iosevka Term"` selects it, and an installed
//! Iosevka Term wins that name as JetBrains Mono's does. (The Windows default
//! is the same design in its SemiBold and ExtraBold cuts, a different family,
//! above.) The Term cut is the one for terminals: strictly monospaced, its
//! arrows and geometric shapes one cell wide. Its cell is narrow — a 0.5 em
//! advance on a 1.25 em line, 8x20 at 16 px where JetBrains Mono is 10x21 —
//! so a window of the same width holds a quarter more columns.
//!
//! What ships is a SUBSET of the official unhinted release (aterm hints with
//! the autohinter, or CoreText on macOS, so the font's own bytecode would be
//! dead weight): 5,015 code points — Latin, Greek, Cyrillic, Armenian,
//! punctuation, arrows, math, Misc Technical, box drawing, blocks, Braille,
//! legacy computing, Powerline — and HarfBuzz's default layout features
//! (ligatures, marks, fractions), not Iosevka's stylistic sets and character
//! variants: about 0.6 MB a face where the whole is 8.4 to 8.9 MB. The
//! Windows Semibold set is cut over the same code points, less `calt`.
//! `publish/DECISIONS.md` holds the source digests, the commands and the
//! measured sizes. Iosevka declares no Reserved Font Name, so the subsets keep
//! their names.
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
//! `bundled:Cascadia Mono PL Bold Italic`, `bundled:JetBrains Mono Bold`. The
//! Iosevka Term Semibold family spells its regular face's family for all four
//! slots (`bundled:Iosevka Term Semibold Bold` is the ExtraBold face, whose
//! own name ID 1 is `Iosevka Term Extrabold`).
//!
//! A user who TYPES the family name — `font_family = "JetBrains Mono"` — gets an
//! INSTALLED JetBrains Mono when one resolves (exactly what the name meant
//! before the stack existed, so the variable `JetBrainsMono[wght].ttf` keeps its
//! `font_weight` / `wght` axis) and the bundled face only when none does
//! ([`crate::resolve_family_or_bundled`]). The same holds for `Cascadia Mono
//! PL`, `Iosevka Term` and `Iosevka Term Semibold`. `Cascadia Mono` (no PL) is
//! a different family name, and it keeps naming the system's variable face.
//! Every consumer of a resolved identity reads its bytes through
//! [`crate::read_resolved_font`], never as a file.
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
pub const ACTIVE: bool = cfg!(all(feature = "embedded-font", not(target_os = "macos")));

/// Whether this build compiles in the Cascadia Mono PL faces: the stack is
/// active AND the target is Windows. Everywhere else the `CASCADIA_*`
/// identities below resolve to nothing.
pub const CASCADIA_ACTIVE: bool = cfg!(all(feature = "embedded-font", windows));

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
pub const IOSEVKA_ACTIVE: bool = cfg!(feature = "embedded-font");

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

/// Whether this build compiles in the Iosevka Term SEMIBOLD faces, the Windows
/// default: the stack is active AND the target is Windows, as for Cascadia
/// Mono PL. Everywhere else the `IOSEVKA_SEMIBOLD_*` identities below resolve
/// to nothing, and the opt-in Iosevka Term above is the only Iosevka there.
pub const IOSEVKA_SEMIBOLD_ACTIVE: bool = cfg!(all(feature = "embedded-font", windows));

/// Iosevka Term Semibold's family name, as a user types it: name ID 1 of its
/// SemiBold and SemiBold Italic faces. Its bold slot is Iosevka Term's
/// ExtraBold, whose own name ID 1 is "Iosevka Term Extrabold" (that spelling
/// resolves too, [`face_for_family`]). Compiled in on Windows only
/// ([`IOSEVKA_SEMIBOLD_ACTIVE`]).
pub const IOSEVKA_SEMIBOLD_FAMILY: &str = "Iosevka Term Semibold";
/// The bundled Iosevka Term Semibold's virtual identity (regular slot: the
/// SemiBold face).
pub const IOSEVKA_SEMIBOLD_ID: &str = "bundled:Iosevka Term Semibold";
/// The bundled Iosevka Term Semibold's BOLD slot: the ExtraBold face.
pub const IOSEVKA_SEMIBOLD_BOLD_ID: &str = "bundled:Iosevka Term Semibold Bold";
/// The bundled Iosevka Term Semibold's ITALIC slot: the SemiBold Italic face.
pub const IOSEVKA_SEMIBOLD_ITALIC_ID: &str = "bundled:Iosevka Term Semibold Italic";
/// The bundled Iosevka Term Semibold's BOLD-ITALIC slot: the ExtraBold Italic
/// face.
pub const IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID: &str = "bundled:Iosevka Term Semibold Bold Italic";

/// The bundled PRIMARY's family name, as a user types it: Iosevka Term
/// Semibold on Windows, JetBrains Mono on every other non-macOS target.
#[cfg(windows)]
pub const PRIMARY_FAMILY: &str = IOSEVKA_SEMIBOLD_FAMILY;
/// The bundled PRIMARY's family name, as a user types it.
#[cfg(not(windows))]
pub const PRIMARY_FAMILY: &str = JETBRAINS_FAMILY;
/// The bundled primary's virtual identity (regular face).
#[cfg(windows)]
pub const PRIMARY_ID: &str = IOSEVKA_SEMIBOLD_ID;
/// The bundled primary's virtual identity (regular face).
#[cfg(not(windows))]
pub const PRIMARY_ID: &str = JETBRAINS_ID;
/// The bundled primary's BOLD face identity.
#[cfg(windows)]
pub const PRIMARY_BOLD_ID: &str = IOSEVKA_SEMIBOLD_BOLD_ID;
/// The bundled primary's BOLD face identity.
#[cfg(not(windows))]
pub const PRIMARY_BOLD_ID: &str = JETBRAINS_BOLD_ID;
/// The bundled primary's ITALIC face identity.
#[cfg(windows)]
pub const PRIMARY_ITALIC_ID: &str = IOSEVKA_SEMIBOLD_ITALIC_ID;
/// The bundled primary's ITALIC face identity.
#[cfg(not(windows))]
pub const PRIMARY_ITALIC_ID: &str = JETBRAINS_ITALIC_ID;
/// The bundled primary's BOLD-ITALIC face identity.
#[cfg(windows)]
pub const PRIMARY_BOLD_ITALIC_ID: &str = IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID;
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
    IOSEVKA_SEMIBOLD_ID,
    IOSEVKA_SEMIBOLD_BOLD_ID,
    IOSEVKA_SEMIBOLD_ITALIC_ID,
    IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID,
    DEJAVU_ID,
    NOTO_SYMBOLS2_ID,
    NOTO_MATH_ID,
];

#[cfg(feature = "embedded-font")]
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
    // Windows only: the Windows primary, Iosevka Term Semibold (subsets of
    // the official SemiBold / ExtraBold faces and their italics, without
    // `calt`). A Linux or macOS binary never selects it, so it does not carry
    // the ~2.3 MB.
    #[cfg(windows)]
    pub(super) static IOSEVKA_SEMIBOLD: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-SemiBold.ttf");
    #[cfg(windows)]
    pub(super) static IOSEVKA_EXTRABOLD: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-ExtraBold.ttf");
    #[cfg(windows)]
    pub(super) static IOSEVKA_SEMIBOLD_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-SemiBoldItalic.ttf");
    #[cfg(windows)]
    pub(super) static IOSEVKA_EXTRABOLD_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-ExtraBoldItalic.ttf");
}

/// The bytes behind one virtual identity, or `None` for anything that is not a
/// bundled identity in THIS build (on macOS: the Iosevka Term faces alone).
#[cfg(feature = "embedded-font")]
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
        #[cfg(windows)]
        IOSEVKA_SEMIBOLD_ID => Some(bytes::IOSEVKA_SEMIBOLD),
        #[cfg(windows)]
        IOSEVKA_SEMIBOLD_BOLD_ID => Some(bytes::IOSEVKA_EXTRABOLD),
        #[cfg(windows)]
        IOSEVKA_SEMIBOLD_ITALIC_ID => Some(bytes::IOSEVKA_SEMIBOLD_ITALIC),
        #[cfg(windows)]
        IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID => Some(bytes::IOSEVKA_EXTRABOLD_ITALIC),
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
#[cfg(not(feature = "embedded-font"))]
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
/// Windows, the same four spellings of `Cascadia Mono PL` and of `Iosevka Term
/// Semibold`, plus `Iosevka Term Extrabold` (`Italic`), the name its bold
/// slots' own faces carry. The fallback-only faces (DejaVu, Noto) resolve by
/// identity only: a family name such as "DejaVu Sans Mono" keeps meaning the
/// INSTALLED face, as before. So does `Cascadia Mono`, which names the
/// system's variable face, not this cut.
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
        #[cfg(windows)]
        "iosevkatermsemibold" | "iosevkatermsemiboldregular" => IOSEVKA_SEMIBOLD_ID,
        #[cfg(windows)]
        "iosevkatermsemiboldbold" | "iosevkatermextrabold" => IOSEVKA_SEMIBOLD_BOLD_ID,
        #[cfg(windows)]
        "iosevkatermsemibolditalic" => IOSEVKA_SEMIBOLD_ITALIC_ID,
        #[cfg(windows)]
        "iosevkatermsemiboldbolditalic" | "iosevkatermextrabolditalic" => {
            IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID
        }
        _ => return None,
    };
    face_for_id(id).map(|bytes| (id, bytes))
}

/// The real `[bold, italic, bold-italic]` faces of a bundled primary, keyed by
/// the primary's identity — what `Renderer::ensure_styled_faces` fills its
/// slots from instead of deriving sibling FILE paths. Empty for any other
/// primary. Every bundled four-face family carries all three: the defaults,
/// and the families chosen by name.
#[must_use]
pub fn styled_siblings(primary: &str) -> [Option<&'static [u8]>; 3] {
    let family = match primary {
        JETBRAINS_ID => JETBRAINS_FACES,
        #[cfg(windows)]
        CASCADIA_ID => CASCADIA_FACES,
        #[cfg(windows)]
        IOSEVKA_SEMIBOLD_ID => IOSEVKA_SEMIBOLD_FACES,
        IOSEVKA_ID => IOSEVKA_FACES,
        _ => return [None; 3],
    };
    [family[1], family[2], family[3]].map(face_for_id)
}

/// Cascadia Mono PL's four faces, `[regular, bold, italic, bold-italic]`.
const CASCADIA_FACES: [&str; 4] = [
    CASCADIA_ID,
    CASCADIA_BOLD_ID,
    CASCADIA_ITALIC_ID,
    CASCADIA_BOLD_ITALIC_ID,
];
/// JetBrains Mono's four faces, `[regular, bold, italic, bold-italic]`.
const JETBRAINS_FACES: [&str; 4] = [
    JETBRAINS_ID,
    JETBRAINS_BOLD_ID,
    JETBRAINS_ITALIC_ID,
    JETBRAINS_BOLD_ITALIC_ID,
];
/// Iosevka Term's four faces, `[regular, bold, italic, bold-italic]`.
const IOSEVKA_FACES: [&str; 4] = [
    IOSEVKA_ID,
    IOSEVKA_BOLD_ID,
    IOSEVKA_ITALIC_ID,
    IOSEVKA_BOLD_ITALIC_ID,
];
/// Iosevka Term Semibold's four slots, `[regular, bold, italic, bold-italic]`:
/// the SemiBold, ExtraBold, SemiBold Italic and ExtraBold Italic faces.
const IOSEVKA_SEMIBOLD_FACES: [&str; 4] = [
    IOSEVKA_SEMIBOLD_ID,
    IOSEVKA_SEMIBOLD_BOLD_ID,
    IOSEVKA_SEMIBOLD_ITALIC_ID,
    IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID,
];

/// The four-face families a bundled PRIMARY can come from, each in
/// `[regular, bold, italic, bold-italic]` order: the defaults (Iosevka Term
/// Semibold on Windows, JetBrains Mono elsewhere), and Cascadia Mono PL and
/// Iosevka Term, which only a user's choice makes the primary. The order is
/// only the letter floor's memo slot; nothing reads a family by its index.
const PRIMARY_FAMILIES: [[&str; 4]; 4] = [
    CASCADIA_FACES,
    JETBRAINS_FACES,
    IOSEVKA_FACES,
    IOSEVKA_SEMIBOLD_FACES,
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
/// * Iosevka Term Semibold (measured 2026-10-16, its SemiBold, ExtraBold and
///   both italics): 259 of 1000, inside the same 285 descent. No change: the
///   clear row between lines it needs is the Windows 1.1 line height's, not
///   the floor's (on a 1.0 line at 17 px a `j` ends on the row an `Š`'s caron
///   starts under it).
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
/// the four Cascadia Mono PL faces, `[regular, bold, italic, bold-italic]`,
/// named as Cascadia's — never as a position in [`PRIMARY_FAMILIES`], so a
/// family added there, or a new default, cannot move the yield.
const PRIVATE_USE_YIELDING_IDS: [&str; 4] = CASCADIA_FACES;

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
/// configures, another PL build, JetBrains Mono, and both Iosevka Term sets —
/// the Windows default among them, whose Powerline glyphs stand inside its
/// line (MEASURED 2026-10-16 at 17 px on the 1.1 line: U+E0A0..E0A3 ink rows
/// 3..19 of the 23-row cell, columns 0..9 of 9).
#[must_use]
pub fn yields_private_use(bytes: &[u8]) -> bool {
    PRIVATE_USE_YIELDING_IDS
        .iter()
        .any(|id| face_for_id(id).is_some_and(|face| same_face(face, bytes)))
}

/// The code points Iosevka Term draws TWO cells wide — an advance of 1000 per
/// 1000 where every other glyph's is 500 — while the grid gives each ONE
/// column (none is East Asian Wide): `⟝ ⟞` (U+27DD, U+27DE), the long arrows
/// `⟵`..`⟿` (U+27F5..27FF) and `⬳` (U+2B33). MEASURED 2026-10-16 over the
/// cmaps of all eight Iosevka Term faces, both sets; JetBrains Mono and
/// Cascadia Mono PL draw no glyph wider than their cell.
#[must_use]
pub fn is_two_cell_glyph(ch: char) -> bool {
    matches!(
        ch,
        '\u{27DD}' | '\u{27DE}' | '\u{27F5}'..='\u{27FF}' | '\u{2B33}'
    )
}

/// THE GRID OWNS THE COLUMNS: whether `bytes` are a bundled face whose
/// two-cell glyphs ([`is_two_cell_glyph`]) a window FITS into the one column
/// the grid gives each, instead of drawing them across the next cell. True
/// for the four Iosevka Term Semibold faces (compiled in on Windows only, so
/// `false` everywhere else). The renderer reads it once per loaded primary
/// and stamps such a cell's key one cell wide
/// (`Renderer::resolve_cell_key_for_span`): the fit an explicit VS15 primary
/// glyph already takes, the ink shrunk to the cell, level with its own
/// centre.
///
/// MEASURED 2026-10-16: drawn at its own width, the face's `⟹` buried the
/// `│` of a table to its right — the defect `fallback_harmony`'s
/// `a_long_arrow_does_not_shear_a_box_drawing_row` was written for. Under
/// Cascadia Mono PL, which has no `⟹`, the arrow came from Noto Sans Math
/// through the fallback fit, the same shrink. Iosevka Term (Regular), the
/// opt-in set every platform carries, maps the same fourteen and still draws
/// them at their own width.
///
/// BY THE BYTES, like [`yields_private_use`]: a face can arrive without its
/// identity (`Renderer::set_primary_font` takes bytes).
#[must_use]
pub fn fits_two_cell_glyphs(bytes: &[u8]) -> bool {
    is_iosevka_semibold(bytes)
}

/// THE GRID OWNS THE COLUMNS, the other way round: whether `bytes` are a
/// bundled face that leaves every character the grid ALWAYS gives two
/// columns — East Asian Wide or Fullwidth, `aterm_grapheme::char_width` 2
/// whatever `ambiguous_width` says — to the fallback chain, instead of
/// drawing it with its own ONE-column glyph. True for the four Iosevka Term
/// Semibold faces (compiled in on Windows only, so `false` everywhere else).
/// The renderer reads it once per loaded primary and applies it per code
/// point (`Renderer::primary_unicode_gid`), so the chain draws such a
/// character fitted and centred across its two columns, from the face it
/// came from under Cascadia Mono PL.
///
/// MEASURED 2026-10-16 over the cmaps: each Semibold face maps 38 such
/// points, every one drawn one column wide (advance 500 per 1000, the
/// cell's) — 22 whose default presentation is emoji (`⌚ ⌛ ⏩ ⏪ ⏫ ⏬ ⏰ ⏳
/// ◽ ◾ ⚡ ⚪ ⚫ ✅ ❌ ❎ ➰ ➿ ⬛ ⬜ ⭐ ⭕`), the angle brackets `〈 〉` (U+2329,
/// U+232A), the trigrams `☰`..`☷` and the monograms and digrams `⚊`..`⚏`.
/// Drawn from the face, the `✅ ❌` of Claude Code's replies stood at the
/// left of their slot at half its width, the right column blank. The engine
/// marks none of the emoji for colour, VS16 or not (`emoji_presentation` is
/// set only for a text-default emoji), so under Cascadia they came from the
/// chain — Segoe UI Symbol for the brackets and `☰`, Noto Sans Symbols 2
/// for the rest, and since 2026-10-17 Segoe UI Emoji, in colour, for the
/// emoji, which every text tier now leaves to the colour face — and so they
/// do again: the same faces, measured. Only `◽ ◾ ⬛ ⬜`, the four Cascadia
/// maps itself (as narrow), now come from the chain too. Iosevka Term
/// (Regular), the opt-in set, maps the same 38 and still draws them itself.
///
/// BY THE BYTES, like [`yields_private_use`]: a face can arrive without its
/// identity (`Renderer::set_primary_font` takes bytes).
#[must_use]
pub fn yields_wide_characters(bytes: &[u8]) -> bool {
    is_iosevka_semibold(bytes)
}

/// The I CHING LINE SYMBOLS: the trigrams `☰`..`☷` (U+2630..2637) and the
/// monograms and digrams `⚊`..`⚏` (U+268A..268F) — one family of stacked
/// solid and broken bars, which a BUNDLED broad face leaves to the symbol
/// tier (`Renderer::fallback_has`).
///
/// MEASURED 2026-10-17: the embedded DejaVu Sans Mono maps only the two
/// monograms `⚊ ⚋`, and draws them as a bar ON THE BASELINE (ink 7..150 of
/// a 2048 em), where every face that maps the family draws its bars about
/// the middle of the line — Noto Sans Symbols 2's `⚊` at 295..406 of 1000,
/// Segoe UI Symbol's at 597..837 of 2048, Iosevka Term's at 301..379. Ahead
/// of the symbol tier it drew `⚊` as a low underscore beside the mid-line
/// bars of a `☰`; left to that tier, every one sits at mid-line.
#[must_use]
pub fn is_yijing_line_symbol(ch: char) -> bool {
    matches!(ch, '\u{2630}'..='\u{2637}' | '\u{268A}'..='\u{268F}')
}

/// The SPINNER AND FOOTER DINGBATS: the teardrop, star and asterisk dingbats
/// `✢`..`✽` (U+2722..273D) and the media triangles `⏴ ⏵ ⏶ ⏷`
/// (U+23F4..23F7) — Claude Code's spinner (`· ✢ ✳ ✶ ✻ ✽`) and its
/// `⏵⏵ bypass permissions on` footer, the glyphs an agent session shows all
/// day. See [`yields_small_dingbats`].
#[must_use]
pub fn is_small_dingbat(ch: char) -> bool {
    matches!(ch, '\u{2722}'..='\u{273D}' | '\u{23F4}'..='\u{23F7}')
}

/// Whether `bytes` are a bundled face that leaves the [`is_small_dingbat`]
/// set to the fallback chain instead of drawing it itself. True for the four
/// Iosevka Term Semibold faces (compiled in on Windows only, so `false`
/// everywhere else). The renderer reads it once per loaded primary and
/// applies it per code point (`Renderer::primary_unicode_gid`), like
/// [`yields_wide_characters`].
///
/// MEASURED 2026-10-17 at the Windows default (17 px, 9x23 cell): the face
/// draws these as x-height marks — `✻` 8x8 px with 30 ink texels, `✶` 25,
/// the size of its ASCII `*` (33) — and `⏵` as a 4x8 triangle, so the spinner
/// cycle reads as one small asterisk changing nothing, where the owner's
/// 0.101 drew a spoked star twice that ink. The chain draws them from the
/// faces they came from under Cascadia Mono PL, fitted into the cell: the
/// asterisks from the embedded DejaVu Sans Mono (`✻` 9x11 with 52 texels at
/// 16 px), the triangles from Noto Sans Symbols 2. Shrinking a glyph is the
/// fit's job; enlarging Iosevka's own would only blur it. The five spinner
/// frames `✢ ✳ ✶ ✻ ✽` go one step further: DejaVu's fitted `✳` and `✶` were
/// soft (8.5% and 14% of their ink solid, about half `✽`'s ink, measured
/// 2026-10-18), so below [`SPINNER_STARS_CRISP_PX`] the renderer draws those
/// from the cell, ahead of every face (`procedural::is_spinner_star`).
#[must_use]
pub fn yields_small_dingbats(bytes: &[u8]) -> bool {
    is_iosevka_semibold(bytes)
}

/// The text size, in device pixels, from which the chain draws Claude Code's
/// spinner frames `✢ ✳ ✶ ✻ ✽` crisp, so a primary that yields them
/// ([`yields_small_dingbats`]) leaves them to the chain's own glyphs; below
/// it the renderer draws them from the cell (`Renderer::spinner_star_preempts`).
///
/// MEASURED 2026-10-18 on the sealed Windows default chain, the embedded
/// DejaVu Sans Mono fitted into the cell, at the sizes Windows' display
/// scales give the default 17 px: the share of each frame's inked texels at
/// 90% coverage or more, for `✢ ✶ ✻ ✽` (Claude Code's cycle), is 0-12% at
/// 17 px (100%, where `✢` never reaches full ink), 5-13% at 21 px (125%),
/// 16-32% at 26 px (150%), 18-39% at 30 px (175%), 22-32% at 32 px and 22-35%
/// at 34 px (200%). Up to 175% the face's slender `✶` is the soft one (16%
/// and 18%, grey tips); from 32 px every frame is at least a fifth solid —
/// a thin cross, a slender star and whole teardrops, the face's designs,
/// which read better there than the cell-drawn ones, drawn solid for the
/// small sizes. Their weights are evened across the cycle
/// ([`SPINNER_FRAME_SPREAD`]).
pub const SPINNER_STARS_CRISP_PX: f32 = 32.0;

/// The most one of Claude Code's spinner frames may outweigh another where a
/// face draws them ([`SPINNER_STARS_CRISP_PX`] up): the ratio of the heaviest
/// frame's ink to the lightest's. The faces' dingbats are not one weight
/// family — the embedded DejaVu Sans Mono's fitted `✢ ✳ ✶ ✻ ✽` carried 70,
/// 64, 71, 119 and 158 texels of ink at 200% (34 px), 2.5x end to end and
/// 3.0x at 300%, so the spinner pulsed thin to heavy each cycle (the
/// 2026-10-18 judge's `j3t-2x-spinner-weight-pulse`) where the cell-drawn
/// frames below that size stay within 1.35x. The renderer moves each frame's
/// outline in or out by a fraction of a pixel (`Renderer::even_spinner_frame`)
/// so the cycle's inks span this ratio about `✻`'s, the frame Claude Code
/// rests on and prints beside every finished turn, which keeps its raster:
/// `✢ ✳ ✶` gain 38-48% at 200% and `✽` gives up 16%, and each frame keeps
/// its rank and its design. Under 1.5x, so Claude Code's Windows cycle
/// `✢ * ✶ ✻ ✽`, whose `*` is the primary's own, spans 1.41x at 200%.
pub const SPINNER_FRAME_SPREAD: f32 = 1.4;

/// How far Iosevka's own HEX-LOW asterisk sits below its default hex-high
/// one, per em: 266 units of its 1000 in all four Semibold source faces.
pub const HEX_LOW_DROP: f32 = 0.266;

/// How far the hex-low asterisk of an Iosevka ITALIC also moves left, per em:
/// 44 units, the 266-unit drop along its slant.
pub const HEX_LOW_ITALIC_LEFT: f32 = 0.044;

/// THE ASTERISK SITS ON THE OPERATOR LINE: where a face's `*` is drawn
/// lowered to Iosevka's own HEX-LOW asterisk ([`lowered_asterisk`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoweredAsterisk {
    /// The face's glyph for U+002A.
    pub gid: u16,
    /// That glyph's left edge and its box's vertical centre, per em.
    x_min: f32,
    y_mid: f32,
    /// The move to hex-low, per em, y up: down, and left on an italic.
    dx: f32,
    dy: f32,
}

impl LoweredAsterisk {
    /// The move at `px`, in whole device pixels `(x, y)`, y up, of a raster
    /// of the glyph whose inked rows are centred `inked_mid` px above the
    /// baseline: down so that centre lands nearest the hex-low outline's, and
    /// on an italic left by the glyph's left edge rounded where it lands
    /// before and after. Measured on the raster because the hinter places
    /// the raised glyph's rows, not its outline's box.
    #[must_use]
    pub fn shift_px(&self, px: f32, inked_mid: f32) -> (i32, i32) {
        let x = ((self.x_min + self.dx) * px).round() as i32 - (self.x_min * px).round() as i32;
        let y = ((self.y_mid + self.dy) * px - inked_mid).round() as i32;
        (x, y)
    }
}

/// Whether `bytes` are a bundled face that draws its `*` as Iosevka's own
/// HEX-LOW asterisk, centred on the operator line like its `+`, instead of
/// its default raised one, and where: `Some` for the four Iosevka Term
/// Semibold faces (compiled in on Windows only, so `None` everywhere else).
/// The renderer reads it once per loaded face and applies it per glyph
/// (`Renderer::lower_asterisk`).
///
/// Iosevka Term raises its `*` like a superscript: centred at 0.613 em in the
/// SemiBold, where its `+ - = ·` sit at 0.340 em. Outside Ghostty, Claude Code
/// animates `· ✢ * ✶ ✻ ✽` and that `*` is the primary's own, so at 17 px the
/// spinner hopped 5 px up on it twice a cycle (the 2026-10-18 judge's
/// `claude-spinner-ascii-star-frame-odd`). Windows Terminal's Cascadia Mono
/// centres its `*` at 0.350 em, JetBrains Mono at 0.361.
///
/// MEASURED 2026-10-18 in the official 34.9.0 release faces the subsets are
/// cut from (`publish/DECISIONS.md`): Iosevka offers its twelve asterisks as
/// the alternates of a `VSAB` feature, no `cvNN` — hex, turned hex, penta and
/// turned penta, each high, mid and low — and its stylistic sets `ss05`,
/// `ss08`, `ss09`, `ss14` (its JetBrains Mono style) and `ss15` pick hex-low,
/// `VSAB=3`, centred at 0.347 em. The subsets keep neither feature, so that
/// glyph is not in the bundled bytes. But hex-low IS the default asterisk
/// moved: the same outline, point for point, [`HEX_LOW_DROP`] lower in all
/// four faces and [`HEX_LOW_ITALIC_LEFT`] to the left on the two italics.
/// So the renderer moves the face's own hinted raster there, by whole
/// pixels so it stays crisp ([`LoweredAsterisk::shift_px`]). Against the
/// font's own hex-low, hinted by aterm from the release faces at every size
/// from 8 to 60 px, its inked rows are centred within a pixel of the
/// hex-low's in all four faces, and within half a pixel at 43 to 49 of the
/// 53 sizes, face by face. At the default 17 px, drawn live through aterm
/// with `font_features = ["VSAB=3"]`, the upright faces' hex-low is this
/// raster within 2 of 255 coverage levels, its ink rows the `+`'s. The
/// hinter fits the moved outline anew, so the two rasters can differ by a
/// fringe, and the italics' by more.
///
/// JUDGED ON RENDERS the same day (`a * b`, `/* */`, `**kwargs`,
/// `src/**/*.rs`, a markdown `* item`, `git log --graph`, a prompt's `*.rs`,
/// Claude Code's spinner rows) against the raised asterisk, Iosevka's hex-mid
/// (0.480 em, which still hops), Cascadia Mono PL and JetBrains Mono: hex-low
/// reads as the operator in `a * b` and `x *= 3`, opens `/*` at the slash's
/// middle, sits ON `git log --graph`'s rails as a node where the raised one
/// perched above them, centres a markdown bullet as Cascadia's does, and
/// leaves the spinner level.
///
/// Only these four faces: the opt-in Iosevka Term (Regular) set, an
/// installed Iosevka and every other face keep their own asterisk. BY THE
/// BYTES, like [`yields_private_use`]: a face can arrive without its
/// identity (`Renderer::set_primary_font` takes bytes).
#[must_use]
pub fn lowered_asterisk(bytes: &[u8]) -> Option<LoweredAsterisk> {
    if !is_iosevka_semibold(bytes) {
        return None;
    }
    let face = aterm_font::Face::parse(bytes, 0).ok()?;
    let gid = face.glyph_index('*')?;
    let b = face.glyph_bounding_box(gid)?;
    let upem = f32::from(face.units_per_em());
    Some(LoweredAsterisk {
        gid: gid.0,
        x_min: f32::from(b.x_min) / upem,
        y_mid: (f32::from(b.y_min) + f32::from(b.y_max)) / 2.0 / upem,
        dx: if face.is_italic() {
            -HEX_LOW_ITALIC_LEFT
        } else {
            0.0
        },
        dy: -HEX_LOW_DROP,
    })
}

/// Whether `bytes` are one of the four bundled Iosevka Term Semibold faces,
/// by contents ([`same_face`]).
fn is_iosevka_semibold(bytes: &[u8]) -> bool {
    IOSEVKA_SEMIBOLD_FACES
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
        .filter_map(|bytes| aterm_font::Face::parse(bytes, 0).ok())
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

/// The DEFAULT primary candidate: the bundled primary's identity (Iosevka
/// Term Semibold on Windows, JetBrains Mono elsewhere), placed ahead of the built-in
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
/// primary first, on Windows Cascadia Mono PL (the default before it) and
/// JetBrains Mono beside it, and Iosevka Term everywhere (macOS included,
/// where it is the one).
#[must_use]
pub fn selectable_families() -> &'static [&'static str] {
    if !IOSEVKA_ACTIVE {
        return &[];
    }
    #[cfg(windows)]
    {
        &[
            IOSEVKA_SEMIBOLD_FAMILY,
            CASCADIA_FAMILY,
            JETBRAINS_FAMILY,
            IOSEVKA_FAMILY,
        ]
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
        // Iosevka Term Semibold resolves by name, in all four styles and by
        // its bold faces' own family name, exactly where it is compiled in
        // (Windows), and nowhere else.
        for (spelling, id) in [
            ("Iosevka Term Semibold", IOSEVKA_SEMIBOLD_ID),
            ("iosevka-term-semibold", IOSEVKA_SEMIBOLD_ID),
            ("IosevkaTermSemiBold", IOSEVKA_SEMIBOLD_ID),
            (" IOSEVKA TERM SEMIBOLD REGULAR ", IOSEVKA_SEMIBOLD_ID),
            ("Iosevka Term Semibold Bold", IOSEVKA_SEMIBOLD_BOLD_ID),
            ("Iosevka Term Semibold Italic", IOSEVKA_SEMIBOLD_ITALIC_ID),
            (
                "Iosevka Term Semibold Bold Italic",
                IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID,
            ),
            ("Iosevka Term Extrabold", IOSEVKA_SEMIBOLD_BOLD_ID),
            (
                "Iosevka Term ExtraBold Italic",
                IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID,
            ),
            (IOSEVKA_SEMIBOLD_ID, IOSEVKA_SEMIBOLD_ID),
        ] {
            assert_eq!(
                face_for_family(spelling).map(|(id, _)| id),
                IOSEVKA_SEMIBOLD_ACTIVE.then_some(id),
                "{spelling:?}"
            );
        }
        // An installed family keeps meaning the installed face — including
        // `Cascadia Mono`, the system's variable cut, which is not this one,
        // and `Iosevka` (not Term), whose arrows are wide.
        assert!(face_for_family("Iosevka").is_none());
        assert!(face_for_family("Iosevka Term Medium").is_none());
        assert!(face_for_family("DejaVu Sans Mono").is_none());
        assert!(face_for_family("JetBrains Mono NL").is_none());
        assert!(face_for_family("Cascadia Mono").is_none());
        assert!(face_for_family("Cascadia Code").is_none());
        assert!(face_for_family("Cascadia Code PL").is_none());
        assert!(face_for_family("").is_none());
    }

    /// THE LETTER FLOOR is measured from the family's own outlines, answers
    /// for each of its four faces, and is a property of the BUNDLED bytes only.
    #[test]
    fn the_letter_floor_is_the_familys_deepest_latin_letter() {
        let per_em = |id: &str| letter_descent_floor(face_for_id(id).unwrap());
        if IOSEVKA_ACTIVE {
            // Iosevka Term: 256 of 1000 (the comma-below letters), inside
            // its 285 descent — the floor exists and changes nothing.
            for id in IOSEVKA_FACES {
                assert_eq!(per_em(id), Some(-0.256), "{id}");
            }
        }
        if IOSEVKA_SEMIBOLD_ACTIVE {
            // Iosevka Term Semibold: 259 of 1000, inside the same 285 descent.
            for id in IOSEVKA_SEMIBOLD_FACES {
                assert_eq!(per_em(id), Some(-0.259), "{id}");
            }
        }
        if !ACTIVE {
            #[cfg(feature = "embedded-font")]
            assert!(letter_descent_floor(crate::embedded_font()).is_none());
            return;
        }
        // JetBrains Mono: 240 of 1000 (the comma-below letters), inside its
        // 300 descent — the floor exists and changes nothing.
        for id in JETBRAINS_FACES {
            assert_eq!(per_em(id), Some(-0.24), "{id}");
        }
        if CASCADIA_ACTIVE {
            // Cascadia Mono PL: the BOLD faces' 608 of 2048 governs all four.
            for id in CASCADIA_FACES {
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
    /// italic bit, and by their `name`-table family (name ID 1). The Iosevka
    /// Term Semibold slots are the SemiBold (600) and ExtraBold (800) faces,
    /// whose name ID 1 reads `Iosevka Term Semibold` / `Iosevka Term
    /// Extrabold`, and none of its four carries `calt`.
    #[test]
    fn every_bundled_face_parses_and_each_family_is_a_real_monospace_family() {
        let served: Vec<&str> = ALL_IDS
            .iter()
            .copied()
            .filter(|id| face_for_id(id).is_some())
            .collect();
        let expected_count = usize::from(ACTIVE) * 7
            + usize::from(CASCADIA_ACTIVE) * 4
            + usize::from(IOSEVKA_ACTIVE) * 4
            + usize::from(IOSEVKA_SEMIBOLD_ACTIVE) * 4;
        assert_eq!(served.len(), expected_count, "{served:?}");
        for id in &served {
            let bytes = face_for_id(id).expect("filtered to served ids");
            let face = aterm_font::Face::parse(bytes, 0).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(face.number_of_glyphs() > 100, "{id}");
        }
        /// A family's four identities, each face's name ID 1, and each
        /// face's `(OS/2 weight, italic)`.
        type Family = ([&'static str; 4], [&'static str; 4], [(u16, bool); 4]);
        const PLAIN: [(u16, bool); 4] = [(400, false), (700, false), (400, true), (700, true)];
        let mut families: Vec<Family> = Vec::new();
        if ACTIVE {
            families.push((JETBRAINS_FACES, [JETBRAINS_FAMILY; 4], PLAIN));
        }
        if IOSEVKA_ACTIVE {
            families.push((IOSEVKA_FACES, [IOSEVKA_FAMILY; 4], PLAIN));
        }
        if CASCADIA_ACTIVE {
            families.push((CASCADIA_FACES, [CASCADIA_FAMILY; 4], PLAIN));
        }
        if IOSEVKA_SEMIBOLD_ACTIVE {
            let extrabold = "Iosevka Term Extrabold";
            families.push((
                IOSEVKA_SEMIBOLD_FACES,
                [
                    IOSEVKA_SEMIBOLD_FAMILY,
                    extrabold,
                    IOSEVKA_SEMIBOLD_FAMILY,
                    extrabold,
                ],
                [(600, false), (800, false), (600, true), (800, true)],
            ));
        }
        for (ids, names, expected) in families {
            let weights: Vec<(u16, bool)> = ids
                .iter()
                .zip(names)
                .map(|(id, family)| {
                    let f = aterm_font::Face::parse(face_for_id(id).unwrap(), 0).unwrap();
                    assert!(f.is_monospaced(), "{id} must be monospaced");
                    assert!(
                        f.variation_axes().into_iter().next().is_none(),
                        "{id} must be a STATIC face"
                    );
                    let names = f.names();
                    let name = (0..names.len()).find_map(|i| {
                        let n = names.get(i)?;
                        (n.name_id == aterm_font::name_id::FAMILY)
                            .then(|| n.to_string())
                            .flatten()
                    });
                    assert_eq!(name.as_deref(), Some(family), "{id}: name ID 1");
                    (f.weight().to_number(), f.is_italic())
                })
                .collect();
            assert_eq!(weights, expected, "{}", names[0]);
        }
        for id in IOSEVKA_SEMIBOLD_FACES {
            if let Some(bytes) = face_for_id(id) {
                assert!(
                    !crate::ligature_shaping::font_has_ligature_features(bytes, 0),
                    "{id}: the Windows default must carry no ligature feature"
                );
            }
        }
    }

    // ---- the stack, bound to the real renderer ------------------------------

    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    use crate::{FaceId, GlyphClass};
    #[cfg(feature = "embedded-font")]
    use crate::{Renderer, Theme};

    /// The default renderer a GUI window builds when nothing is configured,
    /// sealed exactly as the backend worker seals it.
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn default_sealed() -> Renderer {
        let mut r = Renderer::from_system(18.0, Theme::default()).expect("a default face");
        r.seal_admitted_font_sources();
        r
    }

    /// Where `ch` resolves in the sealed default chain, as a readable label:
    /// the bundled identity or system path of the face that draws it.
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
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
    /// platform face — Iosevka Term Semibold on Windows (not the system's
    /// variable `CascadiaMono.ttf`, the first Windows candidate, nor the
    /// bundled Cascadia Mono PL that was the default until 2026-10-16),
    /// JetBrains Mono on Linux (not the system DejaVu Sans Mono `fc-match
    /// monospace` answers, which is the first system candidate and used to
    /// win).
    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn the_default_primary_is_the_bundled_platform_face() {
        let expected = if cfg!(windows) {
            IOSEVKA_SEMIBOLD_ID
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

    /// A CONFIGURED family still wins over the bundled default, by path; and
    /// every bundled family a build compiles in is listed as selectable.
    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn a_configured_font_still_wins_over_the_bundled_default() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/DejaVuSansMono.ttf");
        let r = Renderer::from_system_with_family(Some(path), 18.0, Theme::default()).unwrap();
        assert_eq!(
            r.primary_source_path(),
            Some(path),
            "a configured path wins"
        );
        let mut families = vec![JETBRAINS_FAMILY];
        if CASCADIA_ACTIVE {
            families.push(CASCADIA_FAMILY);
        }
        if IOSEVKA_SEMIBOLD_ACTIVE {
            families.push(IOSEVKA_SEMIBOLD_FAMILY);
        }
        for family in families {
            assert!(
                selectable_families().contains(&family),
                "{family} is bundled but not listed as selectable"
            );
        }
    }

    /// A bundled family resolves by NAME through every resolver (startup,
    /// config admission, the catalogue worker) to its compiled-in faces, and
    /// measures its own `cell` at the 16 px probe.
    ///
    /// The NAME yields to an installed copy (see `resolve_family_or_bundled`,
    /// and the catalogue's fixture test for that half), so on a host that has
    /// one the bundle cannot be asked by name: COULD NOT RUN.
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn bundled_family_resolves_by_name(
        family: &str,
        id: &str,
        bold_id: &str,
        cell: (usize, usize),
    ) {
        if crate::resolve_font_family(family, crate::FaceTarget::REGULAR).is_some() {
            aterm_spec::host::could_not_run(format!(
                "{family} is installed on this host, and an installed copy wins the name"
            ));
        }
        for spelling in [family, id] {
            let r =
                Renderer::from_system_with_family(Some(spelling), 18.0, Theme::default()).unwrap();
            assert_eq!(r.primary_source_path(), Some(id), "{spelling}");
            assert_eq!(
                crate::resolve_config_font(spelling, crate::FaceTarget::REGULAR)
                    .map(|face| face.path)
                    .as_deref(),
                Ok(id)
            );
        }
        let batch = crate::font_catalog::resolve_and_admit(&[
            (family.to_string(), crate::FaceTarget::REGULAR),
            (format!("{family} Bold"), crate::FaceTarget::BOLD),
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
    }

    /// JetBrains Mono by name. Its own line is 1.32 em (ascent 1020 + descent
    /// 300 per 1000 upem, no lineGap) and its advance 0.6 em: ceil(21.12) x
    /// round(9.6) at the 16 px probe. The live renderer ROUNDS the line
    /// (`cell_h_baseline`), so a window draws it 10x21 at 16 px.
    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn jetbrains_mono_resolves_by_name() {
        bundled_family_resolves_by_name(
            JETBRAINS_FAMILY,
            JETBRAINS_ID,
            JETBRAINS_BOLD_ID,
            (10, 22),
        );
    }

    /// Cascadia Mono PL by name, on Windows, where it is compiled in. Its line
    /// at 16 px is its 1900 / 2048 ascent (14.84 px) over the depth of its
    /// deepest letter, 608 / 2048 (4.75 px), plus the hinter's pixel — not the
    /// vendor's 480 (`letter_descent_floor`) — no lineGap, and its advance
    /// 1200 / 2048 em: ceil(20.59) x round(9.375); a window draws it 9x21.
    #[test]
    #[cfg(all(feature = "embedded-font", windows))]
    fn cascadia_mono_pl_resolves_by_name() {
        bundled_family_resolves_by_name(CASCADIA_FAMILY, CASCADIA_ID, CASCADIA_BOLD_ID, (9, 21));
    }

    /// Iosevka Term Semibold by name, on Windows, where it is compiled in (the
    /// platform's default face). Its line is 1.25 em (ascent 965 + descent 285
    /// per 1000, no lineGap; its deepest letter, 259, is inside it) and its
    /// advance 0.5 em: 8x20 at the 16 px probe, Iosevka Term's own cell.
    #[test]
    #[cfg(all(feature = "embedded-font", windows))]
    fn iosevka_term_semibold_resolves_by_name() {
        bundled_family_resolves_by_name(
            IOSEVKA_SEMIBOLD_FAMILY,
            IOSEVKA_SEMIBOLD_ID,
            IOSEVKA_SEMIBOLD_BOLD_ID,
            (8, 20),
        );
    }

    /// IOSEVKA TERM BY NAME, on every platform the `embedded-font` feature is on —
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
    #[cfg(feature = "embedded-font")]
    fn iosevka_term_is_selectable_on_every_platform() {
        assert!(selectable_families().contains(&IOSEVKA_FAMILY));
        assert!(crate::list_fonts().iter().any(|f| f == IOSEVKA_FAMILY));
        for id in IOSEVKA_FACES {
            assert!(!yields_private_use(face_for_id(id).unwrap()), "{id}");
        }
    }

    /// [`iosevka_term_is_selectable_on_every_platform`]'s by-name half: the
    /// name resolves to the compiled-in faces everywhere. An installed Iosevka
    /// Term wins the name, so on a host with one this half COULD NOT RUN.
    #[test]
    #[cfg(feature = "embedded-font")]
    fn iosevka_term_resolves_by_name_on_every_platform() {
        if crate::resolve_font_family(IOSEVKA_FAMILY, crate::FaceTarget::REGULAR).is_some() {
            aterm_spec::host::could_not_run(format!(
                "{IOSEVKA_FAMILY} is installed on this host, and an installed copy wins the name"
            ));
        }
        for spelling in [IOSEVKA_FAMILY, IOSEVKA_ID] {
            assert_eq!(
                crate::resolve_config_font(spelling, crate::FaceTarget::REGULAR)
                    .map(|face| face.path)
                    .as_deref(),
                Ok(IOSEVKA_ID)
            );
            let mut r =
                Renderer::from_system_with_family(Some(spelling), 18.0, Theme::default()).unwrap();
            r.seal_admitted_font_sources();
            assert_eq!(r.primary_source_path(), Some(IOSEVKA_ID), "{spelling}");
            assert_eq!(r.debug_styled_face_indices(), [Some(0); 3]);
            for (slot, id) in r.styled_faces.iter().zip(&IOSEVKA_FACES[1..]) {
                let face = slot.as_ref().expect("slot filled");
                assert_eq!(&face.bytes[..], face_for_id(id).unwrap(), "{id}");
            }
        }
        let batch = crate::font_catalog::resolve_and_admit(&[
            (IOSEVKA_FAMILY.to_string(), crate::FaceTarget::REGULAR),
            (
                format!("{IOSEVKA_FAMILY} Bold Italic"),
                crate::FaceTarget::BOLD_ITALIC,
            ),
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

    /// The size and line height a Windows window draws the default face at:
    /// aterm-gui's `FONT_PX` and its config's line-height default
    /// (`app_config::DEFAULT_LINE_HEIGHT`), which the GUI hands the renderer
    /// through `set_line_height`. The renderer itself knows neither, so the
    /// GUI pins the same cell from its own constants
    /// (`the_windows_defaults_draw_the_judged_9x23_cell`).
    #[cfg(windows)]
    const WINDOWS_DEFAULT_PX: f32 = 17.0;
    /// See [`WINDOWS_DEFAULT_PX`].
    #[cfg(windows)]
    const WINDOWS_DEFAULT_LINE_HEIGHT: f32 = 1.1;

    /// THE OLD LOOK, BACK BY NAME: `font_family = "Cascadia Mono PL"`,
    /// `font_px = 16`, `line_height = 1.0` — the Windows default until
    /// 2026-10-16 — still draws Cascadia's own 9x21 cell on baseline row 15,
    /// from its real styled faces.
    #[test]
    #[cfg(all(feature = "embedded-font", windows))]
    fn cascadia_mono_pl_by_name_at_16_px_keeps_its_9x21_cell() {
        let mut r = cascadia_sealed(16.0);
        r.set_line_height(1.0);
        assert_eq!((r.cell_size(), r.baseline()), ((9, 21), 15));
        assert_eq!(r.debug_styled_face_indices(), [Some(0); 3]);
        for (slot, id) in r.styled_faces.iter().zip(&CASCADIA_FACES[1..]) {
            let face = slot.as_ref().expect("slot filled");
            assert_eq!(&face.bytes[..], face_for_id(id).unwrap(), "{id}");
        }
    }

    /// The bundled Cascadia Mono PL as the primary, by its identity (which
    /// always means the compiled-in face, never an installed one): the face
    /// the yield tests below are about, chosen by name now that it is not the
    /// Windows default.
    #[cfg(all(feature = "embedded-font", windows))]
    fn cascadia(px: f32) -> Renderer {
        let r = Renderer::from_system_with_family(Some(CASCADIA_ID), px, Theme::default())
            .expect("the bundled Cascadia Mono PL");
        assert_eq!(r.primary_source_path(), Some(CASCADIA_ID));
        r
    }

    /// [`cascadia`], sealed as a window seals it.
    #[cfg(all(feature = "embedded-font", windows))]
    fn cascadia_sealed(px: f32) -> Renderer {
        let mut r = cascadia(px);
        r.seal_admitted_font_sources();
        r
    }

    /// THE DEFAULT CELL HOLDS ITS LETTERS (`letter_descent_floor`). Measured on
    /// the rasters the cell draws, at every size from 8 to 40 px on the
    /// platform's default line height (Windows 1.1, elsewhere the face's own)
    /// and in all four styles: every Latin letter that hangs a mark or a
    /// stroke below the baseline ends INSIDE the cell, so the blit's row-band
    /// clip cuts nothing visible off it. And at the default size and line
    /// height (Windows: 17 px on the 1.1 line; elsewhere 16 px on the face's
    /// own) a row of descenders keeps at least one clear pixel row above the
    /// capital accents of the row under it.
    ///
    /// On its own 1.0 line Iosevka Term Semibold keeps its letters too, at
    /// every size but 9 px, where the hinter puts the comma-below and cedilla
    /// letters' marks (`ș ț Ș Ț ķ ļ ņ ŗ Ģ Ķ Ļ Ņ Ŗ`) one row under the 11-row
    /// cell (measured 2026-10-16); its 259 / 1000 is inside its 285 descent,
    /// so the floor does not move its line, and a window's 1.1 line holds
    /// them at every size.
    ///
    /// VISIBLE ink is a row whose strongest pixel reaches 1/8 coverage: the
    /// hinter leaves single-digit fringes (a bold `ķ` inks 5/255 one row
    /// past its 14 px cell) that no eye reads as part of the comma.
    ///
    /// Before the floor, on Windows (Cascadia Mono PL on its vendor line, a
    /// 9x19 cell at 16 px): a regular `ș` ended one row and a bold one two
    /// rows past the cell (coverage 55 and 54/255), and the caron of an `Š`
    /// inked the row a `j` above it ended on. On Windows now the clear row is
    /// the 1.1 line's: Iosevka Term Semibold's accents rise to its line's top
    /// at 17 px, and on its own 1.0 line they start on the row a `j` above
    /// ends on — the collision the 2026-10-16 farm's first round saw.
    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn the_default_cell_holds_its_letters_and_keeps_a_clear_row_between_lines() {
        use crate::{GlyphImage, StyleBits};
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
        #[cfg(windows)]
        let (default_px, default_line_height) = (WINDOWS_DEFAULT_PX, WINDOWS_DEFAULT_LINE_HEIGHT);
        #[cfg(not(windows))]
        let (default_px, default_line_height) = (16.0, 1.0);
        let hanging = "șțȘȚķļņŗģĢĶĻŅŖşţŞŢçÇąęįųgjpqy";
        for px in 8..=40u8 {
            let mut r = Renderer::from_system(f32::from(px), Theme::default()).unwrap();
            r.set_line_height(default_line_height);
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
        // The clear pixel rows between a row's lowest descender and the
        // highest capital accent of the row under it, at `px` on a
        // `line_height` line, on the SEALED generation a window draws with.
        let clear_rows = |px: f32, line_height: f32| -> i32 {
            let mut r = Renderer::from_system(px, Theme::default()).unwrap();
            r.set_line_height(line_height);
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
            // A descender of one row ends at `lowest`; the accent of the row
            // under it starts at `cell_h + highest`.
            cell_h + highest - lowest
        };
        let clear = clear_rows(default_px, default_line_height);
        assert!(
            clear >= 1,
            "{default_px} px on a {default_line_height} line: {clear} clear rows between a \
             row's descenders and the accents of the row under it"
        );
        // Windows: the line height is what buys the row. MEASURED 2026-10-16:
        // two clear rows on the 1.1 line (the farm's J2-lh11 render drew no
        // accent pixel touching a descender), none on the face's own line at
        // the same 17 px.
        #[cfg(windows)]
        {
            assert_eq!(clear, 2, "the measured gap moved");
            let own_line = clear_rows(WINDOWS_DEFAULT_PX, 1.0);
            assert!(
                own_line < 1,
                "CONTROL: on its own 1.0 line the default face kept {own_line} clear rows, so \
                 this test cannot tell the line height's row from the face's"
            );
        }
    }

    /// BOLD / ITALIC RESOLUTION: the bundled primary's three styled slots are
    /// filled from its REAL compiled-in faces — never a sibling FILE lookup,
    /// never the synthetic dilation/shear.
    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn the_bundled_primary_resolves_real_bold_italic_and_bold_italic_faces() {
        let r = default_sealed();
        assert_eq!(r.debug_styled_face_indices(), [Some(0); 3]);
        let styles: Vec<(u16, bool)> = r
            .styled_faces
            .iter()
            .map(|slot| {
                let face = slot.as_ref().expect("slot filled");
                let parsed = aterm_font::Face::parse(&face.bytes, 0).unwrap();
                (parsed.weight().to_number(), parsed.is_italic())
            })
            .collect();
        // Windows: the ExtraBold, SemiBold Italic and ExtraBold Italic faces
        // beside the SemiBold primary.
        let expected = if cfg!(windows) {
            [(800, false), (600, true), (800, true)]
        } else {
            [(700, false), (400, true), (700, true)]
        };
        assert_eq!(styles, expected);
        // And they are THIS build's primary family's own files (Iosevka Term
        // Semibold on Windows: its real italics, not a shear of the upright).
        for (slot, id) in
            r.styled_faces
                .iter()
                .zip([PRIMARY_BOLD_ID, PRIMARY_ITALIC_ID, PRIMARY_BOLD_ITALIC_ID])
        {
            let face = slot.as_ref().expect("slot filled");
            assert_eq!(&face.bytes[..], face_for_id(id).unwrap(), "{id}");
        }
    }

    /// THE WINDOWS DEFAULT IS THE JUDGED CUT, by the bytes: with nothing
    /// configured, the primary and its bold, italic and bold-italic slots
    /// hold exactly the four checked-in subsets of Iosevka Term's SemiBold,
    /// ExtraBold, SemiBold Italic and ExtraBold Italic (each read here by its
    /// own file name, not through the identity table), and the regular
    /// weight is never one of the bundled Iosevka Term Regular set's faces.
    #[test]
    #[cfg(all(feature = "embedded-font", windows))]
    fn the_windows_default_slots_are_the_semibold_and_extrabold_subsets() {
        let r = default_sealed();
        let (primary, _) = r.chrome_primary_face().expect("primary bytes");
        assert_eq!(
            &primary[..],
            include_bytes!("../assets/bundled/IosevkaTerm-SemiBold.ttf")
        );
        let files: [&[u8]; 3] = [
            include_bytes!("../assets/bundled/IosevkaTerm-ExtraBold.ttf"),
            include_bytes!("../assets/bundled/IosevkaTerm-SemiBoldItalic.ttf"),
            include_bytes!("../assets/bundled/IosevkaTerm-ExtraBoldItalic.ttf"),
        ];
        for (slot, file) in r.styled_faces.iter().zip(files) {
            let face = slot.as_ref().expect("slot filled");
            assert_eq!(&face.bytes[..], file);
        }
        for id in IOSEVKA_FACES {
            assert_ne!(&primary[..], face_for_id(id).unwrap(), "{id}");
        }
    }

    /// FALLBACK ORDER over the owner's measured test set, on the SEALED default
    /// generation a window publishes. Each expectation is the first face in the
    /// declared order whose cmap carries the code point — on Linux JetBrains
    /// Mono → DejaVu Sans Mono → Noto Sans Symbols 2 → Noto Sans Math → …, on
    /// Windows Iosevka Term Semibold → DejaVu Sans Mono → the same two Noto
    /// faces → … — and NOTHING in the set may end as `.notdef`.
    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn the_test_set_resolves_in_the_declared_order_and_never_to_notdef() {
        let mut r = default_sealed();
        let primary = format!("primary:{PRIMARY_ID}");
        let dejavu = format!("fallback:{DEJAVU_ID}");
        let sym2 = format!("symbol:{NOTO_SYMBOLS2_ID}");
        let math = format!("symbol:{NOTO_MATH_ID}");
        // WINDOWS, measured from the cmaps (2026-10-16): Iosevka Term
        // Semibold carries the whole set but `ℒ` — the arrows, the checks and
        // crosses, `⚠ ✻`, the media controls Claude Code draws (`⏺ ⏵`) and the
        // math operators — so the chain answers only what it lacks, and the
        // spinner and footer dingbats it leaves (`yields_small_dingbats`): `✻`
        // drawn from the cell ahead of every face (`is_spinner_star`),
        // `⏴ ⏵ ⏶ ⏷` from Noto Sans Symbols 2. The tiers'
        // order is held by points measured to reach each: DejaVu Sans Mono's
        // `☎ ✂ ♔`, Noto Sans Symbols 2's `⮜ ⮞ ⛏`, Noto Sans Math's `ℒ ⏐ ⨍`.
        // (Cascadia Mono PL, the default before it, carried only ◐ of
        // `✗ ⚠ ⇒ ⇐ ⌘ ➜ ∴ ✔ ✘ ↵ ★ ⚙ ✻` and none of the media or math points.)
        #[cfg(windows)]
        let expected: &[(char, &str)] = &[
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
            ('✗', &primary),
            ('⚠', &primary),
            ('⇒', &primary),
            ('⇐', &primary),
            ('⌘', &primary),
            ('➜', &primary),
            ('∴', &primary),
            ('✔', &primary),
            ('✘', &primary),
            ('↵', &primary),
            ('★', &primary),
            ('⚙', &primary),
            ('✻', "Procedural"),
            ('⏴', &sym2),
            ('⏵', &sym2),
            ('⏶', &sym2),
            ('⏷', &sym2),
            ('⏺', &primary),
            ('⏸', &primary),
            ('⊨', &primary),
            ('⨁', &primary),
            ('⩽', &primary),
            ('⟹', &primary),
            ('☎', &dejavu),
            ('✂', &dejavu),
            ('♔', &dejavu),
            ('⮜', &sym2),
            ('⮞', &sym2),
            ('⛏', &sym2),
            ('ℒ', &math),
            ('⏐', &math),
            ('⨍', &math),
        ];
        #[cfg(not(windows))]
        let expected: &[(char, &str)] = &[
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
        for &(ch, want) in expected {
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
        // colour face's, or the synthesis without one — never a bundled CHAIN
        // face; on Windows Segoe UI Symbol's, since the default leaves its own
        // one-column glyph for this two-column character to the chain,
        // `yields_wide_characters`), and whatever carries ⎿ it must not be tofu.
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
    /// chain — the separators U+E0B0..E0BF to the procedural tier; the
    /// branch, line-number, lock and column-number glyphs U+E0A0..E0A3 to
    /// Iosevka Term Semibold itself, whose own glyphs stand inside its cell
    /// (it does not yield them, `yields_private_use`); and everything else to
    /// the embedded Symbols Nerd Font: the Powerline-extra flames, pixels and
    /// trapezoids U+E0C0..E0D4, which the face does not map, the common Nerd
    /// Font prompt set, and the points Segoe UI Symbol and Arial also map
    /// (U+E000, U+E200, U+F013, U+F301) to an unrelated glyph
    /// (`FOREIGN_PRIVATE_USE_FACES` in the crate root). Cascadia Mono PL,
    /// chosen by name, yields its own U+E0A0..E0A3 too
    /// (`the_yielded_powerline_glyphs_draw_inside_their_cell_at_16_px`).
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_windows_prompt_line_resolves_each_icon_to_the_face_that_owns_it() {
        let mut r = default_sealed();
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
        for ch in ['\u{E0A0}', '\u{E0A1}', '\u{E0A2}', '\u{E0A3}'] {
            let key = r.glyph_key(ch);
            assert_eq!(
                (key.source, key.glyph_class),
                (FaceId::Primary, GlyphClass::MonoGid),
                "U+{:04X}: the default face draws its own",
                ch as u32
            );
        }
        for ch in [
            '\u{E0C0}', '\u{E0C4}', '\u{E0D2}', '\u{F418}', '\u{F07C}', '\u{E718}', '\u{E7A8}',
            '\u{F0E7}', '\u{F120}', '\u{F308}', '\u{E5FF}', '\u{F113}', '\u{E000}', '\u{E200}',
            '\u{F013}', '\u{F301}',
        ] {
            assert_embedded_nerd_lane(&mut r, ch);
        }
    }

    /// The default face's own Powerline glyphs, which it keeps, stand inside
    /// the cell a Windows window draws (17 px on the 1.1 line, 9x23): every
    /// pixel of the face's U+E0A0..E0A3 rasters, regular and bold. MEASURED
    /// 2026-10-16: ink rows 3..19 and columns 0..9 (half-open), where
    /// Cascadia Mono PL's own trapezoid painted three rows into the row above
    /// and a column either side. Its italic faces slant them like their
    /// letters, a column left of the cell (the italic branch inks column -1),
    /// as any italic glyph leans into its neighbour.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_default_faces_own_powerline_glyphs_stand_inside_the_windows_cell() {
        use crate::StyleBits;
        let mut r = Renderer::from_system(WINDOWS_DEFAULT_PX, Theme::default()).unwrap();
        r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
        r.seal_admitted_font_sources();
        let (cw, ch) = r.cell_size();
        assert_eq!((cw, ch), (9, 23));
        let baseline = r.baseline();
        for style in [StyleBits::REGULAR, StyleBits::BOLD] {
            for c in ['\u{E0A0}', '\u{E0A1}', '\u{E0A2}', '\u{E0A3}'] {
                let key = r.glyph_key_styled(c, style);
                assert_ne!(key.source, FaceId::RuntimeFallback, "U+{:04X}", c as u32);
                let crate::GlyphImage::Mono {
                    width,
                    height,
                    xmin,
                    ymin,
                    bytes,
                    ..
                } = r.glyph_image(key)
                else {
                    panic!("U+{:04X}: a coverage raster", c as u32);
                };
                let (w, h) = (*width, *height);
                let top = baseline - (*ymin + i32::try_from(h).unwrap());
                for y in 0..h {
                    for x in 0..w {
                        if bytes[y * w + x] == 0 {
                            continue;
                        }
                        let (px, py) = (
                            *xmin + i32::try_from(x).unwrap(),
                            top + i32::try_from(y).unwrap(),
                        );
                        assert!(
                            (0..i32::try_from(cw).unwrap()).contains(&px)
                                && (0..i32::try_from(ch).unwrap()).contains(&py),
                            "U+{:04X} {style:?}: ink at ({px}, {py}) leaves the {cw}x{ch} cell",
                            c as u32
                        );
                    }
                }
            }
        }
    }

    /// `ch` resolves to the runtime tier's I/O-free EMBEDDED lane — the
    /// compiled-in Symbols Nerd Font — in every style: the regular key names
    /// [`FaceId::RuntimeFallback`], the decision the rasterizer recovers it
    /// from is the embedded face, and the bold / italic / bold-italic keys are
    /// that same face's (never a styled sibling of the primary).
    #[cfg(all(windows, feature = "embedded-font"))]
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
        #[cfg(feature = "embedded-font")]
        assert!(!yields_private_use(crate::embedded_symbols_font()));
        // A face of the same length that is not the face does not yield.
        if let Some(bytes) = face_for_id(CASCADIA_ID) {
            let mut other = bytes.to_vec();
            let last = other.len() - 1;
            other[last] ^= 0xFF;
            assert!(!yields_private_use(&other));
        }
    }

    /// THE DEFECT, pinned on pixels at Cascadia Mono PL's 16 px (9x21 cell,
    /// baseline row 15; chosen by name). Its own trapezoid U+E0D2 was 22 px tall,
    /// rows -3..18: three rows painted into the row above, rows 19..20 left
    /// showing the next segment's colour. The yielded glyphs come from the
    /// embedded Nerd Font through the fallback fit, and every pixel of their
    /// raster lies in the cell box — GPU and CPU draw these same bytes at this
    /// same placement. The NEGATIVE CONTROL is the same face with the
    /// runtime tier closed, where nothing yields: its own U+E0D2 still pokes
    /// out of the cell, so the test sees the defect it was written for.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_yielded_powerline_glyphs_draw_inside_their_cell_at_16_px() {
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
        let mut r = cascadia_sealed(16.0);
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
        let mut closed = cascadia(16.0);
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
    #[cfg(all(windows, feature = "embedded-font"))]
    fn a_face_that_is_not_the_bundled_cascadia_keeps_its_own_private_use_glyphs() {
        use crate::{GlyphClass, StyleBits};
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
    /// Settings renderers) nothing could draw a yielded point, so Cascadia
    /// Mono PL keeps drawing its own — never `.notdef`. Opening the tier again
    /// re-resolves every memo, styled ones included, onto the runtime tier
    /// (`set_runtime_font_discovery` drops the styled memo with the rest).
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_yield_waits_for_the_runtime_tier_and_follows_it_open() {
        use crate::{GlyphClass, StyleBits};
        let branch = '\u{E0A0}';
        let mut r = cascadia(16.0);
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
    /// Nerd Font: on Cascadia Mono PL as the primary, and in a BOLD run shaped on an injected
    /// Cascadia bold beside a JetBrains Mono primary, whose own glyphs do not
    /// yield (the run's face follows its style).
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn a_yielded_point_breaks_the_shaping_run() {
        use crate::ColumnGlyph;
        use crate::ligature_shaping::{RowShaper, ShapedRun, build_feature_list, engine_features};
        use aterm_core::terminal::Terminal;
        let text = "a\u{E0A0}\u{E0A2}b";
        let cols: u16 = 8;
        // CONTROL: on the face itself, a run holding the branch and the lock
        // comes back with both columns changed.
        let features = engine_features(&build_feature_list(&[], true));
        let face =
            aterm_font::shape::ShapeFace::from_slice(face_for_id(CASCADIA_ID).unwrap(), 0).unwrap();
        let chars: Vec<char> = text.chars().collect();
        let mut buffer = aterm_font::shape::UnicodeBuffer::new();
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
        let mut r = cascadia_sealed(16.0);
        assert!(
            r.has_ligature_features,
            "Cascadia Mono PL's rows are shaped"
        );
        let plan = plan_of(&mut r, "");
        assert_eq!(plan[1..3], per_cell, "Cascadia Mono PL: {plan:?}");
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

    /// THE WINDOWS DEFAULT DRAWS NO PROGRAMMING LIGATURES — the 2026-10-16
    /// farm's deal-breaker against Iosevka Term's `=>` drawn as `⇒` and `!=`
    /// as `≠`. With `ligatures` at its default (on): shaped under the very
    /// features the row shaper turns on, each of the four Semibold-set faces
    /// returns every glyph of `=> != === -> <= >=` unchanged; the default
    /// renderer never shapes its rows (no `liga` / `calt` in its `GSUB`); and
    /// a row of them plans one per-cell glyph a column, each the face's own
    /// glyph for its own character. CONTROL: the bundled Iosevka Term
    /// (Regular), the same design with `calt` kept, ligates the same text.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_windows_default_draws_no_programming_ligatures() {
        use crate::ColumnGlyph;
        use crate::ligature_shaping::{RowShaper, build_feature_list, engine_features};
        use aterm_core::terminal::Terminal;
        let text = "=> != === -> <= >=";
        let chars: Vec<char> = text.chars().collect();
        let features = engine_features(&build_feature_list(&[], true));
        let shaped_on = |id: &str| {
            let face = aterm_font::shape::ShapeFace::from_slice(face_for_id(id).unwrap(), 0)
                .expect("the face parses for shaping");
            let mut buffer = aterm_font::shape::UnicodeBuffer::new();
            RowShaper::new(face, &features).shape(text, &chars, false, &mut buffer)
        };
        for id in IOSEVKA_SEMIBOLD_FACES {
            assert_eq!(shaped_on(id), None, "{id} substitutes a glyph in {text:?}");
        }
        assert!(
            shaped_on(IOSEVKA_ID).is_some(),
            "CONTROL: Iosevka Term (Regular) no longer ligates {text:?}"
        );

        let cols = u16::try_from(chars.len()).unwrap();
        let plan_of = |r: &mut Renderer| {
            let mut term = Terminal::new(1, cols);
            term.process(format!("\x1b[?25l{text}").as_bytes());
            let input = term.cell_frame(1, usize::from(cols));
            let mut plan = Vec::new();
            r.row_glyph_plan(&input, 0, &[], &mut plan);
            plan
        };
        let mut r = default_sealed();
        assert!(
            !r.has_ligature_features,
            "the default face advertises liga/calt, so its rows would be shaped"
        );
        let plan = plan_of(&mut r);
        assert_eq!(plan.len(), chars.len());
        assert!(
            plan.iter().all(|c| *c == ColumnGlyph::PerCell),
            "the default face ligates: {plan:?}"
        );
        for &ch in chars.iter().filter(|c| !c.is_whitespace()) {
            let key = r.glyph_key(ch);
            assert_eq!(
                (key.source, key.glyph_class),
                (FaceId::Primary, GlyphClass::MonoGid),
                "{ch}"
            );
        }
        let mut control =
            Renderer::from_bytes(face_for_id(IOSEVKA_ID).unwrap(), 17.0, Theme::default()).unwrap();
        control.seal_admitted_font_sources();
        let plan = plan_of(&mut control);
        assert!(
            plan.iter().any(|c| *c != ColumnGlyph::PerCell),
            "CONTROL: a renderer on Iosevka Term (Regular) planned no ligature: {plan:?}"
        );
    }

    /// The bundled faces lead their tiers but never TAKE what the host's own
    /// faces own: no emoji-default and no private-use point is ever picked
    /// from a bundled chain face (the colour face and the Symbols Nerd Font
    /// own those).
    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn a_bundled_chain_face_never_takes_an_emoji_default_or_private_use_point() {
        let mut r = default_sealed();
        let mut checked = 0usize;
        // Every bundled CHAIN face this build places.
        for &id in broad_fallback_ids().iter().chain(symbol_fallback_ids()) {
            let face = aterm_font::Face::parse(face_for_id(id).unwrap(), 0).unwrap();
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
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn a_resolved_bundled_identity_reads_as_its_compiled_in_bytes() {
        let mut cases = Vec::new();
        if crate::resolve_font_family(JETBRAINS_FAMILY, crate::FaceTarget::REGULAR).is_none() {
            cases.extend([
                ("JetBrains Mono", JETBRAINS_ID),
                ("JetBrains Mono Bold", JETBRAINS_BOLD_ID),
                ("JetBrains Mono Italic", JETBRAINS_ITALIC_ID),
                ("JetBrains Mono Bold Italic", JETBRAINS_BOLD_ITALIC_ID),
            ]);
        }
        if CASCADIA_ACTIVE
            && crate::resolve_font_family(CASCADIA_FAMILY, crate::FaceTarget::REGULAR).is_none()
        {
            cases.extend([
                ("Cascadia Mono PL", CASCADIA_ID),
                ("Cascadia Mono PL Bold", CASCADIA_BOLD_ID),
                ("Cascadia Mono PL Italic", CASCADIA_ITALIC_ID),
                ("Cascadia Mono PL Bold Italic", CASCADIA_BOLD_ITALIC_ID),
            ]);
        }
        if IOSEVKA_SEMIBOLD_ACTIVE
            && crate::resolve_font_family(IOSEVKA_SEMIBOLD_FAMILY, crate::FaceTarget::REGULAR)
                .is_none()
        {
            cases.extend([
                ("Iosevka Term Semibold", IOSEVKA_SEMIBOLD_ID),
                ("Iosevka Term Semibold Bold", IOSEVKA_SEMIBOLD_BOLD_ID),
                ("Iosevka Term Semibold Italic", IOSEVKA_SEMIBOLD_ITALIC_ID),
                (
                    "Iosevka Term Semibold Bold Italic",
                    IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID,
                ),
            ]);
        }
        for (name, id) in cases {
            let resolved = crate::resolve_config_font(name, crate::FaceTarget::REGULAR)
                .expect(name)
                .path;
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

    /// THE TWO-CELL GLYPHS, measured from the faces themselves: in each of the
    /// eight Iosevka Term faces exactly the fourteen [`is_two_cell_glyph`]
    /// names advance wider than `M` (1000 against 500 per 1000), and the grid
    /// gives each one column; JetBrains Mono and Cascadia Mono PL have none.
    /// The Semibold set alone asks for the fit ([`fits_two_cell_glyphs`]), by
    /// its bytes wherever they live.
    #[test]
    fn the_two_cell_glyphs_are_the_measured_fourteen_and_only_the_semibold_set_fits_them() {
        let two_cell = |id: &str| -> Option<Vec<char>> {
            let face = aterm_font::Face::parse(face_for_id(id)?, 0).unwrap();
            let cell = face.glyph_hor_advance(face.glyph_index('M').unwrap())?;
            let mut out = Vec::new();
            for sub in face.tables().cmap.unwrap().subtables {
                if sub.is_unicode() {
                    sub.codepoints(|cp| {
                        if let Some(ch) = char::from_u32(cp)
                            && let Some(g) = face.glyph_index(ch).filter(|g| g.0 != 0)
                            && face.glyph_hor_advance(g).is_some_and(|a| a > cell)
                        {
                            out.push(ch);
                        }
                    });
                }
            }
            out.sort_unstable();
            out.dedup();
            Some(out)
        };
        let expected: Vec<char> = (0..=0x10_FFFFu32)
            .filter_map(char::from_u32)
            .filter(|&c| is_two_cell_glyph(c))
            .collect();
        assert_eq!(expected.len(), 14);
        for &ch in &expected {
            assert_eq!(aterm_grapheme::char_width(ch), 1, "{ch}: one grid column");
        }
        for id in IOSEVKA_FACES.iter().chain(&IOSEVKA_SEMIBOLD_FACES) {
            if let Some(found) = two_cell(id) {
                assert_eq!(found, expected, "{id}");
            }
        }
        for id in JETBRAINS_FACES.iter().chain(&CASCADIA_FACES) {
            if let Some(found) = two_cell(id) {
                assert!(found.is_empty(), "{id}: {found:?}");
            }
        }
        for &id in ALL_IDS {
            let Some(bytes) = face_for_id(id) else {
                continue;
            };
            let fits = fits_two_cell_glyphs(bytes);
            assert_eq!(fits, IOSEVKA_SEMIBOLD_FACES.contains(&id), "{id}");
            // A second allocation, so the contents decide, not the pointer.
            let copy = bytes.to_vec();
            assert_eq!(fits_two_cell_glyphs(&copy), fits, "{id}: a copy");
        }
    }

    /// THE WINDOWS DEFAULT'S TWO-CELL GLYPHS STAY IN THEIR COLUMN: in a window
    /// at the default size and line height, each of the fourteen draws from
    /// the default face itself, keyed one cell wide, and its raster lies
    /// inside its 9-px column — where drawn at its own width the long arrow
    /// `⟹` buried the cell to its right (`fallback_harmony`'s
    /// `a_long_arrow_does_not_shear_a_box_drawing_row`, the user-visible law).
    /// A one-cell arrow keeps its ordinary key. CONTROL: on the opt-in Iosevka
    /// Term (Regular), the same design, which does not ask for the fit, `⟹`
    /// keeps its ordinary key and its raster still reaches past the column.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_windows_defaults_two_cell_glyphs_are_fitted_into_their_column() {
        use aterm_core::terminal::Terminal;
        let cell_of = |r: &mut Renderer, ch: char| {
            let mut term = Terminal::new(1, 4);
            term.process(format!("\x1b[?25l{ch}x").as_bytes());
            let input = term.cell_frame(1, 4);
            let span = crate::materialized_cell_span(&input.cells[0], 0);
            assert_eq!(span, 1, "{ch}: the grid gives it one column");
            let key = r.resolve_cell_key_for_span(None, &input.cells[0][0], span);
            let img = r.glyph_image(key).clone();
            (
                key,
                img.xmin(),
                img.xmin() + i32::try_from(img.width()).unwrap(),
            )
        };
        let mut r = Renderer::from_system(WINDOWS_DEFAULT_PX, Theme::default()).unwrap();
        r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
        r.seal_admitted_font_sources();
        let cw = i32::try_from(r.cell_size().0).unwrap();
        assert_eq!(cw, 9);
        let two_cell = ['\u{27DD}', '\u{27DE}', '\u{2B33}']
            .into_iter()
            .chain('\u{27F5}'..='\u{27FF}');
        for ch in two_cell {
            let (key, left, right) = cell_of(&mut r, ch);
            assert_eq!(
                (key.source, key.cell_span),
                (FaceId::Primary, 1),
                "U+{:04X}",
                ch as u32
            );
            assert!(
                left >= 0 && right <= cw,
                "U+{:04X}: raster columns {left}..{right} leave the {cw}-px column",
                ch as u32
            );
        }
        let (key, ..) = cell_of(&mut r, '⇒');
        assert_eq!((key.source, key.cell_span), (FaceId::Primary, 0), "⇒");

        let mut control = Renderer::from_bytes(
            face_for_id(IOSEVKA_ID).unwrap(),
            WINDOWS_DEFAULT_PX,
            Theme::default(),
        )
        .unwrap();
        control.seal_admitted_font_sources();
        let (key, _, right) = cell_of(&mut control, '⟹');
        assert_eq!((key.source, key.cell_span), (FaceId::Primary, 0));
        assert!(
            right > i32::try_from(control.cell_size().0).unwrap(),
            "CONTROL: Iosevka Term (Regular)'s own ⟹ fits its column ({right}), so this test \
             cannot see the overflow it guards against"
        );
    }

    /// The 38 code points the grid always gives two columns that Iosevka Term
    /// maps (measured 2026-10-16, both sets), in code point order: the 22
    /// emoji-presentation points, the angle brackets U+2329 / U+232A, the
    /// trigrams `☰`..`☷` (U+2630..2637) and the monograms and digrams
    /// `⚊`..`⚏` (U+268A..268F) — the last two blocks wide since Unicode 16.
    const IOSEVKA_WIDE_CHARACTERS: [char; 38] = [
        '\u{231A}', '\u{231B}', '\u{2329}', '\u{232A}', '\u{23E9}', '\u{23EA}', '\u{23EB}',
        '\u{23EC}', '\u{23F0}', '\u{23F3}', '\u{25FD}', '\u{25FE}', '\u{2630}', '\u{2631}',
        '\u{2632}', '\u{2633}', '\u{2634}', '\u{2635}', '\u{2636}', '\u{2637}', '\u{268A}',
        '\u{268B}', '\u{268C}', '\u{268D}', '\u{268E}', '\u{268F}', '\u{26A1}', '\u{26AA}',
        '\u{26AB}', '\u{2705}', '\u{274C}', '\u{274E}', '\u{27B0}', '\u{27BF}', '\u{2B1B}',
        '\u{2B1C}', '\u{2B50}', '\u{2B55}',
    ];

    /// The code points `id`'s face maps that the grid always gives two
    /// columns, each with whether its glyph is ONE cell wide (its advance is
    /// `M`'s). `None` when the face is not compiled in.
    fn wide_characters_of(id: &str) -> Option<Vec<(char, bool)>> {
        let face = aterm_font::Face::parse(face_for_id(id)?, 0).unwrap();
        let cell = face.glyph_hor_advance(face.glyph_index('M').unwrap())?;
        let mut out = Vec::new();
        for sub in face.tables().cmap.unwrap().subtables {
            if sub.is_unicode() {
                sub.codepoints(|cp| {
                    if let Some(ch) = char::from_u32(cp)
                        && aterm_grapheme::char_width(ch) == 2
                        && let Some(g) = face.glyph_index(ch).filter(|g| g.0 != 0)
                    {
                        out.push((ch, face.glyph_hor_advance(g) == Some(cell)));
                    }
                });
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    }

    /// THE WIDE CHARACTERS, measured from the faces themselves: in each of
    /// the eight Iosevka Term faces the code points the grid always gives two
    /// columns are exactly the 38 of [`IOSEVKA_WIDE_CHARACTERS`], and every
    /// one is drawn ONE column wide. 22 are emoji by default; the rest are
    /// the angle brackets, the trigrams, the monograms and the digrams. The Semibold set alone yields them
    /// ([`yields_wide_characters`]), by its bytes wherever they live.
    #[test]
    fn the_wide_characters_are_the_measured_thirty_eight_and_only_the_semibold_set_yields_them() {
        let expected: Vec<(char, bool)> = IOSEVKA_WIDE_CHARACTERS
            .iter()
            .map(|&ch| (ch, true))
            .collect();
        let emoji = IOSEVKA_WIDE_CHARACTERS
            .iter()
            .filter(|&&ch| aterm_grapheme::is_emoji_presentation(ch))
            .count();
        assert_eq!(emoji, 22);
        for id in IOSEVKA_FACES.iter().chain(&IOSEVKA_SEMIBOLD_FACES) {
            if let Some(found) = wide_characters_of(id) {
                assert_eq!(found, expected, "{id}");
            }
        }
        // The other bundled primaries map a few, also one column wide, and
        // keep drawing them (Cascadia Mono PL's four were its own glyphs
        // while it was the Windows default).
        for (faces, theirs) in [
            (
                JETBRAINS_FACES,
                ['\u{26A1}', '\u{FE62}', '\u{FF5B}', '\u{FF5D}'],
            ),
            (
                CASCADIA_FACES,
                ['\u{25FD}', '\u{25FE}', '\u{2B1B}', '\u{2B1C}'],
            ),
        ] {
            for id in faces {
                if let Some(found) = wide_characters_of(id) {
                    let want: Vec<(char, bool)> = theirs.iter().map(|&ch| (ch, true)).collect();
                    assert_eq!(found, want, "{id}");
                }
            }
        }
        for &id in ALL_IDS {
            let Some(bytes) = face_for_id(id) else {
                continue;
            };
            let yields = yields_wide_characters(bytes);
            assert_eq!(yields, IOSEVKA_SEMIBOLD_FACES.contains(&id), "{id}");
            // A second allocation, so the contents decide, not the pointer.
            let copy = bytes.to_vec();
            assert_eq!(yields_wide_characters(&copy), yields, "{id}: a copy");
        }
    }

    /// THE WINDOWS DEFAULT'S WIDE CHARACTERS FILL THEIR TWO COLUMNS. In a
    /// window at the default size and line height, each of the 38 wide
    /// characters the default face maps resolves PAST it, to the face it
    /// resolves to under Cascadia Mono PL (the default until 2026-10-16;
    /// but the four Cascadia draws itself, `◽ ◾ ⬛ ⬜`), and never to
    /// `.notdef`; and the `✅ ❌ ⭐ ⚡ ⏳` of an agent's reply draw centred in
    /// their two-column slot, across both columns, where the face's own
    /// one-column glyph stood at the left with the right column blank.
    /// CONTROL: the opt-in Iosevka Term (Regular), which does not yield
    /// them, still draws `✅` from its own glyph, inside the left column.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_windows_defaults_wide_characters_draw_centred_across_their_two_columns() {
        use aterm_core::terminal::Terminal;
        // The inked columns of `ch` as a window draws it, relative to its
        // cell: the key from the materialized two-column span, then every
        // column of its raster with a pixel of 1/8 coverage or more.
        let ink_of = |r: &mut Renderer, ch: char| {
            let mut term = Terminal::new(1, 4);
            term.process(format!("\x1b[?25l{ch}x").as_bytes());
            let input = term.cell_frame(1, 4);
            let span = crate::materialized_cell_span(&input.cells[0], 0);
            assert_eq!(span, 2, "{ch}: the grid gives it two columns");
            let key = r.resolve_cell_key_for_span(None, &input.cells[0][0], span);
            let img = r.glyph_image(key).clone();
            let (w, h) = (img.width(), img.height());
            let texel = if matches!(key.glyph_class, GlyphClass::Mono | GlyphClass::MonoGid) {
                1
            } else {
                4
            };
            let inked: Vec<i32> = (0..w)
                .filter(|&x| (0..h).any(|y| img.bytes()[(y * w + x) * texel + texel - 1] >= 32))
                .map(|x| img.xmin() + i32::try_from(x).unwrap())
                .collect();
            let left = *inked.first().expect("the glyph draws ink");
            let right = *inked.last().unwrap() + 1;
            (key, left, right)
        };
        let mut r = Renderer::from_system(WINDOWS_DEFAULT_PX, Theme::default()).unwrap();
        r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
        r.seal_admitted_font_sources();
        assert_eq!(r.primary_source_path(), Some(IOSEVKA_SEMIBOLD_ID));
        let mut cascadia = cascadia_sealed(WINDOWS_DEFAULT_PX);
        let cascadia_primary = format!("primary:{CASCADIA_ID}");
        for ch in IOSEVKA_WIDE_CHARACTERS {
            let got = resolved_face(&mut r, ch);
            assert!(
                !got.starts_with("primary:") && got != "NOTDEF",
                "U+{:04X} {ch}: {got}",
                ch as u32
            );
            let before = resolved_face(&mut cascadia, ch);
            if before != cascadia_primary {
                assert_eq!(got, before, "U+{:04X} {ch}", ch as u32);
            }
        }
        let cw = i32::try_from(r.cell_size().0).unwrap();
        assert_eq!(cw, 9);
        for ch in ['✅', '❌', '⭐', '⚡', '⏳'] {
            let (key, left, right) = ink_of(&mut r, ch);
            assert_ne!(key.source, FaceId::Primary, "{ch}");
            assert!(
                left < cw && right > cw && left >= 0 && right <= 2 * cw,
                "{ch}: ink columns {left}..{right} do not span the two {cw}-px columns"
            );
            assert!(
                (left - (2 * cw - right)).abs() <= 1,
                "{ch}: ink columns {left}..{right} are not centred in the {}-px slot",
                2 * cw
            );
        }

        let mut control = Renderer::from_bytes(
            face_for_id(IOSEVKA_ID).unwrap(),
            WINDOWS_DEFAULT_PX,
            Theme::default(),
        )
        .unwrap();
        control.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
        control.seal_admitted_font_sources();
        let (key, left, right) = ink_of(&mut control, '✅');
        assert_eq!(key.source, FaceId::Primary);
        assert!(
            left >= 0 && right <= cw,
            "CONTROL: Iosevka Term (Regular)'s own ✅ ({left}..{right}) leaves its left column, \
             so this test cannot see the half-width glyph it guards against"
        );
    }

    /// `ch`'s raster in a sealed window at the default size: its ink texels
    /// (coverage above 40) and the inked rows' count.
    #[cfg(all(windows, feature = "embedded-font"))]
    fn ink_of(r: &mut Renderer, ch: char) -> (usize, usize) {
        let key = r.glyph_key(ch);
        let img = r.glyph_image(key);
        let w = img.width();
        let inked = |y: usize| img.bytes()[y * w..(y + 1) * w].iter().any(|&b| b > 40);
        let rows = (0..img.height()).filter(|&y| inked(y)).count();
        (img.bytes().iter().filter(|&&b| b > 40).count(), rows)
    }

    /// The Windows default, as a window draws it: 17 px on the 1.1 line,
    /// sealed.
    #[cfg(all(windows, feature = "embedded-font"))]
    fn windows_default_sealed() -> Renderer {
        let mut r = Renderer::from_system(WINDOWS_DEFAULT_PX, Theme::default()).unwrap();
        r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
        r.seal_admitted_font_sources();
        assert_eq!(r.primary_source_path(), Some(IOSEVKA_SEMIBOLD_ID));
        r
    }

    /// CLAUDE CODE'S SPINNER READS AS A STAR AGAIN. The Windows default
    /// leaves `✢ ✳ ✶ ✻ ✽` and `⏴ ⏵ ⏶ ⏷` to the chain
    /// ([`yields_small_dingbats`]), which draws each inside its cell with
    /// more ink than the face's own ASCII `*` — the face's own `✢ ✶ ✻` had
    /// less — `✻`, the spinner's resting frame, at least half again as much,
    /// and `⏵` at least 9 rows tall, where the face's own was 7. The spinner
    /// frames are drawn from the cell (`procedural::is_spinner_star`), each
    /// at full ink with at least 30% of it solid, and their ink within 1.5x
    /// of each other, so no frame dims the cycle: DejaVu Sans Mono's fitted
    /// `✳` and `✶` were 8.5% and 14% solid at about half `✽`'s ink.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_windows_defaults_spinner_and_footer_glyphs_outweigh_its_asterisk() {
        let mut r = windows_default_sealed();
        let (cw, chh) = r.cell_size();
        let (star, _) = ink_of(&mut r, '*');
        let mut sums = Vec::new();
        for ch in ['✢', '✳', '✶', '✻', '✽'] {
            let key = r.glyph_key(ch);
            assert_eq!(key.source, FaceId::Procedural, "{ch}: drawn from the cell");
            let crate::GlyphImage::Mono { bytes, .. } = r.glyph_image(key).clone() else {
                panic!("{ch}: a coverage mask");
            };
            let inked = bytes.iter().filter(|&&c| c > 0).count();
            let solid = bytes.iter().filter(|&&c| c >= 230).count();
            assert_eq!(bytes.iter().max(), Some(&255), "{ch}: full ink");
            assert!(10 * solid >= 3 * inked, "{ch}: {solid} of {inked} solid");
            sums.push(bytes.iter().map(|&c| u32::from(c)).sum::<u32>());
        }
        let (lo, hi) = (sums.iter().min(), sums.iter().max());
        assert!(
            hi.zip(lo).is_some_and(|(hi, lo)| 2 * hi <= 3 * lo),
            "frame ink {sums:?}"
        );
        for ch in ['✢', '✳', '✶', '✻', '✽', '⏴', '⏵', '⏶', '⏷'] {
            let key = r.glyph_key(ch);
            assert_ne!(key.source, FaceId::Primary, "{ch}: drawn by the primary");
            let img = r.glyph_image(key).clone();
            let top = r.baseline() - img.height() as i32 - img.ymin();
            assert!(
                img.xmin() >= 0
                    && img.xmin() as usize + img.width() <= cw
                    && top >= 0
                    && top as usize + img.height() <= chh,
                "{ch}: leaves its {cw}x{chh} cell"
            );
        }
        for ch in ['✢', '✳', '✶', '✻', '✽'] {
            let (ink, _) = ink_of(&mut r, ch);
            assert!(ink > star, "{ch}: {ink} ink texels, the face's `*` {star}");
        }
        let (spinner, _) = ink_of(&mut r, '✻');
        assert!(
            2 * spinner >= 3 * star,
            "✻: {spinner} ink texels, not half again the `*`'s {star}"
        );
        let (_, rows) = ink_of(&mut r, '⏵');
        assert!(rows >= 9, "⏵ is {rows} rows tall");
    }

    /// AT 200% THE SPINNER KEEPS ITS FACE'S DESIGNS. The Windows default
    /// draws `✢ ✳ ✶ ✻ ✽` from the cell only while its chain draws them soft
    /// ([`SPINNER_STARS_CRISP_PX`]): at the 100%, 125%, 150% and 175% display
    /// scales (17, 21, 26 and 30 px) each is the procedural tier's; at 32 px,
    /// 200% (34 px) and 250% (42.5 px) each is the embedded DejaVu Sans
    /// Mono's own glyph, inside its cell, and each of `✢ ✶ ✻ ✽`, as the face
    /// draws it fitted into the cell, has at least a fifth of its inked
    /// texels solid: a thin cross, a slender star and whole teardrops. (The
    /// frames drawn are those, their weights evened:
    /// `the_windows_default_evens_its_faces_spinner_frames`.) The judges
    /// measured the cell-drawn `✶` at 200% as a near-hexagon, 80% solid with
    /// twice the face's ink, in place of the face's slender star.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_windows_default_keeps_its_faces_spinner_frames_where_they_are_crisp() {
        let dejavu = format!("fallback:{DEJAVU_ID}");
        for px in [17.0, 21.0, 26.0, 30.0, 32.0, 34.0, 42.5] {
            let mut r = Renderer::from_system(px, Theme::default()).unwrap();
            r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
            r.seal_admitted_font_sources();
            let (cw, chh) = r.cell_size();
            let crisp = px >= SPINNER_STARS_CRISP_PX;
            for ch in ['✢', '✳', '✶', '✻', '✽'] {
                let face = resolved_face(&mut r, ch);
                if !crisp {
                    assert_eq!(face, "Procedural", "{ch} at {px} px: drawn from the cell");
                    continue;
                }
                assert_eq!(face, dejavu, "{ch} at {px} px: the face's own glyph");
                let key = r.glyph_key(ch);
                let img = r.glyph_image(key).clone();
                let top = r.baseline() - img.height() as i32 - img.ymin();
                assert!(
                    img.xmin() >= 0
                        && img.xmin() as usize + img.width() <= cw
                        && top >= 0
                        && top as usize + img.height() <= chh,
                    "{ch} at {px} px: leaves its {cw}x{chh} cell"
                );
                // The face's `✳` is eight hairline spokes, never solid (4-6%
                // at these sizes), and not in Claude Code's cycle.
                if ch != '✳' {
                    let own = r.fitted_fallback_regular(key, ch).5;
                    let inked = own.iter().filter(|&&c| c > 0).count();
                    let solid = own.iter().filter(|&&c| c >= 230).count();
                    assert!(
                        5 * solid >= inked,
                        "{ch} at {px} px: {solid} of {inked} inked texels solid"
                    );
                }
            }
        }
    }

    /// AT 200% CLAUDE CODE'S SPINNER IS ONE WEIGHT. From
    /// [`SPINNER_STARS_CRISP_PX`] the Windows default draws `✢ ✳ ✶ ✻ ✽` from
    /// the embedded DejaVu Sans Mono, whose fitted frames are not one weight
    /// family (70, 64, 71, 119 and 158 texels of ink at 34 px), so the
    /// spinner pulsed thin to heavy each cycle (the 2026-10-18 judge's
    /// `j3t-2x-spinner-weight-pulse`). Each frame is evened
    /// ([`SPINNER_FRAME_SPREAD`]): at 32, 34 (200%), 42.5 (250%) and 51 px
    /// (300%) the frames are still the face's, inside their cells, at full
    /// ink and with solid strokes, their inks span at most that ratio, and
    /// `✻`, the frame Claude Code rests on, is the face's raster byte for
    /// byte. At 200% Claude Code's Windows cycle `✢ * ✶ ✻ ✽`, the primary's
    /// own `*` among them, spans at most 1.5x. Control: the face's own
    /// fitted frames span more than twice.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_windows_default_evens_its_faces_spinner_frames() {
        use crate::procedural::{SPINNER_REST, SPINNER_STARS};
        let ink = |b: &[u8]| b.iter().map(|&c| f32::from(c)).sum::<f32>() / 255.0;
        let spread = |inks: &[f32]| {
            let (lo, hi) = inks
                .iter()
                .fold((f32::MAX, 0.0f32), |(lo, hi), &i| (lo.min(i), hi.max(i)));
            hi / lo
        };
        for px in [32.0, 34.0, 42.5, 51.0] {
            let mut r = Renderer::from_system(px, Theme::default()).unwrap();
            r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
            r.seal_admitted_font_sources();
            let (cw, chh) = r.cell_size();
            let (mut drawn, mut face) = (Vec::new(), Vec::new());
            for ch in SPINNER_STARS {
                let key = r.glyph_key(ch);
                assert_ne!(key.source, FaceId::Procedural, "{ch} at {px} px");
                let own = r.fitted_fallback_regular(key, ch).5;
                let img = r.glyph_image(key).clone();
                let top = r.baseline() - img.height() as i32 - img.ymin();
                assert!(
                    img.xmin() >= 0
                        && img.xmin() as usize + img.width() <= cw
                        && top >= 0
                        && top as usize + img.height() <= chh,
                    "{ch} at {px} px: leaves its {cw}x{chh} cell"
                );
                assert_eq!(img.bytes().iter().max(), Some(&255), "{ch} at {px} px");
                // Thinned or thickened, a frame keeps solid strokes: at least
                // a seventh of its inked texels (✽ at 32 px is the least, 16%,
                // 21% at 200%; the hairline ✳ is never solid).
                if ch != '✳' {
                    let inked = img.bytes().iter().filter(|&&c| c > 0).count();
                    let solid = img.bytes().iter().filter(|&&c| c >= 230).count();
                    assert!(
                        7 * solid >= inked,
                        "{ch} at {px} px: {solid} of {inked} solid"
                    );
                }
                if ch == SPINNER_REST {
                    assert_eq!(img.bytes(), own, "{ch} at {px} px: the face's own");
                }
                drawn.push(ink(img.bytes()));
                face.push(ink(&own));
            }
            assert!(
                spread(&drawn) <= SPINNER_FRAME_SPREAD + 0.01,
                "{px} px: frame inks {drawn:?}"
            );
            assert!(spread(&face) >= 2.0, "control at {px} px: {face:?}");
            if px == 34.0 {
                let key = r.glyph_key('*');
                let star = ink(r.glyph_image(key).bytes());
                // ✢ * ✶ ✻ ✽: the frames at 0, 2, 3 and 4.
                let cycle = [drawn[0], star, drawn[2], drawn[3], drawn[4]];
                assert!(spread(&cycle) <= 1.5, "Claude Code's cycle {cycle:?}");
            }
        }
    }

    /// The VISIBLE ink rows `[top, bottom)` of `ch` in `style` in its cell (a
    /// row whose strongest pixel reaches 1/8 coverage), and their centre.
    #[cfg(all(windows, feature = "embedded-font"))]
    fn ink_band(r: &mut Renderer, ch: char, style: crate::StyleBits) -> (i32, i32, f32) {
        let baseline = r.baseline();
        let key = r.glyph_key_styled(ch, style);
        let img = r.glyph_image(key).clone();
        let (w, h) = (img.width(), img.height());
        let seen = |y: &usize| img.bytes()[y * w..(y + 1) * w].iter().any(|&a| a >= 32);
        let first = (0..h).find(seen).expect("ink");
        let last = (0..h).rev().find(seen).expect("ink");
        let top = baseline - (img.ymin() + i32::try_from(h).unwrap());
        let (a, b) = (
            top + i32::try_from(first).unwrap(),
            top + i32::try_from(last).unwrap() + 1,
        );
        (a, b, (a + b) as f32 / 2.0)
    }

    /// CLAUDE CODE'S SPINNER STAYS LEVEL. The Windows default draws its `*`
    /// as Iosevka's own hex-low asterisk ([`lowered_asterisk`]), on the
    /// operator line, so `· ✢ * ✶ ✻ ✽` no longer hops up on its `*` frame:
    /// at 17 px the `*` is centred within half a pixel of `·`, `+` and `✻`,
    /// Claude Code's resting frame, where it sat 5 px above them. At every
    /// size from 8 to 51 px its ink is centred within a pixel of the same
    /// style's `+` in the upright faces, and within a pixel and a half in the
    /// italics. Control: Iosevka Term (Regular), the opt-in set, keeps its
    /// raised `*`, at least 4 px above its `+` at 17 px.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_windows_defaults_asterisk_sits_level_with_the_spinner_frames() {
        use crate::StyleBits;
        let mut r = windows_default_sealed();
        let (_, _, star) = ink_band(&mut r, '*', StyleBits::REGULAR);
        for ch in ['·', '+', '✻'] {
            let (_, _, level) = ink_band(&mut r, ch, StyleBits::REGULAR);
            assert!(
                (star - level).abs() <= 0.5,
                "17 px: `*` centred on row {star}, `{ch}` on {level}"
            );
        }
        let mut off = Vec::new();
        for px in (8..=51u8).map(f32::from) {
            let mut r = Renderer::from_system(px, Theme::default()).unwrap();
            r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
            r.seal_admitted_font_sources();
            for style in STYLES {
                let (a, b, star) = ink_band(&mut r, '*', style);
                let (c, d, plus) = ink_band(&mut r, '+', style);
                let bound = if style.contains(StyleBits::ITALIC) {
                    1.5
                } else {
                    1.0
                };
                if (star - plus).abs() > bound {
                    off.push(format!("{px} px {style:?}: `*` {a}..{b}, `+` {c}..{d}"));
                }
            }
        }
        assert!(off.is_empty(), "{off:#?}");
        let mut control = Renderer::from_bytes(
            face_for_id(IOSEVKA_ID).unwrap(),
            WINDOWS_DEFAULT_PX,
            Theme::default(),
        )
        .unwrap();
        control.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
        control.seal_admitted_font_sources();
        let (_, _, star) = ink_band(&mut control, '*', StyleBits::REGULAR);
        let (_, _, plus) = ink_band(&mut control, '+', StyleBits::REGULAR);
        assert!(
            plus - star >= 4.0,
            "CONTROL: Iosevka Term (Regular)'s `*` (row {star}) is not raised above its `+` \
             ({plus}), so this test cannot see the hop it guards against"
        );
    }

    /// The four styles, in the order of [`IOSEVKA_SEMIBOLD_FACES`]' slots.
    #[cfg(all(windows, feature = "embedded-font"))]
    const STYLES: [crate::StyleBits; 4] = [
        crate::StyleBits::REGULAR,
        crate::StyleBits::BOLD,
        crate::StyleBits::ITALIC,
        crate::StyleBits(crate::StyleBits::BOLD.0 | crate::StyleBits::ITALIC.0),
    ];

    /// THE FACE'S OWN ASTERISK, MOVED. Only the four Iosevka Term Semibold
    /// faces lower their `*` ([`lowered_asterisk`]), by contents, a copy
    /// too; the opt-in Iosevka Term (Regular) set, Cascadia Mono PL,
    /// JetBrains Mono and DejaVu Sans Mono keep theirs. At 17 px the `*` of
    /// each style is that face's own hinted raster, byte for byte, 5 rows
    /// lower: where the font's own hex-low (`VSAB=3` of the release faces,
    /// hinted through aterm on 2026-10-18) is centred, to half a pixel on the
    /// italics. An upright face never moves it sideways; an italic moves it
    /// left, along its slant, by at most a pixel.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn only_the_semibold_faces_lower_their_own_asterisk() {
        use crate::hinted::{HintBank, HintMode, hinted_glyph_raster};
        let mut r = windows_default_sealed();
        for (id, style) in IOSEVKA_SEMIBOLD_FACES.into_iter().zip(STYLES) {
            let bytes = face_for_id(id).unwrap();
            let lowered = lowered_asterisk(bytes).unwrap_or_else(|| panic!("{id}"));
            let copy = bytes.to_vec();
            assert_eq!(lowered_asterisk(&copy), Some(lowered), "{id}: a copy");
            let face = aterm_font::Face::parse(bytes, 0).unwrap();
            assert_eq!(
                face.glyph_index('*').map(|g| g.0),
                Some(lowered.gid),
                "{id}"
            );
            let inst = HintBank::default()
                .instance(1, bytes, 0, WINDOWS_DEFAULT_PX, HintMode::Full, &[], 0)
                .unwrap();
            let (w, h, xmin, ymin, _, own) =
                hinted_glyph_raster(bytes, 0, lowered.gid, WINDOWS_DEFAULT_PX, &inst, &[]).unwrap();
            let key = r.glyph_key_styled('*', style);
            let img = r.glyph_image(key).clone();
            assert_eq!(
                (img.width(), img.height(), img.bytes()),
                (w, h, own.as_slice()),
                "{id}: the face's own raster"
            );
            assert_eq!(img.ymin(), ymin - 5, "{id}: 5 rows lower");
            let left = xmin - img.xmin();
            let reach = if face.is_italic() { 0..=1 } else { 0..=0 };
            assert!(reach.contains(&left), "{id}: moved {left} px left");
        }
        for id in IOSEVKA_FACES
            .iter()
            .chain(&CASCADIA_FACES)
            .chain(&JETBRAINS_FACES)
            .chain(&[DEJAVU_ID])
        {
            let bytes = face_for_id(id).unwrap();
            assert_eq!(lowered_asterisk(bytes), None, "{id}");
        }
    }
    /// CJK BESIDE THE SEMIBOLD LATIN IS BOLD. The Windows default (Iosevka
    /// Term Semibold, weight 600) draws Han and kana from Microsoft YaHei
    /// Bold and Hangul from Malgun Gothic Bold, the cuts nearest its weight
    /// (`weight_matched_fallback_paths`), so `確認` is set in strokes as solid
    /// as the Latin beside it; a Regular primary (Cascadia Mono PL, by name)
    /// keeps the Regular cuts. Measured: the Regular YaHei drew `一` two rows
    /// thick and the Bold three, and `中` 110 ink texels against 166.
    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_windows_defaults_cjk_comes_in_the_cut_nearest_its_weight() {
        let fonts = std::path::Path::new("C:\\Windows\\Fonts");
        let needed = ["msyh.ttc", "msyhbd.ttc", "malgun.ttf", "malgunbd.ttf"];
        if let Some(absent) = needed.iter().find(|f| !fonts.join(f).is_file()) {
            aterm_spec::host::could_not_run(format!("{absent} is not installed here"));
        }
        let mut r = windows_default_sealed();
        let mut regular = cascadia_sealed(WINDOWS_DEFAULT_PX);
        regular.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
        for (ch, bold, plain) in [
            ('中', "msyhbd.ttc", "msyh.ttc"),
            ('確', "msyhbd.ttc", "msyh.ttc"),
            ('つ', "msyhbd.ttc", "msyh.ttc"),
            ('한', "malgunbd.ttf", "malgun.ttf"),
        ] {
            let got = r.debug_fallback_pick_path(ch).unwrap_or_default();
            assert!(got.ends_with(bold), "{ch}: drawn from {got}, not {bold}");
            let got = regular.debug_fallback_pick_path(ch).unwrap_or_default();
            assert!(
                got.ends_with(plain),
                "{ch} (Regular primary): drawn from {got}"
            );
            // Malgun Gothic's Bold is the nearer of its pair (150 texels for
            // 한 against 135); YaHei's at least a fifth heavier.
            let ((heavy, _), (light, _)) = (ink_of(&mut r, ch), ink_of(&mut regular, ch));
            let gain = if bold.starts_with("msyh") { 6 } else { 5 };
            assert!(
                5 * heavy > gain * light,
                "{ch}: the Bold cut inks {heavy} texels, the Regular {light}"
            );
        }
    }

    /// THE I CHING LINE SYMBOLS SIT AT MID-LINE. The embedded DejaVu Sans
    /// Mono maps only the monograms `⚊ ⚋`, as a bar on the baseline, so the
    /// bundled broad tier leaves the whole set to the symbol tier
    /// ([`is_yijing_line_symbol`]): no line symbol is drawn by it, and on
    /// Windows `⚊` (Noto Sans Symbols 2's) draws its bar about the middle of
    /// `☰`'s three (Segoe UI Symbol's) — not under them, where it drew a low
    /// underscore (measured 2026-10-17).
    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn the_i_ching_line_symbols_skip_the_baseline_bar_and_sit_at_mid_line() {
        let mut r = default_sealed();
        let dejavu = format!("fallback:{DEJAVU_ID}");
        for ch in ('\u{2630}'..='\u{2637}').chain('\u{268A}'..='\u{268F}') {
            let got = resolved_face(&mut r, ch);
            assert_ne!(got, dejavu, "{ch} U+{:04X}", ch as u32);
            assert_ne!(got, "NOTDEF", "{ch} U+{:04X}", ch as u32);
        }
        #[cfg(windows)]
        {
            // Baseline-relative ink rows (y up) of each.
            let rows = |r: &mut Renderer, ch: char| {
                let key = r.glyph_key(ch);
                let img = r.glyph_image(key);
                let (w, h) = (img.width(), img.height());
                let inked: Vec<i32> = (0..h)
                    .filter(|&y| img.bytes()[y * w..(y + 1) * w].iter().any(|&b| b > 40))
                    .map(|y| img.ymin() + (h - 1 - y) as i32)
                    .collect();
                (
                    inked[0].min(inked[inked.len() - 1]),
                    inked[0].max(inked[inked.len() - 1]),
                )
            };
            let (bar_lo, bar_hi) = rows(&mut r, '⚊');
            let (tri_lo, tri_hi) = rows(&mut r, '☰');
            let (bar, tri) = (bar_lo + bar_hi, tri_lo + tri_hi);
            assert!(
                (bar - tri).abs() <= 4,
                "⚊ rows {bar_lo}..={bar_hi} are not about the middle of ☰'s {tri_lo}..={tri_hi}"
            );
        }
    }

    /// THE OWNER'S ORDER puts the embedded Symbols Nerd Font AHEAD of Noto Sans
    /// Symbols 2 / Noto Sans Math. Its private-use icons were never contested;
    /// the six non-private-use points both carry (measured with fontTools:
    /// `⏻ ⏼ ⏽ ⏾ ☰ ⭘`) must not be drawn by a bundled Noto face. The contest
    /// is held under a primary that carries none of the six: the default on
    /// Linux; on Windows Cascadia Mono PL, since the default there, Iosevka
    /// Term Semibold, draws all six itself (a primary's own glyph comes
    /// first, as everywhere).
    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn the_nerd_font_precedes_the_bundled_noto_faces_where_both_draw() {
        #[cfg(windows)]
        let mut r = cascadia_sealed(18.0);
        #[cfg(not(windows))]
        let mut r = default_sealed();
        let contested = ['⏻', '⏼', '⏽', '⏾', '☰', '⭘'];
        for ch in contested {
            let noto_has = [NOTO_SYMBOLS2_ID, NOTO_MATH_ID].iter().any(|id| {
                aterm_font::Face::parse(face_for_id(id).unwrap(), 0)
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
