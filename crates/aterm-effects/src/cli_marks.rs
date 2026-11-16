// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Agent CLI top-line brand marks, icons, and contrast-floored palettes.

use std::sync::Arc;

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, PxRect,
};
use aterm_render::fx as fx_art;
use aterm_render::premul_rgb;

#[cfg(test)]
use crate::color_math::floor_contrast_keeping_hue;
#[cfg(test)]
use crate::harness_card::dark_ground;

/// Minimum WCAG contrast ratio for a CLI's name against its ground.
pub const NAME_CONTRAST: f32 = 4.5;

/// Minimum WCAG contrast ratio for non-hole icon texels against their ground.
pub const ICON_CONTRAST: f32 = 3.0;

/// Alpha value for half-tone texels ([`Texel::Half`]).
pub const HALF_ALPHA: u8 = 128;

/// Width of the icon box in terminal cells.
pub const ICON_CELLS: u16 = 2;

/// Vertical padding rows kept clear inside the icon box.
pub const AIR_PX: u16 = 2;

/// Total cell span of the icon run before the CLI name.
pub const ICON_RUN: usize = 2 + ICON_CELLS as usize;

/// Agent CLI that wears a brand mark on its top line: a handle to its [`Brand`], one of the
/// compiled seeds ([`CliMark::SEEDS`]) or a harness record's mark ([`CliMark::from_spec`]). Two
/// handles are equal when they name the same brand.
#[derive(Clone, Copy)]
pub struct CliMark(&'static Brand);

impl std::fmt::Debug for CliMark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CliMark({})", self.0.word)
    }
}

impl PartialEq for CliMark {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for CliMark {}

impl std::hash::Hash for CliMark {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.0, state);
    }
}

impl CliMark {
    /// Claude Code.
    pub const CLAUDE: CliMark = CliMark(&CLAUDE);
    /// OpenAI Codex (and `codex-alab`).
    pub const CODEX: CliMark = CliMark(&CODEX);
    /// Google Antigravity CLI.
    pub const AGY: CliMark = CliMark(&AGY);
    /// Meta Muse Code.
    pub const MUSE: CliMark = CliMark(&MUSE);
    /// xAI Grok.
    pub const GROK: CliMark = CliMark(&GROK);
    /// Moonshot Kimi Code.
    pub const KIMI: CliMark = CliMark(&KIMI);

    /// The compiled marks, the fallback for an agent whose harness record carries none, until
    /// each harness ships its own (`docs/HARNESS-CONTRACT.md` §2).
    pub const SEEDS: [CliMark; 6] = [
        CliMark::CLAUDE,
        CliMark::CODEX,
        CliMark::AGY,
        CliMark::MUSE,
        CliMark::GROK,
        CliMark::KIMI,
    ];

    /// The compiled mark whose lockup word is `word`.
    #[must_use]
    pub fn seed(word: &str) -> Option<Self> {
        Self::SEEDS.into_iter().find(|m| m.word() == word)
    }

    /// The mark a harness record spells, checked ([`MarkSpec::check`]) and interned: the same
    /// spec gives the same handle for the life of the process, so reading the registry again
    /// allocates nothing new.
    ///
    /// # Errors
    /// The rule the spec breaks.
    pub fn from_spec(spec: &MarkSpec) -> Result<Self, &'static str> {
        static INTERNED: std::sync::Mutex<Vec<(MarkSpec, CliMark)>> =
            std::sync::Mutex::new(Vec::new());
        spec.check()?;
        let mut interned = INTERNED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, mark)) = interned.iter().find(|(kept, _)| kept == spec) {
            return Ok(*mark);
        }
        fn leak<T: Clone>(v: &[T]) -> &'static [T] {
            Box::leak(v.to_vec().into_boxed_slice())
        }
        fn rows(v: &[String]) -> &'static [&'static str] {
            let rows: Vec<&'static str> = v
                .iter()
                .map(|r| &*Box::leak(r.clone().into_boxed_str()))
                .collect();
            Box::leak(rows.into_boxed_slice())
        }
        let brand: &'static Brand = Box::leak(Box::new(Brand {
            word: Box::leak(spec.word.clone().into_boxed_str()),
            alab_word: spec
                .alab_word
                .as_ref()
                .map(|w| &*Box::leak(w.clone().into_boxed_str())),
            name: Inks {
                dark: leak(&spec.name[0]),
                paper: leak(&spec.name[1]),
            },
            icon: Inks {
                dark: leak(&spec.icon[0]),
                paper: leak(&spec.icon[1]),
            },
            chars: leak(&spec.chars),
            texels: leak(&spec.texels),
            sd: rows(&spec.sd),
            hd: spec.hd.as_deref().map(rows),
        }));
        let mark = CliMark(brand);
        interned.push((spec.clone(), mark));
        Ok(mark)
    }

    /// The brand this mark draws.
    #[inline]
    #[must_use]
    pub const fn brand(self) -> &'static Brand {
        self.0
    }

    /// Canonical lowercase word for this CLI.
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        self.0.word
    }

    /// Display name printed by the lockup for `build`.
    #[inline]
    #[must_use]
    pub const fn name(self, build: Build) -> &'static str {
        match (build, self.0.alab_word) {
            (Build::Alab, Some(word)) => word,
            _ => self.word(),
        }
    }

    /// Total cell width of the lockup for `build`.
    #[inline]
    #[must_use]
    pub const fn lockup_cells(self, build: Build) -> usize {
        ICON_RUN + self.name(build).len() + 1
    }
}

