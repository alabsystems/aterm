// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Agent CLI top-line brand marks, icons, and contrast-floored palettes.

use std::sync::{Arc, OnceLock};

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, PxRect,
};
use aterm_render::fx as fx_art;
use aterm_render::premul_rgb;

use crate::color_math::floor_contrast_keeping_hue;
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

/// Agent CLI that wears a brand mark on its top line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CliMark {
    /// Claude Code.
    Claude,
    /// OpenAI Codex (and `codex-alab`).
    Codex,
    /// Google Antigravity CLI.
    Agy,
    /// Meta Muse Code.
    Muse,
    /// xAI Grok.
    Grok,
    /// Moonshot Kimi Code.
    Kimi,
}

impl CliMark {
    /// All marks in declaration order.
    pub const ALL: [CliMark; 6] = [
        CliMark::Claude,
        CliMark::Codex,
        CliMark::Agy,
        CliMark::Muse,
        CliMark::Grok,
        CliMark::Kimi,
    ];

    /// Look up the mark for a canonical lowercase CLI app id.
    #[must_use]
    pub const fn for_app(app_id: &str) -> Option<Self> {
        match app_id.as_bytes() {
            b"claude" => Some(Self::Claude),
            b"codex" => Some(Self::Codex),
            b"agy" => Some(Self::Agy),
            b"muse" => Some(Self::Muse),
            b"grok" => Some(Self::Grok),
            b"kimi" => Some(Self::Kimi),
            _ => None,
        }
    }

    /// Canonical lowercase word for this CLI.
    #[must_use]
    pub const fn word(self) -> &'static str {
        brand(self).word
    }

    /// Display name printed by the lockup for `build`.
    #[must_use]
    pub const fn name(self, build: Build) -> &'static str {
        match (build, brand(self).alab_word) {
            (Build::Alab, Some(word)) => word,
            _ => self.word(),
        }
    }

    /// Total cell width of the lockup for `build`.
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

/// How a master character paints in the icon palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Texel {
    /// Opaque icon ink `i` ([`Brand::icon`]), contrast-floored to [`ICON_CONTRAST`].
    Ink(u8),
    /// Contrast-floored icon ink `i` at [`HALF_ALPHA`].
    Half(u8),
    /// Unfloored exact `0x00RRGGBB` hole colour.
    Hole(u32),
}

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
    const fn both(inks: &'static [u32]) -> Self {
        Self {
            dark: inks,
            paper: inks,
        }
    }

    /// Select the ink list for ground polarity `dark`.
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
    #[must_use]
    pub const fn design_scale(self) -> u16 {
        match self {
            Master::Sd => 1,
            Master::Hd => 2,
        }
    }
}

/// Brand specification row for one [`CliMark`].
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
    #[must_use]
    pub const fn master(&self, master: Master) -> Option<&'static [&'static str]> {
        match master {
            Master::Sd => Some(self.sd),
            Master::Hd => self.hd,
        }
    }
}

/// Look up the [`Brand`] specification for `mark`.
#[must_use]
pub const fn brand(mark: CliMark) -> &'static Brand {
    match mark {
        CliMark::Claude => &CLAUDE,
        CliMark::Codex => &CODEX,
        CliMark::Agy => &AGY,
        CliMark::Muse => &MUSE,
        CliMark::Grok => &GROK,
        CliMark::Kimi => &KIMI,
    }
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

const MAX_ICON_INKS: usize = 49;

