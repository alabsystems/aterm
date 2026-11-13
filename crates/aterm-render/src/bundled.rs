// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bundled font discovery, glyph fallback chains, and embedded font assets.
/// Whether the bundled stack.
pub const ACTIVE: bool = cfg!(all(feature = "embedded-font", not(target_os = "macos")));

/// Whether this build compiles in the Cascadia Mono PL faces.
pub const CASCADIA_ACTIVE: bool = cfg!(all(feature = "embedded-font", windows));

/// The virtual identity scheme for bundled faces.
pub const SCHEME: &str = "bundled:";

/// JetBrains Mono's family name, as a user types it (name ID 1).
pub const JETBRAINS_FAMILY: &str = "JetBrains Mono";
/// The bundled JetBrains Mono's virtual identity (regular face).
pub const JETBRAINS_ID: &str = "bundled:JetBrains Mono";
/// The bundled JetBrains Mono's bold face identity.
pub const JETBRAINS_BOLD_ID: &str = "bundled:JetBrains Mono Bold";
/// The bundled JetBrains Mono's italic face identity.
pub const JETBRAINS_ITALIC_ID: &str = "bundled:JetBrains Mono Italic";
/// The bundled JetBrains Mono's bold-italic face identity.
pub const JETBRAINS_BOLD_ITALIC_ID: &str = "bundled:JetBrains Mono Bold Italic";

/// Cascadia Mono PL's family name, as a user types it (name ID 1 of all four static 2407.24 faces).
pub const CASCADIA_FAMILY: &str = "Cascadia Mono PL";
/// The bundled Cascadia Mono PL's virtual identity (regular face).
pub const CASCADIA_ID: &str = "bundled:Cascadia Mono PL";
/// The bundled Cascadia Mono PL's bold face identity.
pub const CASCADIA_BOLD_ID: &str = "bundled:Cascadia Mono PL Bold";
/// The bundled Cascadia Mono PL's italic face identity.
pub const CASCADIA_ITALIC_ID: &str = "bundled:Cascadia Mono PL Italic";
/// The bundled Cascadia Mono PL's bold-italic face identity.
pub const CASCADIA_BOLD_ITALIC_ID: &str = "bundled:Cascadia Mono PL Bold Italic";

/// Whether this build compiles in the Iosevka Term faces.
pub const IOSEVKA_ACTIVE: bool = cfg!(feature = "embedded-font");

/// Iosevka Term's family name, as a user types it (name ID 1 of all four faces).
pub const IOSEVKA_FAMILY: &str = "Iosevka Term";
/// The bundled Iosevka Term's virtual identity (regular face).
pub const IOSEVKA_ID: &str = "bundled:Iosevka Term";
/// The bundled Iosevka Term's bold face identity.
pub const IOSEVKA_BOLD_ID: &str = "bundled:Iosevka Term Bold";
/// The bundled Iosevka Term's italic face identity.
pub const IOSEVKA_ITALIC_ID: &str = "bundled:Iosevka Term Italic";
/// The bundled Iosevka Term's bold-italic face identity.
pub const IOSEVKA_BOLD_ITALIC_ID: &str = "bundled:Iosevka Term Bold Italic";

/// Whether this build compiles in the Iosevka Term NL faces, the Windows default.
pub const IOSEVKA_NL_ACTIVE: bool = cfg!(all(feature = "embedded-font", windows));

/// Iosevka Term NL's family name, as a user types it: Iosevka Term (the Regular set) with No programming Ligatures, cut from the bundled files at build time (`no_ligatures`), so `!=`, `->` and `>=` stay the characters a program wrote.
pub const IOSEVKA_NL_FAMILY: &str = "Iosevka Term NL";
/// The bundled Iosevka Term NL's virtual identity (regular face).
pub const IOSEVKA_NL_ID: &str = "bundled:Iosevka Term NL";
/// The bundled Iosevka Term NL's bold face identity.
pub const IOSEVKA_NL_BOLD_ID: &str = "bundled:Iosevka Term NL Bold";
/// The bundled Iosevka Term NL's italic face identity.
pub const IOSEVKA_NL_ITALIC_ID: &str = "bundled:Iosevka Term NL Italic";
/// The bundled Iosevka Term NL's bold-italic face identity.
pub const IOSEVKA_NL_BOLD_ITALIC_ID: &str = "bundled:Iosevka Term NL Bold Italic";