/// Proven build provenance of the CLI running in a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Build {
    /// Vendor build (or unproven).
    #[default]
    Vendor,
    /// Proven ALab build.
    Alab,
}

impl Build {
    /// Both builds, vendor first.
    pub const ALL: [Build; 2] = [Build::Vendor, Build::Alab];
}

pub use aterm_types::mark::{MarkSpec, Texel};

/// Dark-ground and paper-ground ink lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inks {
    /// Inks on a dark ground.
    pub dark: &'static [u32],
    /// Inks on a paper ground.
    pub paper: &'static [u32],
}

impl Inks {
    /// The same inks on either polarity.
    #[inline]
    const fn both(inks: &'static [u32]) -> Self {
        Self {
            dark: inks,
            paper: inks,
        }
    }

    /// Select the ink list for ground polarity `dark`.
    #[inline]
    #[must_use]
    pub const fn on(self, dark: bool) -> &'static [u32] {
        if dark { self.dark } else { self.paper }
    }
}

/// Resolution tier of a mark drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Master {
    /// Standard-definition drawing (design scale 1).
    Sd,
    /// High-definition drawing (design scale 2).
    Hd,
}

impl Master {
    /// Design scale factor of this master.
    #[inline]
    #[must_use]
    pub const fn design_scale(self) -> u16 {
        match self {
            Master::Sd => 1,
            Master::Hd => 2,
        }
    }
}

/// Brand specification behind one [`CliMark`].
#[derive(Debug, Clone, Copy)]
pub struct Brand {
    /// Vendor lockup word.
    pub word: &'static str,
    /// ALab lockup word, if distinct.
    pub alab_word: Option<&'static str>,
    /// Name inks before contrast flooring.
    pub name: Inks,
    /// Icon inks before contrast flooring.
    pub icon: Inks,
    /// Master character table (`chars[k]` maps to texel `k + 1`).
    pub chars: &'static [u8],
    /// Texel mapping for each entry in `chars`.
    pub texels: &'static [Texel],
    /// Standard-definition sprite rows.
    pub sd: &'static [&'static str],
    /// High-definition sprite rows, if any.
    pub hd: Option<&'static [&'static str]>,
}

impl Brand {
    /// Sprite rows for `master`, if present.
    #[inline]
    #[must_use]
    pub const fn master(&self, master: Master) -> Option<&'static [&'static str]> {
        match master {
            Master::Sd => Some(self.sd),
            Master::Hd => self.hd,
        }
    }
}

/// The [`Brand`] specification behind `mark`.
#[inline]
#[must_use]
pub const fn brand(mark: CliMark) -> &'static Brand {
    mark.brand()
}

/// Claude Code brand specification (`#D77757` mascot and `#000000` eye holes).
const CLAUDE: Brand = Brand {
    word: "claude",
    alab_word: None,
    name: Inks::both(&[0xD7_7757]),
    icon: Inks::both(&[0xD7_7757]),
    chars: b"Ck",
    texels: &[Texel::Ink(0), Texel::Hole(0x00_0000)],
    sd: CLAUDE_SD,
    hd: Some(CLAUDE_HD),
};