const _: () = {
    let mut m = 0;
    while m < CliMark::ALL.len() {
        let b = brand(CliMark::ALL[m]);
        assert!(b.texels.len() == b.chars.len());
        assert!(
            b.icon.dark.len() == b.icon.paper.len()
                && !b.icon.dark.is_empty()
                && b.icon.dark.len() <= MAX_ICON_INKS
        );
        let mut k = 0;
        while k < b.texels.len() {
            match b.texels[k] {
                Texel::Ink(i) | Texel::Half(i) => assert!((i as usize) < b.icon.dark.len()),
                Texel::Hole(rgb) => assert!(rgb <= 0x00FF_FFFF),
            }
            k += 1;
        }
        let w = b.word.len();
        assert!(w > 0);
        assert!(b.name.dark.len() == 1 || b.name.dark.len() == w);
        assert!(b.name.paper.len() == 1 || b.name.paper.len() == w);
        if let Some(alab) = b.alab_word {
            let a = alab.len();
            assert!(a > 0);
            assert!(b.name.dark.len() == 1 || b.name.dark.len() == a);
            assert!(b.name.paper.len() == 1 || b.name.paper.len() == a);
        }
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
    #[must_use]
    pub const fn effective(self) -> u32 {
        self.master.design_scale() as u32 * self.scale as u32
    }
}

/// Compute the [`Fit`] of `mark` inside a `cw × ch` cell's icon box.
#[must_use]
pub const fn fit(mark: CliMark, cw: u16, ch: u16) -> Option<Fit> {
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
pub fn art(mark: CliMark, master: Master) -> Option<&'static Arc<FxArt>> {
    static ARTS: [OnceLock<[Option<Arc<FxArt>>; 2]>; CliMark::ALL.len()] =
        [const { OnceLock::new() }; CliMark::ALL.len()];
    let arts = ARTS[mark as usize].get_or_init(|| {
        let b = brand(mark);
        [Master::Sd, Master::Hd].map(|m| match b.master(m) {
            Some(rows) => fx_art::indexed(rows, b.chars),
            None => None,
        })
    });
    arts[master as usize].as_ref()
}

/// Unfloored ink for letter `letter` of `mark`'s name on ground polarity `dark`.
#[must_use]
pub const fn name_ink(mark: CliMark, letter: usize, dark: bool) -> u32 {
    let inks = brand(mark).name.on(dark);
    let last = inks.len() - 1;
    inks[if letter < last { letter } else { last }]
}

/// Contrast-floored foreground RGB for letter `letter` of `mark`'s name on `ground`.
#[must_use]
pub const fn name_fg(mark: CliMark, letter: usize, ground: u32) -> u32 {
    let ink = name_ink(mark, letter, dark_ground(ground));
    floor_contrast_keeping_hue(ink, ground, NAME_CONTRAST)
}

/// Contrast-floored icon palette for `mark` on `ground`.
#[must_use]
pub fn icon_palette(mark: CliMark, ground: u32) -> Vec<FxInk> {
    let b = brand(mark);
    let inks = b.icon.on(dark_ground(ground));
    let mut floored = [0u32; MAX_ICON_INKS];
    for (slot, &ink) in floored[..inks.len()].iter_mut().zip(inks) {
        *slot = floor_contrast_keeping_hue(ink, ground, ICON_CONTRAST);
    }
    let mut out = Vec::with_capacity(b.texels.len());
    for &t in b.texels {
        out.push(match t {
            Texel::Ink(i) => FxInk::over(floored[usize::from(i)], 255),
            Texel::Half(i) => {
                FxInk::over(premul_rgb(floored[usize::from(i)], HALF_ALPHA), HALF_ALPHA)
            }
            Texel::Hole(rgb) => FxInk::over(rgb, 255),
        });
    }
    out
}

/// Build the [`FxContent`] sprite payload for `mark` at `fit` on `ground`.
#[must_use]
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
        art: Arc::clone(art),
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
mod tests {
    use super::*;
    use crate::color_math::{contrast_ratio, rgb2hsv};
    use aterm_render::fx::paint_layers;
    use aterm_render::{FxFrame, over_premul};
    use aterm_types::scheme::{builtin, builtin_names};

    /// Codex's slab: the tinted pad row its composer is erased with
    /// (`48;2;45;47;51`, 0.154), a ground of its own.
    const CODEX_SLAB: u32 = 0x2D_2F33;

    /// A built-in theme's background as `0x00RRGGBB`.
    fn ground(name: &str) -> u32 {
        let c = builtin(name)
            .unwrap_or_else(|| panic!("{name} is built in"))
            .background;
        (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
    }

    /// Every built-in theme's ground, and Codex's slab.
    fn grounds() -> Vec<(&'static str, u32)> {
        let mut all: Vec<_> = builtin_names()
            .into_iter()
            .map(|n| (n, ground(n)))
            .collect();
        all.push(("Codex slab", CODEX_SLAB));
        all
    }

    /// The HSV hue distance between two colours, in degrees.
    fn hue_gap(a: u32, b: u32) -> f32 {
        let d = (rgb2hsv(a).0 - rgb2hsv(b).0).abs() % 360.0;
        d.min(360.0 - d)
    }

    /// Icon inks read at 3:1 or better on every built-in theme ground and Codex's slab.
    #[test]
    fn icon_inks_read_at_3_on_every_ground() {
        let default = ground("Default");
        let grounds = grounds();
        assert!(
            grounds.iter().filter(|(_, g)| dark_ground(*g)).count() > 1
                && grounds.iter().filter(|(_, g)| !dark_ground(*g)).count() > 1,
            "grounds of both polarities"
        );
        for mark in CliMark::ALL {
            let b = brand(mark);
            for &(name, g) in &grounds {
                let raw = b.icon.on(dark_ground(g));
                let pal = icon_palette(mark, g);
                assert_eq!(pal.len(), b.chars.len(), "{mark:?}: an entry a char");
                let full = |i: u8| {
                    let k = b.texels.iter().position(|&t| t == Texel::Ink(i));
                    pal[k.unwrap_or_else(|| panic!("{mark:?}: ink {i} has a full texel"))]
                };
                for (k, (&texel, &ink)) in b.texels.iter().zip(&pal).enumerate() {
                    let c = char::from(b.chars[k]);
                    match texel {
                        Texel::Ink(i) => {
                            let brand_ink = raw[usize::from(i)];
                            let ratio = contrast_ratio(ink.color, g);
                            assert_eq!(ink.alpha, 255, "{mark:?} {c} on {name}: opaque");
                            assert!(
                                ratio >= ICON_CONTRAST,
                                "{mark:?} {c} on {name}: {:06x} reads {ratio:.2}",
                                ink.color
                            );
                            if rgb2hsv(brand_ink).1 > 0.1 {
                                let gap = hue_gap(ink.color, brand_ink);
                                assert!(gap <= 2.0, "{mark:?} {c} on {name}: hue moved {gap:.1}°");
                            }
                            if g == default {
                                assert_eq!(ink.color, brand_ink, "{mark:?} {c}: the brand's bytes");
                            }
                        }
                        Texel::Half(i) => {
                            let f = full(i).color;
                            let want = FxInk::over(premul_rgb(f, HALF_ALPHA), HALF_ALPHA);
                            assert_eq!(ink, want, "{mark:?} {c} on {name}: its full ink, halved");
                            if g == default {
                                assert_eq!(
                                    f,
                                    raw[usize::from(i)],
                                    "{mark:?} {c}: the brand's bytes"
                                );
                            }
                        }
                        Texel::Hole(rgb) => {
                            assert_eq!(ink, FxInk::over(rgb, 255), "{mark:?} {c}: never floored");
                        }
                    }
                }
            }
        }
        let (paper, core) = (ground("Gruvbox Light"), MUSE.icon.paper[1]);
        let raw = contrast_ratio(core, paper);
        assert!(raw < ICON_CONTRAST, "CONTROL: the raw core reads {raw:.2}");
        let floored = icon_palette(CliMark::Muse, paper)[1].color;
        assert_ne!(floored, core, "CONTROL: the floor moved it");
        assert!(contrast_ratio(floored, paper) >= ICON_CONTRAST, "CONTROL");
    }

    /// Lockup name letters read at 4.5:1 or better on every built-in theme ground and Codex's slab.
    #[test]
    fn names_read_at_4_5_on_every_ground() {
        let default = ground("Default");
        let mut moved = 0;
        let names = CliMark::ALL
            .into_iter()
            .flat_map(|mark| Build::ALL.map(|build| (mark, mark.name(build))));
        for (mark, word) in names {
            for k in 0..word.chars().count() {
                for &(name, g) in &grounds() {
                    let raw = name_ink(mark, k, dark_ground(g));
                    let fg = name_fg(mark, k, g);
                    let ratio = contrast_ratio(fg, g);
                    assert!(
                        ratio >= NAME_CONTRAST,
                        "{word} letter {k} on {name}: {fg:06x} reads {ratio:.2}"
                    );
                    if rgb2hsv(raw).1 > 0.1 {
                        let gap = hue_gap(fg, raw);
                        assert!(
                            gap <= 2.0,
                            "{word} letter {k} on {name}: hue moved {gap:.1}°"
                        );
                    }
                    if g == default && contrast_ratio(raw, g) >= NAME_CONTRAST {
                        assert_eq!(fg, raw, "{word} letter {k}: the brand's bytes");
                    }
                    moved += usize::from(fg != raw);
                }
            }
        }
        assert!(moved > 0, "the floor moved some letter on some ground");
        let (paper, ink) = (ground("Default Light"), CLAUDE.name.paper[0]);
        let raw = contrast_ratio(ink, paper);
        assert!(
            raw < NAME_CONTRAST && (raw - 2.94).abs() < 0.01,
            "CONTROL: Claude's raw name reads {raw:.2} on Default Light"
        );
        let floored = name_fg(CliMark::Claude, 0, paper);
        assert_ne!(floored, ink, "CONTROL: the floor moved it");
        assert!(contrast_ratio(floored, paper) >= NAME_CONTRAST, "CONTROL");
        let alab = CliMark::Codex.name(Build::Alab);
        assert_eq!(alab, "codex-alab", "the owner's name for the ALab build");
        let last = alab.len() - 1;
        assert!(
            last >= CliMark::Codex.word().len(),
            "past the vendor's word"
        );
        let raw = name_ink(CliMark::Codex, last, true);
        let reads = contrast_ratio(raw, CODEX_SLAB);
        assert!(
            reads < NAME_CONTRAST && (reads - 3.65).abs() < 0.01,
            "CONTROL: codex-alab's raw last letter reads {reads:.2} on the slab"
        );
        let floored = name_fg(CliMark::Codex, last, CODEX_SLAB);
        assert_ne!(floored, raw, "CONTROL: the floor moved it");
        assert!(
            contrast_ratio(floored, CODEX_SLAB) >= NAME_CONTRAST,
            "CONTROL"
        );
    }

    /// `codex-alab` prints `codex-alab` while every other mark prints its canonical word on either build.
    #[test]
    fn every_build_prints_its_own_name() {
        for mark in CliMark::ALL {
            let want = if mark == CliMark::Codex {
                "codex-alab"
            } else {
                mark.word()
            };
            assert_eq!(mark.name(Build::Alab), want, "{mark:?}");
            assert_eq!(mark.name(Build::Vendor), mark.word(), "{mark:?}");
            for build in Build::ALL {
                assert_eq!(
                    mark.lockup_cells(build),
                    ICON_RUN + mark.name(build).len() + 1,
                    "{mark:?} {build:?}"
                );
            }
        }
        assert_eq!(
            CliMark::Codex.lockup_cells(Build::Alab) - CliMark::Codex.lockup_cells(Build::Vendor),
            5,
            "`-alab`"
        );
        assert_eq!(
            CliMark::Codex.name(Build::Vendor),
            "codex",
            "CONTROL: vendor codex keeps its word"
        );
        assert_eq!(
            CliMark::for_app("codex-alab"),
            None,
            "the app id stays the CLI's word"
        );
    }

    /// The shape of a master: ink or not, a row a vector.
    fn shape(rows: &[&str]) -> Vec<Vec<bool>> {
        rows.iter()
            .map(|r| r.bytes().map(|b| b != b'.').collect())
            .collect()
    }

    /// Whether a master's shape is its own mirror image.
    fn mirrored(rows: &[&str]) -> bool {
        shape(rows).iter().all(|r| r.iter().eq(r.iter().rev()))
    }

    /// Whether a master's shape is itself turned half round.
    fn turned(rows: &[&str]) -> bool {
        let s = shape(rows);
        let turned: Vec<Vec<bool>> = s
            .iter()
            .rev()
            .map(|r| r.iter().rev().copied().collect())
            .collect();
        s == turned
    }

    /// What is wrong with a master: no rows, a ragged row, or a byte neither
    /// `'.'` nor one of `chars`.
    fn master_faults(rows: &[&str], chars: &[u8]) -> Vec<String> {
        let mut faults = Vec::new();
        let w = rows.first().map_or(0, |r| r.len());
        if w == 0 {
            faults.push("no texels".to_owned());
        }
        for (y, row) in rows.iter().enumerate() {
            if row.len() != w {
                faults.push(format!("row {y} is {} wide, not {w}", row.len()));
            }
            if let Some(b) = row.bytes().find(|b| *b != b'.' && !chars.contains(b)) {
                faults.push(format!("row {y} holds {:?}", char::from(b)));
            }
        }
        faults
    }

    /// Every master is rectangular, holds only its declared chars, preserves its symmetry, and fits centred at integer scale.
    #[test]
    fn every_mark_is_crisp_and_centred() {
        use CliMark::{Agy, Claude, Codex, Grok, Kimi, Muse};
        use Master::{Hd, Sd};
        for mark in CliMark::ALL {
            let b = brand(mark);
            assert_eq!(
                b.hd.is_some(),
                mark != Agy,
                "{mark:?}: §2.4 draws an HD master"
            );
            for master in [Sd, Hd] {
                let Some(rows) = b.master(master) else {
                    continue;
                };
                let faults = master_faults(rows, b.chars);
                assert!(faults.is_empty(), "{mark:?} {master:?}: {faults:?}");
                let built = art(mark, master).expect("a well-formed master builds");
                assert_eq!(
                    (usize::from(built.w()), usize::from(built.h())),
                    (rows[0].len(), rows.len()),
                    "{mark:?} {master:?}"
                );
                if matches!(mark, Claude | Agy | Muse) {
                    assert!(mirrored(rows), "{mark:?} {master:?} mirrors");
                }
                if matches!(mark, Codex | Muse) {
                    assert!(turned(rows), "{mark:?} {master:?} turns half round");
                }
            }
        }
        let table = [
            ((7, 14), Claude, Sd, 1, (0, 2)),
            ((7, 14), Codex, Sd, 1, (1, 1)),
            ((7, 14), Agy, Sd, 1, (1, 2)),
            ((7, 14), Muse, Sd, 1, (1, 1)),
            ((7, 14), Grok, Sd, 1, (1, 1)),
            ((7, 14), Kimi, Sd, 1, (1, 1)),
            ((9, 23), Claude, Sd, 1, (2, 6)),
            ((9, 23), Codex, Sd, 1, (3, 5)),
            ((9, 23), Agy, Sd, 1, (3, 6)),
            ((9, 23), Muse, Sd, 1, (3, 5)),
            ((9, 23), Grok, Sd, 1, (3, 5)),
            ((9, 23), Kimi, Sd, 1, (3, 5)),
            ((11, 21), Claude, Sd, 1, (4, 5)),
            ((11, 21), Codex, Sd, 1, (5, 4)),
            ((11, 21), Agy, Sd, 1, (5, 5)),
            ((11, 21), Muse, Sd, 1, (5, 4)),
            ((11, 21), Grok, Sd, 1, (5, 4)),
            ((11, 21), Kimi, Sd, 1, (5, 4)),
            ((15, 28), Claude, Hd, 1, (1, 6)),
            ((15, 28), Codex, Hd, 1, (3, 2)),
            ((15, 28), Agy, Sd, 2, (3, 4)),
            ((15, 28), Muse, Hd, 1, (3, 2)),
            ((15, 28), Grok, Hd, 1, (3, 2)),
            ((15, 28), Kimi, Hd, 1, (3, 2)),
            ((22, 42), Claude, Sd, 3, (1, 6)),
            ((22, 42), Codex, Sd, 3, (4, 3)),
            ((22, 42), Agy, Sd, 3, (4, 6)),
            ((22, 42), Muse, Sd, 3, (4, 3)),
            ((22, 42), Grok, Sd, 3, (4, 3)),
            ((22, 42), Kimi, Sd, 3, (4, 3)),
            ((30, 56), Claude, Hd, 2, (2, 12)),
            ((30, 56), Codex, Hd, 2, (6, 4)),
            ((30, 56), Agy, Sd, 5, (0, 3)),
            ((30, 56), Muse, Hd, 2, (6, 4)),
            ((30, 56), Grok, Hd, 2, (6, 4)),
            ((30, 56), Kimi, Hd, 2, (6, 4)),
        ];
        for ((cw, ch), mark, master, scale, (dx, dy)) in table {
            let f = fit(mark, cw, ch).unwrap_or_else(|| panic!("{mark:?} fits {cw}×{ch}"));
            let at = format!("{mark:?} at {cw}×{ch}");
            assert_eq!(
                (f.master, f.scale, f.dx, f.dy),
                (master, scale, dx, dy),
                "{at}"
            );
            let rows = brand(mark).master(f.master).expect("its master");
            let (w, h) = (rows[0].len() as u16, rows.len() as u16);
            assert_eq!(
                (f.dw, f.dh),
                (w * f.scale, h * f.scale),
                "{at}: whole texel blocks"
            );
            assert!(
                f.dw <= 2 * cw && f.dh <= ch - AIR_PX,
                "{at}: in the box, with air"
            );
            assert_eq!(
                (f.dx, f.dy),
                ((2 * cw - f.dw) / 2, (ch - f.dh) / 2),
                "{at}: centred"
            );
        }
        for mark in CliMark::ALL {
            let f = fit(mark, 6, 12);
            if mark == Agy {
                let want = Fit {
                    master: Sd,
                    scale: 1,
                    dw: 12,
                    dh: 10,
                    dx: 0,
                    dy: 1,
                };
                assert_eq!(f, Some(want), "CONTROL: agy's 12×10 banner still fits");
            } else {
                assert_eq!(f, None, "CONTROL: {mark:?} has no room at 6×12");
            }
            assert_eq!(
                mark.lockup_cells(Build::Vendor),
                4 + mark.word().len() + 1,
                "CONTROL: {mark:?}"
            );
        }
        assert_eq!(ICON_RUN, 4, "CONTROL: a pad, the icon's two cells, a pad");
        assert!(
            !master_faults(&["..C", ".C"], b"C").is_empty(),
            "CONTROL: ragged"
        );
        assert!(
            !master_faults(&["..X"], b"C").is_empty(),
            "CONTROL: a stray byte"
        );
    }

    /// Captured `agy` 1.4.4 launch banner cells (`(col, row, glyph, fg, bg)`).
    #[rustfmt::skip]
    const AGY_BANNER_1_4_4: &[(usize, usize, char, u32, Option<u32>)] = &[
        (4, 0, '▄', 0xDB_B131, None), (5, 0, '▀', 0xF2_922E, Some(0xF6_912E)),
        (6, 0, '▀', 0xF0_7236, Some(0xF3_7337)), (7, 0, '▄', 0xF0_583B, None),
        (3, 1, '▀', 0x9E_C345, Some(0x86_C64E)), (4, 1, '▀', 0xB5_B43E, Some(0x75_B45E)),
        (5, 1, '▀', 0xE2_993D, Some(0xCC_954D)), (6, 1, '▀', 0xF6_7A34, Some(0xEF_7947)),
        (7, 1, '▀', 0xF8_6A35, Some(0xE1_6652)), (8, 1, '▀', 0xEF_5442, Some(0xE1_4F59)),
        (2, 2, '▀', 0x7C_C251, Some(0x80_C654)), (3, 2, '▀', 0x71_C25C, Some(0x54_B881)),
        (4, 2, '▀', 0x5C_A98F, Some(0x40_97DE)), (5, 2, '▀', 0x5C_91B3, None),
        (6, 2, '▀', 0x83_73B0, None), (7, 2, '▀', 0x74_6FC3, Some(0x4A_7EE4)),
        (8, 2, '▀', 0x99_5DA8, Some(0x70_6ECE)), (9, 2, '▀', 0x9C_5B97, Some(0x8F_64B4)),
        (1, 3, '▄', 0x6D_C694, None), (2, 3, '▀', 0x61_C37D, Some(0x62_BAD5)),
        (3, 3, '▀', 0x43_AEAB, Some(0x47_A8DC)), (8, 3, '▀', 0x4A_80EA, Some(0x3D_89FB)),
        (9, 3, '▀', 0x6C_73D8, Some(0x4A_81F0)), (10, 3, '▄', 0x65_79E1, None),
        (0, 4, '▄', 0x67_B9F4, None), (1, 4, '▀', 0x6B_C7A3, Some(0x64_B6F6)),
        (2, 4, '▀', 0x64_B6F6, None), (9, 4, '▀', 0x38_86FB, None),
        (10, 4, '▀', 0x48_81F4, Some(0x38_83F9)), (11, 4, '▄', 0x3D_85FC, None),
    ];

    /// A half-block banner as pixels, one half-cell each (`None`: the
    /// ground): a `▀` is its foreground over its background, a `▄` the
    /// ground over its foreground.
    fn banner_pixels(
        cells: &[(usize, usize, char, u32, Option<u32>)],
        (w, h): (usize, usize),
        as_text: bool,
    ) -> Vec<Vec<Option<u32>>> {
        let mut px = vec![vec![None; w]; h];
        for &(x, row, glyph, fg, bg) in cells {
            match glyph {
                '▀' => {
                    px[2 * row][x] = Some(fg);
                    px[2 * row + 1][x] = bg.filter(|_| !as_text);
                }
                '▄' => px[2 * row + 1][x] = Some(fg),
                other => panic!("not a half block: {other:?}"),
            }
        }
        px
    }

    /// `agy`'s SD master matches its launch banner on both ground polarities, one half-cell per pixel.
    #[test]
    fn agys_mark_is_its_banner() {
        let b = brand(CliMark::Agy);
        let size = (b.sd[0].len(), b.sd.len());
        let master = |dark: bool| -> Vec<Vec<Option<u32>>> {
            b.sd.iter()
                .map(|row| {
                    row.bytes()
                        .map(|c| {
                            let k = b.chars.iter().position(|&ch| ch == c)?;
                            match b.texels[k] {
                                Texel::Ink(i) => Some(b.icon.on(dark)[usize::from(i)]),
                                other => panic!("agy paints inks only, not {other:?}"),
                            }
                        })
                        .collect()
                })
                .collect()
        };
        let banner = banner_pixels(AGY_BANNER_1_4_4, size, false);
        assert_eq!(
            master(true),
            banner,
            "agy's master on a dark ground is its banner"
        );
        assert_eq!(
            master(false),
            banner,
            "and on paper: agy paints one banner on both"
        );
        let text = banner_pixels(AGY_BANNER_1_4_4, size, true);
        assert_ne!(text, banner, "CONTROL: the text alone is not the banner");
        assert_eq!(
            (text[1][5], banner[1][5]),
            (None, Some(0xF6_912E)),
            "CONTROL: read as text, the apex is hollow where agy paints it"
        );
    }

    /// What `mark`'s map shows on `ground` in a `w × h` buffer when its box
    /// sits at pixel `at`: each texel of the fitted master a whole
    /// `scale × scale` block, an ink opaque, a half-tone blended over the
    /// ground by the byte law, a hole exact, the rest the ground.
    fn map_pixels(
        mark: CliMark,
        f: Fit,
        ground: u32,
        at: (usize, usize),
        (w, h): (usize, usize),
    ) -> Vec<u32> {
        let b = brand(mark);
        let rows = b.master(f.master).expect("its master");
        let raw = b.icon.on(dark_ground(ground));
        let full = |i: u8| floor_contrast_keeping_hue(raw[usize::from(i)], ground, ICON_CONTRAST);
        let s = usize::from(f.scale);
        let (ox, oy) = (at.0 + usize::from(f.dx), at.1 + usize::from(f.dy));
        let mut px = vec![ground; w * h];
        for (ty, row) in rows.iter().enumerate() {
            for (tx, byte) in row.bytes().enumerate() {
                let Some(k) = b.chars.iter().position(|&c| c == byte) else {
                    continue;
                };
                for y in oy + ty * s..oy + (ty + 1) * s {
                    for x in ox + tx * s..ox + (tx + 1) * s {
                        let dst = &mut px[y * w + x];
                        *dst = match b.texels[k] {
                            Texel::Ink(i) => full(i),
                            Texel::Half(i) => {
                                over_premul(*dst, premul_rgb(full(i), HALF_ALPHA), HALF_ALPHA)
                            }
                            Texel::Hole(rgb) => rgb,
                        };
                    }
                }
            }
        }
        px
    }

    /// `mark`'s layer for `f` on `ground`, its first cell at `offset`, painted
    /// over the ground by the renderers' CPU path, every band open.
    fn painted(
        mark: CliMark,
        f: Fit,
        ground: u32,
        offset: (i32, i32),
        w: usize,
        ch: u16,
    ) -> Vec<u32> {
        let cell_h = usize::from(ch);
        let h = 3 * cell_h;
        let mut px = vec![ground; w * h];
        let frame = FxFrame {
            w,
            h,
            pad: 0,
            grid_top: 0,
            cell_h,
            rows: 3,
        };
        let pane = PxRect::new(0, 0, w as i32, h as i32);
        let laid = layer(7, content(mark, f, ground), offset, pane);
        paint_layers(&mut px, &frame, &[laid], &[]);
        px
    }

    /// CPU layer rendering of each mark at 7×14 and 15×28 matches its pixel map texel for texel.
    #[test]
    fn the_marks_are_their_maps_on_the_cpu() {
        for (cw, ch) in [(7u16, 14u16), (15, 28)] {
            let w = 3 * usize::from(ICON_CELLS) * usize::from(cw);
            let at = (usize::from(ICON_CELLS) * usize::from(cw), usize::from(ch));
            let offset = (at.0 as i32, at.1 as i32);
            for mark in CliMark::ALL {
                for ground in [ground("Default"), ground("Default Light"), CODEX_SLAB] {
                    let f = fit(mark, cw, ch).unwrap_or_else(|| panic!("{mark:?} fits"));
                    let want = map_pixels(mark, f, ground, at, (w, 3 * usize::from(ch)));
                    let got = painted(mark, f, ground, offset, w, ch);
                    let miss = got.iter().zip(&want).position(|(g, m)| g != m);
                    assert!(
                        miss.is_none(),
                        "{mark:?} at {cw}×{ch} on {ground:06x}: pixel {:?} is {:06x}, its map {:06x}",
                        miss.map(|i| (i % w, i / w)),
                        miss.map_or(0, |i| got[i]),
                        miss.map_or(0, |i| want[i]),
                    );
                    let off = painted(mark, f, ground, (offset.0 + 1, offset.1), w, ch);
                    assert_ne!(off, want, "CONTROL: {mark:?} one pixel off at {cw}×{ch}");
                }
            }
        }
    }
}
