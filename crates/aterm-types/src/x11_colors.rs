// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! X11 named color lookup table for OSC color specifications.
//!
//! Provides case-insensitive lookup of the standard X11 `rgb.txt` color names
//! (140 named colors). Used by `ColorPalette::parse_color_spec` to support
//! named color arguments in OSC 4/10/11/12 sequences (e.g., `OSC 4;1;red ST`).
//!
//! Implementation: sorted array of `(&str, [u8; 3])` pairs with binary search.
//! All names are stored lowercase; input is lowercased before lookup. No heap
//! allocation is needed at lookup time.

use crate::Rgb;

/// The full X11 named color table (140 entries), sorted by lowercase name.
///
/// Values sourced from the X11 `rgb.txt` / CSS Color Module Level 4 named
/// color list. All names are lowercase for case-insensitive matching.
///
/// Note: "grey" variants are included alongside "gray" variants per X11
/// convention (both spellings map to the same RGB value).
#[rustfmt::skip]
const X11_COLORS: &[(&str, [u8; 3])] = &[
    ("aliceblue",            [240, 248, 255]),
    ("antiquewhite",         [250, 235, 215]),
    ("aqua",                 [  0, 255, 255]),
    ("aquamarine",           [127, 255, 212]),
    ("azure",                [240, 255, 255]),
    ("beige",                [245, 245, 220]),
    ("bisque",               [255, 228, 196]),
    ("black",                [  0,   0,   0]),
    ("blanchedalmond",       [255, 235, 205]),
    ("blue",                 [  0,   0, 255]),
    ("blueviolet",           [138,  43, 226]),
    ("brown",                [165,  42,  42]),
    ("burlywood",            [222, 184, 135]),
    ("cadetblue",            [ 95, 158, 160]),
    ("chartreuse",           [127, 255,   0]),
    ("chocolate",            [210, 105,  30]),
    ("coral",                [255, 127,  80]),
    ("cornflowerblue",       [100, 149, 237]),
    ("cornsilk",             [255, 248, 220]),
    ("crimson",              [220,  20,  60]),
    ("cyan",                 [  0, 255, 255]),
    ("darkblue",             [  0,   0, 139]),
    ("darkcyan",             [  0, 139, 139]),
    ("darkgoldenrod",        [184, 134,  11]),
    ("darkgray",             [169, 169, 169]),
    ("darkgreen",            [  0, 100,   0]),
    ("darkgrey",             [169, 169, 169]),
    ("darkkhaki",            [189, 183, 107]),
    ("darkmagenta",          [139,   0, 139]),
    ("darkolivegreen",       [ 85, 107,  47]),
    ("darkorange",           [255, 140,   0]),
    ("darkorchid",           [153,  50, 204]),
    ("darkred",              [139,   0,   0]),
    ("darksalmon",           [233, 150, 122]),
    ("darkseagreen",         [143, 188, 143]),
    ("darkslateblue",        [ 72,  61, 139]),
    ("darkslategray",        [ 47,  79,  79]),
    ("darkslategrey",        [ 47,  79,  79]),
    ("darkturquoise",        [  0, 206, 209]),
    ("darkviolet",           [148,   0, 211]),
    ("deeppink",             [255,  20, 147]),
    ("deepskyblue",          [  0, 191, 255]),
    ("dimgray",              [105, 105, 105]),
    ("dimgrey",              [105, 105, 105]),
    ("dodgerblue",           [ 30, 144, 255]),
    ("firebrick",            [178,  34,  34]),
    ("floralwhite",          [255, 250, 240]),
    ("forestgreen",          [ 34, 139,  34]),
    ("fuchsia",              [255,   0, 255]),
    ("gainsboro",            [220, 220, 220]),
    ("ghostwhite",           [248, 248, 255]),
    ("gold",                 [255, 215,   0]),
    ("goldenrod",            [218, 165,  32]),
    ("gray",                 [128, 128, 128]),
    ("green",                [  0, 128,   0]),
    ("greenyellow",          [173, 255,  47]),
    ("grey",                 [128, 128, 128]),
    ("honeydew",             [240, 255, 240]),
    ("hotpink",              [255, 105, 180]),
    ("indianred",            [205,  92,  92]),
    ("indigo",               [ 75,   0, 130]),
    ("ivory",                [255, 255, 240]),
    ("khaki",                [240, 230, 140]),
    ("lavender",             [230, 230, 250]),
    ("lavenderblush",        [255, 240, 245]),
    ("lawngreen",            [124, 252,   0]),
    ("lemonchiffon",         [255, 250, 205]),
    ("lightblue",            [173, 216, 230]),
    ("lightcoral",           [240, 128, 128]),
    ("lightcyan",            [224, 255, 255]),
    ("lightgoldenrodyellow", [250, 250, 210]),
    ("lightgray",            [211, 211, 211]),
    ("lightgreen",           [144, 238, 144]),
    ("lightgrey",            [211, 211, 211]),
    ("lightpink",            [255, 182, 193]),
    ("lightsalmon",          [255, 160, 122]),
    ("lightseagreen",        [ 32, 178, 170]),
    ("lightskyblue",         [135, 206, 250]),
    ("lightslategray",       [119, 136, 153]),
    ("lightslategrey",       [119, 136, 153]),
    ("lightsteelblue",       [176, 196, 222]),
    ("lightyellow",          [255, 255, 224]),
    ("lime",                 [  0, 255,   0]),
    ("limegreen",            [ 50, 205,  50]),
    ("linen",                [250, 240, 230]),
    ("magenta",              [255,   0, 255]),
    ("maroon",               [128,   0,   0]),
    ("mediumaquamarine",     [102, 205, 170]),
    ("mediumblue",           [  0,   0, 205]),
    ("mediumorchid",         [186,  85, 211]),
    ("mediumpurple",         [147, 112, 219]),
    ("mediumseagreen",       [ 60, 179, 113]),
    ("mediumslateblue",      [123, 104, 238]),
    ("mediumspringgreen",    [  0, 250, 154]),
    ("mediumturquoise",      [ 72, 209, 204]),
    ("mediumvioletred",      [199,  21, 133]),
    ("midnightblue",         [ 25,  25, 112]),
    ("mintcream",            [245, 255, 250]),
    ("mistyrose",            [255, 228, 225]),
    ("moccasin",             [255, 228, 181]),
    ("navajowhite",          [255, 222, 173]),
    ("navy",                 [  0,   0, 128]),
    ("oldlace",              [253, 245, 230]),
    ("olive",                [128, 128,   0]),
    ("olivedrab",            [107, 142,  35]),
    ("orange",               [255, 165,   0]),
    ("orangered",            [255,  69,   0]),
    ("orchid",               [218, 112, 214]),
    ("palegoldenrod",        [238, 232, 170]),
    ("palegreen",            [152, 251, 152]),
    ("paleturquoise",        [175, 238, 238]),
    ("palevioletred",        [219, 112, 147]),
    ("papayawhip",           [255, 239, 213]),
    ("peachpuff",            [255, 218, 185]),
    ("peru",                 [205, 133,  63]),
    ("pink",                 [255, 192, 203]),
    ("plum",                 [221, 160, 221]),
    ("powderblue",           [176, 224, 230]),
    ("purple",               [128,   0, 128]),
    ("rebeccapurple",        [102,  51, 153]),
    ("red",                  [255,   0,   0]),
    ("rosybrown",            [188, 143, 143]),
    ("royalblue",            [ 65, 105, 225]),
    ("saddlebrown",          [139,  69,  19]),
    ("salmon",               [250, 128, 114]),
    ("sandybrown",           [244, 164,  96]),
    ("seagreen",             [ 46, 139,  87]),
    ("seashell",             [255, 245, 238]),
    ("sienna",               [160,  82,  45]),
    ("silver",               [192, 192, 192]),
    ("skyblue",              [135, 206, 235]),
    ("slateblue",            [106,  90, 205]),
    ("slategray",            [112, 128, 144]),
    ("slategrey",            [112, 128, 144]),
    ("snow",                 [255, 250, 250]),
    ("springgreen",          [  0, 255, 127]),
    ("steelblue",            [ 70, 130, 180]),
    ("tan",                  [210, 180, 140]),
    ("teal",                 [  0, 128, 128]),
    ("thistle",              [216, 191, 216]),
    ("tomato",               [255,  99,  71]),
    ("turquoise",            [ 64, 224, 208]),
    ("violet",               [238, 130, 238]),
    ("wheat",                [245, 222, 179]),
    ("white",                [255, 255, 255]),
    ("whitesmoke",           [245, 245, 245]),
    ("yellow",               [255, 255,   0]),
    ("yellowgreen",          [154, 205,  50]),
];