/// Whether this build compiles in the Iosevka Term SEMIBOLD faces (Windows only, where they were the default before Iosevka Term NL).
pub const IOSEVKA_SEMIBOLD_ACTIVE: bool = cfg!(all(feature = "embedded-font", windows));

/// Iosevka Term Semibold's family name, as a user types it.
pub const IOSEVKA_SEMIBOLD_FAMILY: &str = "Iosevka Term Semibold";
/// The bundled Iosevka Term Semibold's virtual identity.
pub const IOSEVKA_SEMIBOLD_ID: &str = "bundled:Iosevka Term Semibold";
/// The bundled Iosevka Term Semibold's BOLD slot: the ExtraBold face.
pub const IOSEVKA_SEMIBOLD_BOLD_ID: &str = "bundled:Iosevka Term Semibold Bold";
/// The bundled Iosevka Term Semibold's ITALIC slot: the SemiBold Italic face.
pub const IOSEVKA_SEMIBOLD_ITALIC_ID: &str = "bundled:Iosevka Term Semibold Italic";
/// The bundled Iosevka Term Semibold's BOLD-ITALIC slot.
pub const IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID: &str = "bundled:Iosevka Term Semibold Bold Italic";

/// The bundled PRIMARY's family name, as a user types it.
#[cfg(windows)]
pub const PRIMARY_FAMILY: &str = IOSEVKA_NL_FAMILY;
/// The bundled primary's family name, as a user types it.
#[cfg(not(windows))]
pub const PRIMARY_FAMILY: &str = JETBRAINS_FAMILY;
/// The bundled primary's virtual identity (regular face).
#[cfg(windows)]
pub const PRIMARY_ID: &str = IOSEVKA_NL_ID;
/// The bundled primary's virtual identity (regular face).
#[cfg(not(windows))]
pub const PRIMARY_ID: &str = JETBRAINS_ID;
/// The bundled primary's bold face identity.
#[cfg(windows)]
pub const PRIMARY_BOLD_ID: &str = IOSEVKA_NL_BOLD_ID;
/// The bundled primary's bold face identity.
#[cfg(not(windows))]
pub const PRIMARY_BOLD_ID: &str = JETBRAINS_BOLD_ID;
/// The bundled primary's italic face identity.
#[cfg(windows)]
pub const PRIMARY_ITALIC_ID: &str = IOSEVKA_NL_ITALIC_ID;
/// The bundled primary's italic face identity.
#[cfg(not(windows))]
pub const PRIMARY_ITALIC_ID: &str = JETBRAINS_ITALIC_ID;
/// The bundled primary's bold-italic face identity.
#[cfg(windows)]
pub const PRIMARY_BOLD_ITALIC_ID: &str = IOSEVKA_NL_BOLD_ITALIC_ID;
/// The bundled primary's bold-italic face identity.
#[cfg(not(windows))]
pub const PRIMARY_BOLD_ITALIC_ID: &str = JETBRAINS_BOLD_ITALIC_ID;