/// Codex brand specification (`#3B82F6` on both polarities).
const CODEX: Brand = Brand {
    word: "codex",
    alab_word: Some("codex-alab"),
    name: Inks::both(&[0x3B_82F6]),
    icon: Inks::both(&[0x3B_82F6]),
    chars: b"Oo",
    texels: &[Texel::Ink(0), Texel::Half(0)],
    sd: CODEX_SD,
    hd: Some(CODEX_HD),
};

/// Antigravity CLI brand specification (49-colour solid `A` gradient).
const AGY: Brand = Brand {
    word: "agy",
    alab_word: None,
    name: Inks {
        dark: &[0x5C_5CFF],
        paper: &[0x00_4DA3],
    },
    icon: Inks::both(&[
        0xF2_922E, 0xF0_7236, 0xDB_B131, 0xF6_912E, 0xF3_7337, 0xF0_583B, 0x9E_C345, 0xB5_B43E,
        0xE2_993D, 0xF6_7A34, 0xF8_6A35, 0xEF_5442, 0x86_C64E, 0x75_B45E, 0xCC_954D, 0xEF_7947,
        0xE1_6652, 0xE1_4F59, 0x7C_C251, 0x71_C25C, 0x5C_A98F, 0x5C_91B3, 0x83_73B0, 0x74_6FC3,
        0x99_5DA8, 0x9C_5B97, 0x80_C654, 0x54_B881, 0x40_97DE, 0x4A_7EE4, 0x70_6ECE, 0x8F_64B4,
        0x61_C37D, 0x43_AEAB, 0x4A_80EA, 0x6C_73D8, 0x6D_C694, 0x62_BAD5, 0x47_A8DC, 0x3D_89FB,
        0x4A_81F0, 0x65_79E1, 0x6B_C7A3, 0x64_B6F6, 0x38_86FB, 0x48_81F4, 0x67_B9F4, 0x38_83F9,
        0x3D_85FC,
    ]),
    chars: b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvw",
    texels: &AGY_TEXELS,
    sd: AGY_SD,
    hd: None,
};

/// agy's texels: `chars[k]` paints icon ink `k`, opaque.
const AGY_TEXELS: [Texel; 49] = {
    let mut t = [Texel::Ink(0); 49];
    let mut k = 0;
    while k < t.len() {
        t[k] = Texel::Ink(k as u8);
        k += 1;
    }
    t
};

/// Muse Code brand specification (`#0082FB` outline and `#5AA0FF` core).
const MUSE: Brand = Brand {
    word: "muse",
    alab_word: None,
    name: Inks::both(&[0x00_82FB]),
    icon: Inks::both(&[0x00_82FB, 0x5A_A0FF]),
    chars: b"Mmn",
    texels: &[Texel::Ink(0), Texel::Ink(1), Texel::Half(0)],
    sd: MUSE_SD,
    hd: Some(MUSE_HD),
};

/// Grok brand specification (monochrome, flipped with ground polarity).
const GROK: Brand = Brand {
    word: "grok",
    alab_word: None,
    name: Inks {
        dark: &[0xF2_F2F2],
        paper: &[0x1F_1F1F],
    },
    icon: Inks {
        dark: &[0xF2_F2F2],
        paper: &[0x1F_1F1F],
    },
    chars: b"Gg",
    texels: &[Texel::Ink(0), Texel::Half(0)],
    sd: GROK_SD,
    hd: Some(GROK_HD),
};

/// Kimi Code brand specification (`#38BDF8` sky-cyan).
const KIMI: Brand = Brand {
    word: "kimi",
    alab_word: None,
    name: Inks::both(&[0x38_BDF8]),
    icon: Inks::both(&[0x38_BDF8]),
    chars: b"Kk",
    texels: &[Texel::Ink(0), Texel::Half(0)],
    sd: KIMI_SD,
    hd: Some(KIMI_HD),
};

const _: () = {
    let mut m = 0;
    while m < CliMark::SEEDS.len() {
        let b = CliMark::SEEDS[m].brand();
        assert!(
            aterm_types::mark::check(
                b.word,
                b.alab_word,
                [b.name.dark, b.name.paper],
                [b.icon.dark, b.icon.paper],
                b.chars,
                b.texels,
            )
            .is_ok()
        );
        m += 1;
    }
};

