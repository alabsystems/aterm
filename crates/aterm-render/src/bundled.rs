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

/// Whether this build compiles in the Iosevka Term SEMIBOLD faces, the Windows default.
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
pub const PRIMARY_FAMILY: &str = IOSEVKA_SEMIBOLD_FAMILY;
/// The bundled primary's family name, as a user types it.
#[cfg(not(windows))]
pub const PRIMARY_FAMILY: &str = JETBRAINS_FAMILY;
/// The bundled primary's virtual identity (regular face).
#[cfg(windows)]
pub const PRIMARY_ID: &str = IOSEVKA_SEMIBOLD_ID;
/// The bundled primary's virtual identity (regular face).
#[cfg(not(windows))]
pub const PRIMARY_ID: &str = JETBRAINS_ID;
/// The bundled primary's bold face identity.
#[cfg(windows)]
pub const PRIMARY_BOLD_ID: &str = IOSEVKA_SEMIBOLD_BOLD_ID;
/// The bundled primary's bold face identity.
#[cfg(not(windows))]
pub const PRIMARY_BOLD_ID: &str = JETBRAINS_BOLD_ID;
/// The bundled primary's italic face identity.
#[cfg(windows)]
pub const PRIMARY_ITALIC_ID: &str = IOSEVKA_SEMIBOLD_ITALIC_ID;
/// The bundled primary's italic face identity.
#[cfg(not(windows))]
pub const PRIMARY_ITALIC_ID: &str = JETBRAINS_ITALIC_ID;
/// The bundled primary's bold-italic face identity.
#[cfg(windows)]
pub const PRIMARY_BOLD_ITALIC_ID: &str = IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID;
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
const IOSEVKA_SEMIBOLD_FACES: [&str; 4] = [
    IOSEVKA_SEMIBOLD_ID,
    IOSEVKA_SEMIBOLD_BOLD_ID,
    IOSEVKA_SEMIBOLD_ITALIC_ID,
    IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID,
];

const PRIMARY_FAMILIES: [[&str; 4]; 4] = [
    CASCADIA_FACES,
    JETBRAINS_FACES,
    IOSEVKA_FACES,
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
    is_iosevka_semibold(bytes)
}

/// THE GRID OWNS THE COLUMNS, the other way round.
#[inline]
#[must_use]
pub fn yields_wide_characters(bytes: &[u8]) -> bool {
    is_iosevka_semibold(bytes)
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
    is_iosevka_semibold(bytes)
}

/// The text size.
pub const SPINNER_STARS_CRISP_PX: f32 = 32.0;

/// The most one of Claude Code's spinner frames may outweigh another where a face draws them ([`SPINNER_STARS_CRISP_PX`] up).
pub const SPINNER_FRAME_SPREAD: f32 = 1.4;

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