/// The embedded DejaVu Sans Mono, as a broad-tier chain entry.
pub const DEJAVU_ID: &str = "bundled:DejaVu Sans Mono";
/// Noto Sans Symbols 2, as a symbol-tier chain entry.
pub const NOTO_SYMBOLS2_ID: &str = "bundled:Noto Sans Symbols 2";
/// Noto Sans Math, as a symbol-tier chain entry.
pub const NOTO_MATH_ID: &str = "bundled:Noto Sans Math";

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
    IOSEVKA_NL_ID,
    IOSEVKA_NL_BOLD_ID,
    IOSEVKA_NL_ITALIC_ID,
    IOSEVKA_NL_BOLD_ITALIC_ID,
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
    pub(super) static IOSEVKA_REGULAR: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-Regular.ttf");
    pub(super) static IOSEVKA_BOLD: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-Bold.ttf");
    pub(super) static IOSEVKA_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-Italic.ttf");
    pub(super) static IOSEVKA_BOLD_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/IosevkaTerm-BoldItalic.ttf");
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
    #[cfg(windows)]
    pub(super) static IOSEVKA_NL_REGULAR: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/IosevkaTerm-Regular.nl.ttf"));
    #[cfg(windows)]
    pub(super) static IOSEVKA_NL_BOLD: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/IosevkaTerm-Bold.nl.ttf"));
    #[cfg(windows)]
    pub(super) static IOSEVKA_NL_ITALIC: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/IosevkaTerm-Italic.nl.ttf"));
    #[cfg(windows)]
    pub(super) static IOSEVKA_NL_BOLD_ITALIC: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/IosevkaTerm-BoldItalic.nl.ttf"));
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

/// The bytes behind one virtual identity, or `None` for anything that is not a bundled identity in this build.
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
        IOSEVKA_NL_ID => Some(bytes::IOSEVKA_NL_REGULAR),
        #[cfg(windows)]
        IOSEVKA_NL_BOLD_ID => Some(bytes::IOSEVKA_NL_BOLD),
        #[cfg(windows)]
        IOSEVKA_NL_ITALIC_ID => Some(bytes::IOSEVKA_NL_ITALIC),
        #[cfg(windows)]
        IOSEVKA_NL_BOLD_ITALIC_ID => Some(bytes::IOSEVKA_NL_BOLD_ITALIC),
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
#[inline]
#[must_use]
pub fn is_bundled_id(path: &str) -> bool {
    face_for_id(path).is_some()
}

/// Whether `family` is spelled as a bundled virtual identity (`bundled:…`) rather than a family name.
#[inline]
#[must_use]
pub fn is_identity(family: &str) -> bool {
    family.trim().starts_with(SCHEME)
}

/// Resolve a config-authored FAMILY request to a bundled face.
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
        "iosevkatermnl" | "iosevkatermnlregular" => IOSEVKA_NL_ID,
        #[cfg(windows)]
        "iosevkatermnlbold" => IOSEVKA_NL_BOLD_ID,
        #[cfg(windows)]
        "iosevkatermnlitalic" => IOSEVKA_NL_ITALIC_ID,
        #[cfg(windows)]
        "iosevkatermnlbolditalic" => IOSEVKA_NL_BOLD_ITALIC_ID,
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

/// The real `[bold, italic, bold-italic]` faces of a bundled primary, keyed by the primary's identity.
#[must_use]
pub fn styled_siblings(primary: &str) -> [Option<&'static [u8]>; 3] {
    let family = match primary {
        JETBRAINS_ID => JETBRAINS_FACES,
        #[cfg(windows)]
        CASCADIA_ID => CASCADIA_FACES,
        #[cfg(windows)]
        IOSEVKA_NL_ID => IOSEVKA_NL_FACES,
        #[cfg(windows)]
        IOSEVKA_SEMIBOLD_ID => IOSEVKA_SEMIBOLD_FACES,
        IOSEVKA_ID => IOSEVKA_FACES,
        _ => return [None; 3],
    };
    [family[1], family[2], family[3]].map(face_for_id)
}