/// Placement and integer scale of a master sprite inside the icon box ([`fit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fit {
    /// Selected master tier.
    pub master: Master,
    /// Integer texel scale in pixels.
    pub scale: u16,
    /// Drawn width in pixels.
    pub dw: u16,
    /// Drawn height in pixels.
    pub dh: u16,
    /// Left offset in pixels inside the icon box.
    pub dx: u16,
    /// Top offset in pixels inside the icon box.
    pub dy: u16,
}

impl Fit {
    /// Effective resolution in SD-texel units.
    #[inline]
    #[must_use]
    pub const fn effective(self) -> u32 {
        self.master.design_scale() as u32 * self.scale as u32
    }
}

/// Compute the [`Fit`] of `mark` inside a `cw × ch` cell's icon box.
#[must_use]
#[inline]
pub const fn fit(mark: CliMark, cw: u16, ch: u16) -> Option<Fit> {
    if cw == 0 {
        return None;
    }
    let box_w = ICON_CELLS as u32 * cw as u32;
    let box_h = ch as u32;
    let Some(room_h) = box_h.checked_sub(AIR_PX as u32) else {
        return None;
    };
    let b = brand(mark);
    let mut best: Option<Fit> = None;
    let masters = [Master::Sd, Master::Hd];
    let mut i = 0;
    while i < masters.len() {
        let master = masters[i];
        i += 1;
        let Some(rows) = b.master(master) else {
            continue;
        };
        let (w, h) = (
            match rows.first() {
                Some(r) => r.len() as u32,
                None => 0,
            },
            rows.len() as u32,
        );
        if w == 0 || h == 0 {
            continue;
        }
        let most = u16::MAX as u32;
        let mut s = box_w / w;
        let rh = room_h / h;
        if rh < s {
            s = rh;
        }
        let mw = most / w;
        if mw < s {
            s = mw;
        }
        let mh = most / h;
        if mh < s {
            s = mh;
        }
        if s == 0 || s > u16::MAX as u32 {
            continue;
        }
        let scale = s as u16;
        let (dw, dh) = (w * s, h * s);
        let cand = Fit {
            master,
            scale,
            dw: dw as u16,
            dh: dh as u16,
            dx: ((box_w - dw) / 2) as u16,
            dy: ((box_h - dh) / 2) as u16,
        };
        let replace = match best {
            None => true,
            Some(kept) => cand.effective() >= kept.effective(),
        };
        if replace {
            best = Some(cand);
        }
    }
    best
}

/// Interned `Index8` [`FxArt`] for `mark` and `master`.
#[must_use]
pub fn art(mark: CliMark, master: Master) -> Option<Arc<FxArt>> {
    type Arts = [Option<Arc<FxArt>>; 2];
    static ARTS: std::sync::Mutex<Vec<(CliMark, Arts)>> = std::sync::Mutex::new(Vec::new());
    let mut arts = ARTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let at = match arts.iter().position(|(kept, _)| *kept == mark) {
        Some(at) => at,
        None => {
            let b = mark.brand();
            let built = [Master::Sd, Master::Hd].map(|m| match b.master(m) {
                Some(rows) => fx_art::indexed(rows, b.chars),
                None => None,
            });
            arts.push((mark, built));
            arts.len() - 1
        }
    };
    arts[at].1[master as usize].clone()
}

/// Unfloored ink for letter `letter` of `mark`'s name on ground polarity `dark`.
#[inline]
#[must_use]
pub const fn name_ink(mark: CliMark, letter: usize, dark: bool) -> u32 {
    let inks = brand(mark).name.on(dark);
    let last = inks.len() - 1;
    inks[if letter < last { letter } else { last }]
}

/// Contrast-floored foreground RGB for letter `letter` of `mark`'s name on `ground`.
#[inline]
#[must_use]
pub const fn name_fg(mark: CliMark, letter: usize, ground: u32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let lg = crate::color_math::relative_luminance_with(table, ground & 0x00FF_FFFF);
    let ink = name_ink(mark, letter, lg < 0.5);
    crate::color_math::floor_contrast_keeping_hue_with_ground_lum(table, ink, lg, NAME_CONTRAST)
}

