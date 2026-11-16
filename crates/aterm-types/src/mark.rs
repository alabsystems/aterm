// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The rules an agent's top-line mark obeys, shared by the marks aterm compiles
//! (`aterm_effects::cli_marks`, checked in a const block) and the marks a harness record carries
//! (`atpkg::harness_record`, checked when `atpkg-publish pack` copies the record and again when
//! aterm builds its registry).

/// How a master character paints in the icon palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Texel {
    /// Opaque icon ink `i`, contrast-floored.
    Ink(u8),
    /// Contrast-floored icon ink `i` at half alpha.
    Half(u8),
    /// Unfloored exact `0x00RRGGBB` hole colour.
    Hole(u32),
}

/// The most icon inks a mark may carry.
pub const MAX_ICON_INKS: usize = 49;

/// The longest side of a mark's drawing, in texels.
pub const MAX_MARK_SIDE: usize = 64;

/// The longest lockup word.
pub const MAX_MARK_WORD: usize = 24;

/// Whether the parts of a mark agree: one texel per master character; one to
/// [`MAX_ICON_INKS`] icon inks, as many on paper as on a dark ground; every texel's ink exists
/// and every hole is an RGB value; a mark of more than two inks paints character `k` in ink `k`;
/// a lockup word of 1 to [`MAX_MARK_WORD`] bytes whose name inks are one or one per letter (the
/// ALab word's too).
///
/// # Errors
/// The first rule broken.
pub const fn check(
    word: &str,
    alab_word: Option<&str>,
    name: [&[u32]; 2],
    icon: [&[u32]; 2],
    chars: &[u8],
    texels: &[Texel],
) -> Result<(), &'static str> {
    if texels.len() != chars.len() || chars.is_empty() {
        return Err("a mark has one texel per master character");
    }
    let inks = icon[0].len();
    if inks != icon[1].len() || inks == 0 || inks > MAX_ICON_INKS {
        return Err("a mark has 1 to 49 icon inks, as many on paper as on a dark ground");
    }
    let mut k = 0;
    while k < texels.len() {
        match texels[k] {
            Texel::Ink(i) | Texel::Half(i) => {
                if i as usize >= inks {
                    return Err("a texel names an icon ink the mark lacks");
                }
            }
            Texel::Hole(rgb) => {
                if rgb > 0x00FF_FFFF {
                    return Err("a hole is an RGB colour");
                }
            }
        }
        if inks > 2 && !matches!(texels[k], Texel::Ink(i) if i as usize == k) {
            return Err("a mark of more than two inks paints character k in ink k");
        }
        let c = chars[k];
        if c == b'.' || !c.is_ascii_graphic() {
            return Err("a master character is a printable ASCII byte other than '.'");
        }
        let mut j = 0;
        while j < k {
            if chars[j] == c {
                return Err("a master character is named twice");
            }
            j += 1;
        }
        k += 1;
    }
    if inks > 2 && texels.len() != inks {
        return Err("a mark of more than two inks has one character per ink");
    }
    if let Err(e) = word_inks(word, name) {
        return Err(e);
    }
    if let Some(alab) = alab_word
        && let Err(e) = word_inks(alab, name)
    {
        return Err(e);
    }
    Ok(())
}

/// A lockup word of 1 to [`MAX_MARK_WORD`] bytes, its inks one or one per letter on each ground.
const fn word_inks(word: &str, name: [&[u32]; 2]) -> Result<(), &'static str> {
    let w = word.len();
    if w == 0 || w > MAX_MARK_WORD {
        return Err("a lockup word is 1 to 24 bytes");
    }
    let mut g = 0;
    while g < name.len() {
        let n = name[g].len();
        if n != 1 && n != w {
            return Err("a lockup word's inks are one, or one per letter");
        }
        g += 1;
    }
    Ok(())
}

/// Whether `rows` is a drawing of `chars`: 1 to [`MAX_MARK_SIDE`] rows, each as wide as the
/// first (1 to [`MAX_MARK_SIDE`]), every byte `'.'` (transparent) or a master character.
///
/// # Errors
/// The first rule broken.
pub fn check_rows<S: AsRef<str>>(rows: &[S], chars: &[u8]) -> Result<(), &'static str> {
    let width = rows.first().map_or(0, |r| r.as_ref().len());
    if rows.is_empty() || rows.len() > MAX_MARK_SIDE || width == 0 || width > MAX_MARK_SIDE {
        return Err("a drawing is 1 to 64 rows of 1 to 64 texels");
    }
    for row in rows {
        let row = row.as_ref().as_bytes();
        if row.len() != width {
            return Err("every row of a drawing is as wide as the first");
        }
        if row.iter().any(|b| *b != b'.' && !chars.contains(b)) {
            return Err("a drawing uses only '.' and the master characters");
        }
    }
    Ok(())
}

/// A mark as a harness record spells it, owned: the parts [`check`] and [`check_rows`] judge.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MarkSpec {
    /// The lockup word.
    pub word: String,
    /// The ALab lockup word, if distinct.
    pub alab_word: Option<String>,
    /// Name inks on a dark ground, then on paper.
    pub name: [Vec<u32>; 2],
    /// Icon inks on a dark ground, then on paper.
    pub icon: [Vec<u32>; 2],
    /// Master characters: `chars[k]` paints `texels[k]`.
    pub chars: Vec<u8>,
    /// What each master character paints.
    pub texels: Vec<Texel>,
    /// The standard-definition drawing.
    pub sd: Vec<String>,
    /// The high-definition drawing, if any (twice the design scale).
    pub hd: Option<Vec<String>>,
}

impl MarkSpec {
    /// [`check`] and [`check_rows`] over every drawing.
    ///
    /// # Errors
    /// The first rule broken.
    pub fn check(&self) -> Result<(), &'static str> {
        check(
            &self.word,
            self.alab_word.as_deref(),
            [&self.name[0], &self.name[1]],
            [&self.icon[0], &self.icon[1]],
            &self.chars,
            &self.texels,
        )?;
        check_rows(&self.sd, &self.chars)?;
        if let Some(hd) = &self.hd {
            check_rows(hd, &self.chars)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "mark_tests.rs"]
mod tests;