/// Look up an X11 named color (case-insensitive).
///
/// Returns `Some(Rgb)` if the name matches one of the 140 standard X11/CSS
/// named colors, `None` otherwise. The lookup lowercases the input and uses
/// binary search on the sorted table.
// Skip: the table search closure drives absent std iterator bodies; a miss returns None.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn lookup(name: &str) -> Option<Rgb> {
    // Fast reject: X11 names are 3-20 chars, all ASCII.
    if name.is_empty() || name.len() > 24 || !name.is_ascii() {
        return None;
    }

    // Lowercase into a stack buffer to avoid heap allocation.
    // Longest X11 name is "lightgoldenrodyellow" (20 chars).
    let mut buf = [0u8; 24];
    let len = name.len();
    // Zip the fixed 24-byte buffer with the input so the write index is bounded
    // by the buffer (never > 24) — `name.len() <= 24` was checked above, so this
    // writes exactly `buf[..len]`, identical to the old indexed loop.
    for (slot, &b) in buf.iter_mut().zip(name.as_bytes()) {
        *slot = b.to_ascii_lowercase();
    }
    // Byte-wise key comparison: `&str` ordering IS the byte ordering of its
    // UTF-8 representation, so `binary_search_by` on `n.as_bytes()` visits the
    // same entries and returns the same result as the old `&str` search — the
    // strict `str::from_utf8` round-trip (a hardened utf8_reject boundary,
    // even though the buffer is provably ASCII here) drops out entirely.
    let key = buf.get(..len)?;

    X11_COLORS
        .binary_search_by(|&(n, _)| n.as_bytes().cmp(key))
        .ok()
        .and_then(|idx| {
            // `idx` comes from `binary_search`, always in range; the total `get`
            // discharges the slice bound without changing behaviour.
            let &(_, [r, g, b]) = X11_COLORS.get(idx)?;
            Some(Rgb::new(r, g, b))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted() {
        for window in X11_COLORS.windows(2) {
            assert!(
                window[0].0 < window[1].0,
                "X11 color table not sorted: {:?} >= {:?}",
                window[0].0,
                window[1].0
            );
        }
    }

    #[test]
    fn lookup_basic_colors() {
        assert_eq!(lookup("red"), Some(Rgb::new(255, 0, 0)));
        assert_eq!(lookup("green"), Some(Rgb::new(0, 128, 0)));
        assert_eq!(lookup("blue"), Some(Rgb::new(0, 0, 255)));
        assert_eq!(lookup("black"), Some(Rgb::new(0, 0, 0)));
        assert_eq!(lookup("white"), Some(Rgb::new(255, 255, 255)));
    }

    #[test]
    fn lookup_case_insensitive() {
        assert_eq!(lookup("Red"), Some(Rgb::new(255, 0, 0)));
        assert_eq!(lookup("RED"), Some(Rgb::new(255, 0, 0)));
        assert_eq!(lookup("DarkSlateGray"), Some(Rgb::new(47, 79, 79)));
        assert_eq!(lookup("DARKSLATEGRAY"), Some(Rgb::new(47, 79, 79)));
        assert_eq!(lookup("darkslategray"), Some(Rgb::new(47, 79, 79)));
    }

    #[test]
    fn lookup_grey_variants() {
        // Both "gray" and "grey" spellings should work
        assert_eq!(lookup("gray"), lookup("grey"));
        assert_eq!(lookup("darkgray"), lookup("darkgrey"));
        assert_eq!(lookup("lightgray"), lookup("lightgrey"));
        assert_eq!(lookup("slategray"), lookup("slategrey"));
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert_eq!(lookup("notacolor"), None);
        assert_eq!(lookup(""), None);
        assert_eq!(lookup("redd"), None);
    }

    #[test]
    fn lookup_non_ascii_returns_none() {
        assert_eq!(lookup("r\u{00e9}d"), None);
    }

    #[test]
    fn table_has_140_entries() {
        // The standard X11/CSS named color list has 148 entries
        // (140 unique colors + 8 grey/gray duplicates = 148 names).
        // We include all standard names.
        assert!(
            X11_COLORS.len() >= 140,
            "Expected at least 140 X11 colors, got {}",
            X11_COLORS.len()
        );
    }
}