/// Contrast-floored icon palette for `mark` on `ground`.
#[must_use]
#[inline]
pub fn icon_palette(mark: CliMark, ground: u32) -> Vec<FxInk> {
    let b = brand(mark);
    let table = crate::color_math::relative_luminance_table();
    let lg = crate::color_math::relative_luminance_with(table, ground);
    let inks = b.icon.on(lg < 0.5);
    let mut out = Vec::with_capacity(b.texels.len());
    if inks.len() <= 2 {
        let f0 = crate::color_math::floor_contrast_keeping_hue_with_ground_lum(
            table,
            inks[0],
            lg,
            ICON_CONTRAST,
        );
        let f1 = if inks.len() > 1 {
            crate::color_math::floor_contrast_keeping_hue_with_ground_lum(
                table,
                inks[1],
                lg,
                ICON_CONTRAST,
            )
        } else {
            0
        };
        let floored = [f0, f1];
        out.extend(b.texels.iter().map(|&t| match t {
            Texel::Ink(i) => FxInk::over(floored[usize::from(i)], 255),
            Texel::Half(i) => {
                FxInk::over(premul_rgb(floored[usize::from(i)], HALF_ALPHA), HALF_ALPHA)
            }
            Texel::Hole(rgb) => FxInk::over(rgb, 255),
        }));
    } else {
        out.extend(inks.iter().map(|&ink| {
            FxInk::over(
                crate::color_math::floor_contrast_keeping_hue_with_ground_lum(
                    table,
                    ink,
                    lg,
                    ICON_CONTRAST,
                ),
                255,
            )
        }));
    }
    out
}

/// Build the [`FxContent`] sprite payload for `mark` at `fit` on `ground`.
#[must_use]
#[inline]
pub fn content(mark: CliMark, fit: Fit, ground: u32) -> Arc<FxContent> {
    let Some(art) = art(mark, fit.master) else {
        return Arc::new(FxContent::new(Vec::new(), Vec::new()));
    };
    let sprite = FxSprite {
        x: i32::from(fit.dx),
        y: i32::from(fit.dy),
        dw: fit.dw,
        dh: fit.dh,
        flip_x: false,
        art,
        ink: 0,
        clip: PxRect::EVERYWHERE,
    };
    Arc::new(FxContent::new(
        vec![FxItem::Sprite(sprite)],
        icon_palette(mark, ground),
    ))
}

/// Build the [`FxLayer`] placing `content` above the terminal glyphs.
#[must_use]
#[inline]
pub fn layer(id: u64, content: Arc<FxContent>, offset: (i32, i32), clip: PxRect) -> FxLayer {
    let mut layer = FxLayer::new(id, content, FxSlot::Over, FxSpace::Grid, FxBlend::Exact);
    layer.offset = offset;
    layer.clip = clip;
    layer
}

/// Claude Code's whole mascot, SD 14×10: its five equal bands (head, eyes,
/// arms, body, legs) at two pixels each, decoded from 2.1.30x's quadrant
/// rows ` ▐▛███▛█` / `▝▜██████▀` / ` ▝▝   ▝▝`.
const CLAUDE_SD: &[&str] = &[
    "..CCCCCCCCCC..",
    "..CCCCCCCCCC..",
    "..CCkCCCCkCC..",
    "..CCkCCCCkCC..",
    "CCCCCCCCCCCCCC",
    "CCCCCCCCCCCCCC",
    "..CCCCCCCCCC..",
    "..CCCCCCCCCC..",
    "..C.C....C.C..",
    "..C.C....C.C..",
];