fn is_iosevka_semibold(bytes: &[u8]) -> bool {
    IOSEVKA_SEMIBOLD_FACES
        .iter()
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
        assert!(face_for_family("Iosevka").is_none());
        assert!(face_for_family("Iosevka Term Medium").is_none());
        assert!(face_for_family("DejaVu Sans Mono").is_none());
        assert!(face_for_family("JetBrains Mono NL").is_none());
        assert!(face_for_family("Cascadia Mono").is_none());
        assert!(face_for_family("Cascadia Code").is_none());
        assert!(face_for_family("Cascadia Code PL").is_none());
        assert!(face_for_family("").is_none());
    }

    #[test]
    fn the_letter_floor_is_the_familys_deepest_latin_letter() {
        let per_em = |id: &str| letter_descent_floor(face_for_id(id).unwrap());
        if IOSEVKA_ACTIVE {
            for id in IOSEVKA_FACES {
                assert_eq!(per_em(id), Some(-0.256), "{id}");
            }
        }
        if IOSEVKA_SEMIBOLD_ACTIVE {
            for id in IOSEVKA_SEMIBOLD_FACES {
                assert_eq!(per_em(id), Some(-0.259), "{id}");
            }
        }
        if !ACTIVE {
            #[cfg(feature = "embedded-font")]
            assert!(letter_descent_floor(crate::embedded_font()).is_none());
            return;
        }
        for id in JETBRAINS_FACES {
            assert_eq!(per_em(id), Some(-0.24), "{id}");
        }
        if CASCADIA_ACTIVE {
            for id in CASCADIA_FACES {
                assert_eq!(per_em(id), Some(-608.0 / 2048.0), "{id}");
            }
        }
        let copy = face_for_id(PRIMARY_ID).unwrap().to_vec();
        assert_eq!(
            letter_descent_floor(&copy),
            letter_descent_floor(face_for_id(PRIMARY_ID).unwrap())
        );
        assert!(letter_descent_floor(face_for_id(DEJAVU_ID).unwrap()).is_none());
        assert!(letter_descent_floor(face_for_id(NOTO_MATH_ID).unwrap()).is_none());
    }

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

    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    use crate::{FaceId, GlyphClass};
    #[cfg(feature = "embedded-font")]
    use crate::{Renderer, Theme};

    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn default_sealed() -> Renderer {
        let mut r = Renderer::from_system(18.0, Theme::default()).expect("a default face");
        r.seal_admitted_font_sources();
        r
    }

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
        let candidates = crate::primary_font_candidate_paths();
        assert_eq!(candidates.first().map(String::as_str), Some(expected));
    }

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

    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    const PRIVATE_FONT_HOME: &str = "ATERM_RENDER_TEST_PRIVATE_FONT_HOME";

    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn in_private_font_home(test: &str) -> bool {
        if std::env::var_os(PRIVATE_FONT_HOME).is_some() {
            return true;
        }
        let module = module_path!();
        let path = module
            .split_once("::")
            .map_or_else(|| test.to_owned(), |(_, inner)| format!("{inner}::{test}"));
        let home = std::env::temp_dir().join(format!(
            "aterm-render-font-home-{}-{test}",
            std::process::id()
        ));
        std::fs::create_dir(&home).expect("a fresh empty HOME for the child");
        let out = aterm_uds::spawnfd::command(
            std::env::current_exe().expect("the test binary's own path"),
        )
        .args(["--exact", "--nocapture", "--test-threads=1", &path])
        .env(PRIVATE_FONT_HOME, "1")
        .env("HOME", &home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_DATA_DIRS")
        .output()
        .expect("re-running this test binary on one test");
        let _ = std::fs::remove_dir_all(&home);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            assert!(
                stdout.contains("1 passed"),
                "the child ran no test named {path}\nstdout: {stdout}\nstderr: {stderr}"
            );
            return false;
        }
        let why = stderr.lines().chain(stdout.lines()).find_map(|line| {
            let (_, rest) = line.split_once(aterm_spec::host::COULD_NOT_RUN)?;
            Some(rest.trim_start_matches(|c: char| c == '—' || c.is_whitespace()))
        });
        if let Some(why) = why {
            aterm_spec::host::could_not_run(format!("{why} ({path}, with an empty HOME)"));
        }
        panic!("{path} failed with an empty HOME\nstdout: {stdout}\nstderr: {stderr}");
    }

    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn bundled_family_resolves_by_name(
        test: &str,
        family: &str,
        id: &str,
        bold_id: &str,
        cell: (usize, usize),
    ) {
        if !in_private_font_home(test) {
            return;
        }
        if crate::resolve_font_family(family, crate::FaceTarget::REGULAR).is_some() {
            aterm_spec::host::could_not_run(format!(
                "{family} is installed in this host's system font roots, and an installed \
                 copy wins the name"
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

    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn jetbrains_mono_resolves_by_name() {
        bundled_family_resolves_by_name(
            "jetbrains_mono_resolves_by_name",
            JETBRAINS_FAMILY,
            JETBRAINS_ID,
            JETBRAINS_BOLD_ID,
            (10, 22),
        );
    }

    #[test]
    #[cfg(all(feature = "embedded-font", windows))]
    fn cascadia_mono_pl_resolves_by_name() {
        bundled_family_resolves_by_name(
            "cascadia_mono_pl_resolves_by_name",
            CASCADIA_FAMILY,
            CASCADIA_ID,
            CASCADIA_BOLD_ID,
            (9, 21),
        );
    }

    #[test]
    #[cfg(all(feature = "embedded-font", windows))]
    fn iosevka_term_semibold_resolves_by_name() {
        bundled_family_resolves_by_name(
            "iosevka_term_semibold_resolves_by_name",
            IOSEVKA_SEMIBOLD_FAMILY,
            IOSEVKA_SEMIBOLD_ID,
            IOSEVKA_SEMIBOLD_BOLD_ID,
            (8, 20),
        );
    }

    #[test]
    #[cfg(feature = "embedded-font")]
    fn iosevka_term_is_selectable_on_every_platform() {
        assert!(selectable_families().contains(&IOSEVKA_FAMILY));
        assert!(crate::list_fonts().iter().any(|f| f == IOSEVKA_FAMILY));
        for id in IOSEVKA_FACES {
            assert!(!yields_private_use(face_for_id(id).unwrap()), "{id}");
        }
    }

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

    #[cfg(windows)]
    const WINDOWS_DEFAULT_PX: f32 = 17.0;
    #[cfg(windows)]
    const WINDOWS_DEFAULT_LINE_HEIGHT: f32 = 1.1;

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

    #[cfg(all(feature = "embedded-font", windows))]
    fn cascadia(px: f32) -> Renderer {
        let r = Renderer::from_system_with_family(Some(CASCADIA_ID), px, Theme::default())
            .expect("the bundled Cascadia Mono PL");
        assert_eq!(r.primary_source_path(), Some(CASCADIA_ID));
        r
    }

    #[cfg(all(feature = "embedded-font", windows))]
    fn cascadia_sealed(px: f32) -> Renderer {
        let mut r = cascadia(px);
        r.seal_admitted_font_sources();
        r
    }

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
            cell_h + highest - lowest
        };
        let clear = clear_rows(default_px, default_line_height);
        assert!(
            clear >= 1,
            "{default_px} px on a {default_line_height} line: {clear} clear rows between a \
             row's descenders and the accents of the row under it"
        );
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
        let expected = if cfg!(windows) {
            [(800, false), (600, true), (800, true)]
        } else {
            [(700, false), (400, true), (700, true)]
        };
        assert_eq!(styles, expected);
        for (slot, id) in
            r.styled_faces
                .iter()
                .zip([PRIMARY_BOLD_ID, PRIMARY_ITALIC_ID, PRIMARY_BOLD_ITALIC_ID])
        {
            let face = slot.as_ref().expect("slot filled");
            assert_eq!(&face.bytes[..], face_for_id(id).unwrap(), "{id}");
        }
    }

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

    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn the_test_set_resolves_in_the_declared_order_and_never_to_notdef() {
        let mut r = default_sealed();
        let primary = format!("primary:{PRIMARY_ID}");
        let dejavu = format!("fallback:{DEJAVU_ID}");
        let sym2 = format!("symbol:{NOTO_SYMBOLS2_ID}");
        let math = format!("symbol:{NOTO_MATH_ID}");
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
            ('◐', &dejavu),
            ('✔', &dejavu),
            ('✘', &dejavu),
            ('↵', &dejavu),
            ('★', &dejavu),
            ('⚙', &dejavu),
            ('⏴', &sym2),
            ('⏵', &sym2),
            ('⏶', &sym2),
            ('⏷', &sym2),
            ('⏺', &sym2),
            ('⏸', &sym2),
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
        for ch in ['⎿', '⏩', '⠀', '⣿'] {
            let got = resolved_face(&mut r, ch);
            assert_ne!(got, "NOTDEF", "{ch} U+{:04X} is tofu", ch as u32);
            assert!(
                !got.starts_with("fallback:bundled") && !got.starts_with("symbol:bundled"),
                "{ch}: an incidental bundled pick ({got})"
            );
        }
    }

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

    #[test]
    fn exactly_the_cascadia_mono_pl_faces_yield_their_private_use() {
        let mut yielding = Vec::new();
        for &id in ALL_IDS {
            let Some(bytes) = face_for_id(id) else {
                continue;
            };
            let yields = yields_private_use(bytes);
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
        if let Some(bytes) = face_for_id(CASCADIA_ID) {
            let mut other = bytes.to_vec();
            let last = other.len() - 1;
            other[last] ^= 0xFF;
            assert!(!yields_private_use(&other));
        }
    }

    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_yielded_powerline_glyphs_draw_inside_their_cell_at_16_px() {
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
        r.set_primary_font(&cascadia).unwrap();
        assert_eq!(r.glyph_key(branch).source, FaceId::RuntimeFallback);
        r.set_primary_font(jbm).unwrap();
        assert_eq!(r.glyph_key(branch).source, FaceId::Primary);
    }

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

    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn a_yielded_point_breaks_the_shaping_run() {
        use crate::ColumnGlyph;
        use crate::ligature_shaping::{RowShaper, ShapedRun, build_feature_list, engine_features};
        use aterm_core::terminal::Terminal;
        let text = "a\u{E0A0}\u{E0A2}b";
        let cols: u16 = 8;
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

    #[test]
    #[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
    fn a_bundled_chain_face_never_takes_an_emoji_default_or_private_use_point() {
        let mut r = default_sealed();
        let mut checked = 0usize;
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
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/DejaVuSansMono.ttf");
        assert_eq!(
            &crate::read_resolved_font(path).unwrap()[..],
            &std::fs::read(path).unwrap()[..]
        );
    }

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
            let copy = bytes.to_vec();
            assert_eq!(fits_two_cell_glyphs(&copy), fits, "{id}: a copy");
        }
    }

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

    const IOSEVKA_WIDE_CHARACTERS: [char; 38] = [
        '\u{231A}', '\u{231B}', '\u{2329}', '\u{232A}', '\u{23E9}', '\u{23EA}', '\u{23EB}',
        '\u{23EC}', '\u{23F0}', '\u{23F3}', '\u{25FD}', '\u{25FE}', '\u{2630}', '\u{2631}',
        '\u{2632}', '\u{2633}', '\u{2634}', '\u{2635}', '\u{2636}', '\u{2637}', '\u{268A}',
        '\u{268B}', '\u{268C}', '\u{268D}', '\u{268E}', '\u{268F}', '\u{26A1}', '\u{26AA}',
        '\u{26AB}', '\u{2705}', '\u{274C}', '\u{274E}', '\u{27B0}', '\u{27BF}', '\u{2B1B}',
        '\u{2B1C}', '\u{2B50}', '\u{2B55}',
    ];

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
            let copy = bytes.to_vec();
            assert_eq!(yields_wide_characters(&copy), yields, "{id}: a copy");
        }
    }

    #[test]
    #[cfg(all(windows, feature = "embedded-font"))]
    fn the_windows_defaults_wide_characters_draw_centred_across_their_two_columns() {
        use aterm_core::terminal::Terminal;
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

    #[cfg(all(windows, feature = "embedded-font"))]
    fn ink_of(r: &mut Renderer, ch: char) -> (usize, usize) {
        let key = r.glyph_key(ch);
        let img = r.glyph_image(key);
        let w = img.width();
        let inked = |y: usize| img.bytes()[y * w..(y + 1) * w].iter().any(|&b| b > 40);
        let rows = (0..img.height()).filter(|&y| inked(y)).count();
        (img.bytes().iter().filter(|&&b| b > 40).count(), rows)
    }

    #[cfg(all(windows, feature = "embedded-font"))]
    fn windows_default_sealed() -> Renderer {
        let mut r = Renderer::from_system(WINDOWS_DEFAULT_PX, Theme::default()).unwrap();
        r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
        r.seal_admitted_font_sources();
        assert_eq!(r.primary_source_path(), Some(IOSEVKA_SEMIBOLD_ID));
        r
    }

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
                let cycle = [drawn[0], star, drawn[2], drawn[3], drawn[4]];
                assert!(spread(&cycle) <= 1.5, "Claude Code's cycle {cycle:?}");
            }
        }
    }

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

    #[cfg(all(windows, feature = "embedded-font"))]
    const STYLES: [crate::StyleBits; 4] = [
        crate::StyleBits::REGULAR,
        crate::StyleBits::BOLD,
        crate::StyleBits::ITALIC,
        crate::StyleBits(crate::StyleBits::BOLD.0 | crate::StyleBits::ITALIC.0),
    ];

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
            let ((heavy, _), (light, _)) = (ink_of(&mut r, ch), ink_of(&mut regular, ch));
            let gain = if bold.starts_with("msyh") { 6 } else { 5 };
            assert!(
                5 * heavy > gain * light,
                "{ch}: the Bold cut inks {heavy} texels, the Regular {light}"
            );
        }
    }

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
        assert_eq!(
            resolved_face(&mut r, '⏵'),
            format!("symbol:{NOTO_SYMBOLS2_ID}")
        );
    }
}