const CASCADIA_FACES: [&str; 4] = [
    CASCADIA_ID,
    CASCADIA_BOLD_ID,
    CASCADIA_ITALIC_ID,
    CASCADIA_BOLD_ITALIC_ID,
];
const JETBRAINS_FACES: [&str; 4] = [
    JETBRAINS_ID,
    JETBRAINS_BOLD_ID,
    JETBRAINS_ITALIC_ID,
    JETBRAINS_BOLD_ITALIC_ID,
];
const IOSEVKA_FACES: [&str; 4] = [
    IOSEVKA_ID,
    IOSEVKA_BOLD_ID,
    IOSEVKA_ITALIC_ID,
    IOSEVKA_BOLD_ITALIC_ID,
];
const IOSEVKA_NL_FACES: [&str; 4] = [
    IOSEVKA_NL_ID,
    IOSEVKA_NL_BOLD_ID,
    IOSEVKA_NL_ITALIC_ID,
    IOSEVKA_NL_BOLD_ITALIC_ID,
];
const IOSEVKA_SEMIBOLD_FACES: [&str; 4] = [
    IOSEVKA_SEMIBOLD_ID,
    IOSEVKA_SEMIBOLD_BOLD_ID,
    IOSEVKA_SEMIBOLD_ITALIC_ID,
    IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID,
];

const PRIMARY_FAMILIES: [[&str; 4]; 5] = [
    CASCADIA_FACES,
    JETBRAINS_FACES,
    IOSEVKA_FACES,
    IOSEVKA_NL_FACES,
    IOSEVKA_SEMIBOLD_FACES,
];

/// When `bytes` is one of a bundled primary family's faces.
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

#[inline]
fn same_face(face: &[u8], bytes: &[u8]) -> bool {
    face.len() == bytes.len() && (std::ptr::eq(face.as_ptr(), bytes.as_ptr()) || face == bytes)
}

const PRIVATE_USE_YIELDING_IDS: [&str; 4] = CASCADIA_FACES;

/// Whether `bytes` are one of the four Cascadia Mono PL faces (Windows only), in whichever slot.
#[must_use]
pub fn yields_private_use(bytes: &[u8]) -> bool {
    PRIVATE_USE_YIELDING_IDS
        .iter()
        .any(|id| face_for_id(id).is_some_and(|face| same_face(face, bytes)))
}

/// The code points Iosevka Term draws TWO cells wide.
#[inline]
#[must_use]
pub const fn is_two_cell_glyph(ch: char) -> bool {
    matches!(
        ch,
        '\u{27DD}' | '\u{27DE}' | '\u{27F5}'..='\u{27FF}' | '\u{2B33}'
    )
}

/// THE GRID OWNS THE COLUMNS.
#[inline]
#[must_use]
pub fn fits_two_cell_glyphs(bytes: &[u8]) -> bool {
    is_grid_fitted_iosevka(bytes)
}

/// THE GRID OWNS THE COLUMNS, the other way round.
#[inline]
#[must_use]
pub fn yields_wide_characters(bytes: &[u8]) -> bool {
    is_grid_fitted_iosevka(bytes)
}

/// The I CHING LINE SYMBOLS.
#[inline]
#[must_use]
pub const fn is_yijing_line_symbol(ch: char) -> bool {
    matches!(ch, '\u{2630}'..='\u{2637}' | '\u{268A}'..='\u{268F}')
}

/// The SPINNER AND FOOTER DINGBATS.
#[inline]
#[must_use]
pub const fn is_small_dingbat(ch: char) -> bool {
    matches!(ch, '\u{2722}'..='\u{273D}' | '\u{23F4}'..='\u{23F7}')
}

/// Whether `bytes` are a bundled face that leaves the [`is_small_dingbat`] set to the fallback chain instead of drawing it itself.
#[inline]
#[must_use]
pub fn yields_small_dingbats(bytes: &[u8]) -> bool {
    is_grid_fitted_iosevka(bytes)
}

/// The text size.
pub const SPINNER_STARS_CRISP_PX: f32 = 32.0;

/// The most one of Claude Code's spinner frames may outweigh another where a face draws them ([`SPINNER_STARS_CRISP_PX`] up).
pub const SPINNER_FRAME_SPREAD: f32 = 1.4;