/// Claude Code's whole mascot, HD 28×16 (aspect 1.75 against the true 1.7):
/// head 3, eyes 3 (each 2×3), arms 4, body 3, legs 3 (each 2 wide).
const CLAUDE_HD: &[&str] = &[
    "...CCCCCCCCCCCCCCCCCCCCCC...",
    "...CCCCCCCCCCCCCCCCCCCCCC...",
    "...CCCCCCCCCCCCCCCCCCCCCC...",
    "...CCCkkCCCCCCCCCCCCkkCCC...",
    "...CCCkkCCCCCCCCCCCCkkCCC...",
    "...CCCkkCCCCCCCCCCCCkkCCC...",
    "CCCCCCCCCCCCCCCCCCCCCCCCCCCC",
    "CCCCCCCCCCCCCCCCCCCCCCCCCCCC",
    "CCCCCCCCCCCCCCCCCCCCCCCCCCCC",
    "CCCCCCCCCCCCCCCCCCCCCCCCCCCC",
    "...CCCCCCCCCCCCCCCCCCCCCC...",
    "...CCCCCCCCCCCCCCCCCCCCCC...",
    "...CCCCCCCCCCCCCCCCCCCCCC...",
    "...CC.CC............CC.CC...",
    "...CC.CC............CC.CC...",
    "...CC.CC............CC.CC...",
];

/// The OpenAI knot, SD 12×12: six blades round a hexagonal hole, 180°
/// symmetric, hand-cleaned so each blade's gap reads at one pixel.
const CODEX_SD: &[&str] = &[
    "....O.OO....",
    "..OOO.OOOo..",
    ".OOOO.OOOO..",
    "..OO..Oo..oO",
    "O..O....OOOO",
    "OOo.....oOOO",
    "OOOo.....oOO",
    "OOOO....O..O",
    "Oo..oO..OO..",
    "..OOOO.OOOO.",
    "..oOOO.OOO..",
    "....OO.O....",
];

/// The OpenAI knot, HD 24×24: supersampled 8×8 (hole 0.31, gap 0.12,
/// twist 60°, rotation 30°).
const CODEX_HD: &[&str] = &[
    ".........OO.OOO.........",
    ".......oOOo.OOOOo.......",
    ".....oOOOOo.OOOOOOo.....",
    "...oOOOOOO.oOOOOOOOOo...",
    "..OOOOOOOO.OOOOOOOOOOO..",
    ".oOOOOOOOO.OOOOOOOOOo...",
    "..oOOOOOOo.OOOOOOo...oO.",
    "oo.oOOOOOo.OOOo...oOOOOo",
    "oOO..OOOO..o...oOOOOOOOo",
    "oOOO..OOO.....oOOOOOOOOo",
    "oOOOO..OO......oOOOOOOOo",
    "oOOOOO..........oOOOOOOo",
    "oOOOOOOo..........OOOOOo",
    "oOOOOOOOo......OO..OOOOo",
    "oOOOOOOOOo.....OOO..OOOo",
    "oOOOOOOOo...o..OOOO..OOo",
    "oOOOOo...oOOO.oOOOOOo.oo",
    ".Oo...oOOOOOO.oOOOOOOo..",
    "...oOOOOOOOOO.OOOOOOOOo.",
    "..OOOOOOOOOOO.OOOOOOOO..",
    "...oOOOOOOOOo.OOOOOOo...",
    ".....oOOOOOO.oOOOOo.....",
    ".......oOOOO.oOOo.......",
    ".........OOO.OO.........",
];

/// Antigravity CLI launch banner, SD 12×10 (one half-cell per pixel).
const AGY_SD: &[&str] = &[
    ".....AB.....",
    "....CDEF....",
    "...GHIJKL...",
    "...MNOPQR...",
    "..STUVWXYZ..",
    "..abc..def..",
    "..gh....ij..",
    ".klm....nop.",
    ".qr......st.",
    "ur........vw",
];

/// Muse Code's `◈` (its `STATUS_GLYPHS` middle diamond), SD 12×12.
const MUSE_SD: &[&str] = &[
    ".....MM.....",
    "....M..M....",
    "...M....M...",
    "..M..mm..M..",
    ".M..mmmm..M.",
    "M..mmmmmm..M",
    "M..mmmmmm..M",
    ".M..mmmm..M.",
    "..M..mm..M..",
    "...M....M...",
    "....M..M....",
    ".....MM.....",
];

/// Muse Code's `◈`, HD 24×24, its outline and core edged in half-tone.
const MUSE_HD: &[&str] = &[
    "...........nn...........",
    "..........nMMn..........",
    ".........nMMMMn.........",
    "........nMMMMMMn........",
    ".......nMMM..MMMn.......",
    "......nMMM....MMMn......",
    ".....nMMM..nn..MMMn.....",
    "....nMMM..nmmn..MMMn....",
    "...nMMM..nmmmmn..MMMn...",
    "..nMMM..nmmmmmmn..MMMn..",
    ".nMMM..nmmmmmmmmn..MMMn.",
    "nMMM..nmmmmmmmmmmn..MMMn",
    "nMMM..nmmmmmmmmmmn..MMMn",
    ".nMMM..nmmmmmmmmn..MMMn.",
    "..nMMM..nmmmmmmn..MMMn..",
    "...nMMM..nmmmmn..MMMn...",
    "....nMMM..nmmn..MMMn....",
    ".....nMMM..nn..MMMn.....",
    "......nMMM....MMMn......",
    ".......nMMM..MMMn.......",
    "........nMMMMMMn........",
    ".........nMMMMn.........",
    "..........nMMn..........",
    "...........nn...........",
];

/// Grok's slashed ring, SD 12×12.
const GROK_SD: &[&str] = &[
    "...gGGGG..GG",
    "..GGGGGG.GGG",
    ".GGG....GGG.",
    "gGG....GGG..",
    "GG....GGG.GG",
    "GG...GGG..GG",
    "GG..GGG...GG",
    "GG.GGG....GG",
    "gGGGG....GGg",
    ".GGG....GGG.",
    "..GGGGGGGG..",
    "...gGGGGg...",
];

/// Grok's slashed ring, HD 24×24.
const GROK_HD: &[&str] = &[
    "........gGGGGGGg.....gGG",
    "......gGGGGGGGGG....gGGG",
    ".....GGGGGGGGGGG...gGGGg",
    "...gGGGGg.........gGGGg.",
    "...GGGg..........gGGGg..",
    "..GGGg..........gGGGg...",
    ".gGGg..........gGGGg....",
    ".GGG..........gGGGg.....",
    "gGGg.........gGGGg...GGg",
    "GGG.........gGGGg....GGG",
    "GGG........gGGGg.....GGG",
    "GGG.......gGGGg......GGG",
    "GGG......gGGGg.......GGG",
    "GGG.....gGGGg........GGG",
    "GGG....gGGGg.........GGG",
    "gGGg..gGGGg.........gGGg",
    ".GGG.gGGGg..........GGG.",
    ".gGGGGGGg..........gGGg.",
    "..GGGGGg..........gGGG..",
    "...GGGG..........gGGG...",
    "...gGGGGg......gGGGGg...",
    ".....GGGGGGGGGGGGGG.....",
    "......gGGGGGGGGGGg......",
    "........gGGGGGGg........",
];

/// Kimi Code's `K` glyph mark, SD 12×12.
const KIMI_SD: &[&str] = &[
    ".KKK....kKKK",
    ".KKK...kKKK.",
    ".KKK..kKKK..",
    ".KKK.kKKK...",
    ".KKKKKKk....",
    ".KKKKKK.....",
    ".KKKKKKk....",
    ".KKK.kKKK...",
    ".KKK..kKKK..",
    ".KKK...kKKK.",
    ".KKK....kKKK",
    "............",
];

/// Kimi Code's `K` glyph mark, HD 24×24.
const KIMI_HD: &[&str] = &[
    "..KKKKKK........kKKKKKK.",
    "..KKKKKK.......kKKKKKK..",
    "..KKKKKK......kKKKKKK...",
    "..KKKKKK.....kKKKKKK....",
    "..KKKKKK....kKKKKKK.....",
    "..KKKKKK...kKKKKKK......",
    "..KKKKKK..kKKKKKK.......",
    "..KKKKKK.kKKKKKK........",
    "..KKKKKKKKKKKKK.........",
    "..KKKKKKKKKKKK..........",
    "..KKKKKKKKKKKK..........",
    "..KKKKKKKKKKKKK.........",
    "..KKKKKK.kKKKKKK........",
    "..KKKKKK..kKKKKKK.......",
    "..KKKKKK...kKKKKKK......",
    "..KKKKKK....kKKKKKK.....",
    "..KKKKKK.....kKKKKKK....",
    "..KKKKKK......kKKKKKK...",
    "..KKKKKK.......kKKKKKK..",
    "..KKKKKK........kKKKKKK.",
    "..KKKKKK.........kKKKKKK",
    "........................",
    "........................",
    "........................",
];

#[cfg(test)]
#[path = "cli_marks_tests.rs"]
mod tests;