/// The most a face-drawn spinner frame may outweigh the primary's own `*`, which Claude Code's cycle shows between the frames and which is not redrawn: a frame evened past it comes down to it. The rest ([`crate::procedural::SPINNER_REST`]) stays as its face draws it (thinned, its strokes go soft), so under a `*` light enough that the rest itself lies past this, the heavier frames come down to the rest.
pub const SPINNER_CYCLE_SPREAD: f32 = 1.5;

/// How far Iosevka's own HEX-LOW asterisk sits below its default hex-high one, per em.
pub const HEX_LOW_DROP: f32 = 0.266;

/// How far the hex-low asterisk of an Iosevka ITALIC also moves left, per em.
pub const HEX_LOW_ITALIC_LEFT: f32 = 0.044;

/// THE ASTERISK SITS ON THE OPERATOR LINE.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoweredAsterisk {
    pub gid: u16,
    x_min: f32,
    y_mid: f32,
    dx: f32,
    dy: f32,
}

impl LoweredAsterisk {
    /// The move at `px`.
    #[inline]
    #[must_use]
    pub fn shift_px(&self, px: f32, inked_mid: f32) -> (i32, i32) {
        let x = ((self.x_min + self.dx) * px).round() as i32 - (self.x_min * px).round() as i32;
        let y = ((self.y_mid + self.dy) * px - inked_mid).round() as i32;
        (x, y)
    }
}

/// Whether `bytes` are a bundled face that draws its `*` as Iosevka's own HEX-LOW asterisk.
#[must_use]
pub fn lowered_asterisk(bytes: &[u8]) -> Option<LoweredAsterisk> {
    if !is_grid_fitted_iosevka(bytes) {
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

/// Whether `bytes` are a face of one of the Iosevka Term sets Windows has drawn by default, which the grid treatments above apply to: Iosevka Term NL and the Semibold set before it. Both are compiled in on Windows only; Iosevka Term by name, on any system, is drawn as the file is.
fn is_grid_fitted_iosevka(bytes: &[u8]) -> bool {
    IOSEVKA_NL_FACES
        .iter()
        .chain(&IOSEVKA_SEMIBOLD_FACES)
        .any(|id| face_for_id(id).is_some_and(|face| same_face(face, bytes)))
}

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

/// Default primary font candidate identities.
#[inline]
#[must_use]
pub const fn default_primary_candidates() -> &'static [&'static str] {
    if ACTIVE { &[PRIMARY_ID] } else { &[] }
}

/// The bundled broad-tier chain entries, after the configured `fallback_fonts` and ahead of every system discovery path.
#[inline]
#[must_use]
pub const fn broad_fallback_ids() -> &'static [&'static str] {
    if ACTIVE { &[DEJAVU_ID] } else { &[] }
}

/// The bundled symbol-tier chain entries, in order, placed after a configured `symbol_font` and ahead of the system symbol faces.
#[inline]
#[must_use]
pub const fn symbol_fallback_ids() -> &'static [&'static str] {
    if ACTIVE {
        &[NOTO_SYMBOLS2_ID, NOTO_MATH_ID]
    } else {
        &[]
    }
}

const SUPERSEDED_SYSTEM_FILES: &[&str] =
    &["NotoSansSymbols2-Regular.ttf", "NotoSansMath-Regular.ttf"];

/// Whether a built-in system candidate `path` names, by file name, a face the bundled stack already serves ahead of it.
#[must_use]
pub fn supersedes_system_face(path: &str) -> bool {
    ACTIVE
        && std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| SUPERSEDED_SYSTEM_FILES.contains(&n))
}

/// Families the bundled stack makes selectable by name (for `list-fonts`).
#[inline]
#[must_use]
pub const fn selectable_families() -> &'static [&'static str] {
    if !IOSEVKA_ACTIVE {
        return &[];
    }
    #[cfg(windows)]
    {
        &[
            IOSEVKA_NL_FAMILY,
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

use crate::normalize_family as normalize;

#[cfg(test)]
#[path = "bundled_tests.rs"]
mod tests;
