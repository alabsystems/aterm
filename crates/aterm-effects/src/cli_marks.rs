// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TOP LINE'S MARKS: one drawn icon and one brand colour per agent CLI.
//! The owner: "each CLI has an emoji-sized icon and color on the left
//! side of the top line of CLI to standardize". Every agent CLI's top line
//! opens, two cells in, with the same LOCKUP: a pad cell, the icon over two
//! cells, a pad cell, the CLI's name in its brand ink, a trailing pad
//! (`──[##]claude ────`, [`CliMark::lockup_cells`]).
//!
//! This module is the art and its law, and draws nothing itself: the host
//! (the composer footer's top line) lays the cells and hands the renderer
//! [`layer`]. It holds:
//!
//! * **The brand table** ([`brand`]): one row per mark, one exhaustive match,
//!   so a new [`CliMark`] does not compile until its row is written. A row is
//!   the CLI's word, its name inks and icon inks for both polarities, and its
//!   masters: an SD drawing and, where one is drawn, an HD drawing at twice
//!   the design scale. Each master is art strings for
//!   [`aterm_render::fx::indexed`]: `'.'` is transparent and `chars[k]` is
//!   texel `k + 1`, which paints palette entry `k` ([`Texel`] says how).
//! * **The fit law** ([`fit`]): which master at which integer scale fills the
//!   icon's box (two cells wide, one row tall) with a pixel of air above and
//!   below. An integer scale makes whole texel blocks under pixel law F, so a
//!   mark is crisp at every scale and never sampled.
//! * **The inks** ([`name_fg`], [`icon_palette`]): the name is text, held to
//!   4.5:1 on its ground; the icon is a graphic, held to 3:1 (WCAG 1.4.11).
//!   Both floors move lightness alone ([`floor_contrast_keeping_hue`]), so the
//!   hue holds, and on a dark ground no brand ink moves. Polarity is the
//!   harness card's own test ([`dark_ground`] of the cell's background), so
//!   the icon, the name and the switch letters flip together.
//! * **The payload** ([`content`], [`layer`]): one sprite of the fitted
//!   master over that palette, sealed once per key; the host keeps the last
//!   few and hands the same `Arc` frame after frame, so a mark at rest costs
//!   the renderer one pointer compare.
//!
//! # Keyed by its own enum
//!
//! The brand table matches on [`CliMark`], not on `aterm_phase::Program`:
//! this crate is host-agnostic and does not link the phase reader (the host
//! reads the screen and hands the effects plain data, as it does the harness
//! card's place). A mark is named by the CLI's lowercase word
//! ([`CliMark::for_app`]), the same canonical app id the kitty registry keys
//! its flagship art by, and the same word `Program::name` prints; the host
//! maps its `Program` onto a mark with an exhaustive match of its own. That
//! is also why Grok has a row while the reader has no `Program::Grok`: its
//! word, inks and masters are ready as data, and nothing draws them until the
//! host can read a Grok top line and maps a program onto [`CliMark::Grok`].
//!
//! The brand marks identify the program running in the pane (nominative use
//! inside the terminal), never aterm's own branding.

use std::sync::{Arc, OnceLock};

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, PxRect,
};
use aterm_render::fx as fx_art;
use aterm_render::premul_rgb;

use crate::color_math::floor_contrast_keeping_hue;
use crate::harness_card::dark_ground;

/// The contrast a CLI's name holds on its ground (WCAG 1.4.3, text).
pub const NAME_CONTRAST: f32 = 4.5;

/// The contrast every icon ink but a [`Texel::Hole`] holds on its ground
/// (WCAG 1.4.11, graphics).
pub const ICON_CONTRAST: f32 = 3.0;

/// The opacity of a half-tone texel ([`Texel::Half`]): its full ink at
/// alpha 128, premultiplied, an antialiased edge over any ground.
pub const HALF_ALPHA: u8 = 128;

/// The cells the icon's box spans: the sprite is drawn over two blank cells.
pub const ICON_CELLS: u16 = 2;

/// The rows of air the fit keeps clear inside the box, one pixel above and one
/// below, so a mark never touches the rows around it.
pub const AIR_PX: u16 = 2;

/// The cells the icon takes in the lockup without the name: a pad, the
/// [`ICON_CELLS`] the sprite covers, a pad.
pub const ICON_RUN: usize = 2 + ICON_CELLS as usize;

/// An agent CLI that wears a mark on its top line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CliMark {
    /// Claude Code: its whole mascot, in its terracotta.
    Claude,
    /// Codex (and codex-alab, whose lockup names it apart): the OpenAI
    /// knot, in OpenAI blue.
    Codex,
    /// Google's Antigravity CLI: its own launch banner, in the colours it
    /// paints it.
    Agy,
    /// Meta's Muse Code: the `◈` its status row cycles.
    Muse,
    /// Google's Gemini CLI: the four-point sparkle in its own gradient.
    Gemini,
    /// xAI's Grok: the slashed ring, monochrome. No reader maps a program
    /// onto it yet.
    Grok,
}

impl CliMark {
    /// Every mark, in declaration order (each at its own discriminant).
    pub const ALL: [CliMark; 6] = [
        CliMark::Claude,
        CliMark::Codex,
        CliMark::Agy,
        CliMark::Muse,
        CliMark::Gemini,
        CliMark::Grok,
    ];

    /// The mark for a CLI's lowercase word (the canonical app id
    /// `kitty_registry` keys flagship art by, and `Program::name`'s word);
    /// `None` for any other program.
    #[must_use]
    pub fn for_app(app_id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.word() == app_id)
    }

    /// The CLI's own lowercase word: its canonical app id, and the name a
    /// vendor build's lockup prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        brand(self).word
    }

    /// The name the lockup prints for `build`: the ALab build's own word
    /// where the brand table names one apart ([`Brand::alab_word`]:
    /// `codex-alab`), else the CLI's [`word`](Self::word).
    #[must_use]
    pub const fn name(self, build: Build) -> &'static str {
        match (build, brand(self).alab_word) {
            (Build::Alab, Some(word)) => word,
            _ => self.word(),
        }
    }

    /// The cells the whole lockup takes for `build`: [`ICON_RUN`], the
    /// [`name`](Self::name), a trailing pad. The same with or without a
    /// sprite drawn ([`fit`] refusing one leaves blank cells), so the layout
    /// never moves.
    #[must_use]
    pub const fn lockup_cells(self, build: Build) -> usize {
        ICON_RUN + self.name(build).len() + 1
    }
}

/// Which build of a CLI a pane runs, as the host proves it: the vendor's, or
/// ALab's own (codex-alab: a Codex the host admits, by its proven
/// self-claim, aterm's copy of its record or its ALab image). The icon and
/// the inks are the CLI's either way; only the name the lockup prints may
/// differ ([`CliMark::name`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Build {
    /// The vendor's build, or one the host has not proven ALab's.
    #[default]
    Vendor,
    /// ALab's build, proven.
    Alab,
}

impl Build {
    /// Both builds, the vendor's first.
    pub const ALL: [Build; 2] = [Build::Vendor, Build::Alab];
}

/// How a master's character paints: the palette entry its texel names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Texel {
    /// Icon ink `i` of the polarity ([`Brand::icon`]), floored to
    /// [`ICON_CONTRAST`], opaque.
    Ink(u8),
    /// Icon ink `i`, floored as [`Texel::Ink`] floors it, at [`HALF_ALPHA`].
    Half(u8),
    /// A hole the mark paints in exactly this `0x00RRGGBB`, never floored:
    /// Claude's black eyes, as Claude paints them.
    Hole(u32),
}

/// A pair of ink lists, one per polarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inks {
    /// On a dark ground.
    pub dark: &'static [u32],
    /// On paper.
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

    /// The list for a ground of polarity `dark`.
    #[must_use]
    pub const fn on(self, dark: bool) -> &'static [u32] {
        if dark { self.dark } else { self.paper }
    }
}

/// Which drawing of a mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Master {
    /// The standard drawing, design scale 1.
    Sd,
    /// The fine drawing, design scale 2 (twice the SD's texels a side).
    Hd,
}

impl Master {
    /// The scale the drawing was designed at: one HD texel is half an SD one.
    #[must_use]
    pub const fn design_scale(self) -> u16 {
        match self {
            Master::Sd => 1,
            Master::Hd => 2,
        }
    }
}

/// One row of the brand table.
#[derive(Debug, Clone, Copy)]
pub struct Brand {
    /// The CLI's word: the name a vendor build's lockup prints.
    pub word: &'static str,
    /// The name an ALab build's lockup prints, where aterm tells that build
    /// apart (the owner calls aterm's Codex build "codex-alab"); `None`: the
    /// [`word`](Self::word) for every build.
    pub alab_word: Option<&'static str>,
    /// The name's inks before the floor: one for the whole word, or one a
    /// letter.
    pub name: Inks,
    /// The icon's full inks before the floor, indexed by [`Texel`].
    pub icon: Inks,
    /// The masters' characters: `chars[k]` is texel `k + 1`.
    pub chars: &'static [u8],
    /// How each character paints: `texels[k]` is `chars[k]`'s.
    pub texels: &'static [Texel],
    /// The standard drawing.
    pub sd: &'static [&'static str],
    /// The fine drawing, where one is drawn.
    pub hd: Option<&'static [&'static str]>,
}

impl Brand {
    /// The drawing `master`, if this mark has one.
    #[must_use]
    pub const fn master(&self, master: Master) -> Option<&'static [&'static str]> {
        match master {
            Master::Sd => Some(self.sd),
            Master::Hd => self.hd,
        }
    }
}

/// The brand table: each mark's row. One exhaustive match, so a new mark does
/// not compile until its row is written.
#[must_use]
pub const fn brand(mark: CliMark) -> &'static Brand {
    match mark {
        CliMark::Claude => &CLAUDE,
        CliMark::Codex => &CODEX,
        CliMark::Agy => &AGY,
        CliMark::Muse => &MUSE,
        CliMark::Gemini => &GEMINI,
        CliMark::Grok => &GROK,
    }
}

/// Claude Code. MEASURED: the mascot's `38;2;215;119;87` and the eyes'
/// `48;2;0;0;0` (2.1.304), on every ground.
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

/// Codex: the owner's "OpenAI blue". Codex itself draws no brand colour
/// (`>_ OpenAI Codex` in bold, `/model` in ANSI cyan, 0.154). ALab's build
/// prints the owner's name for it, `codex-alab`, in the same ink.
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

/// agy. MEASURED on screen (1.4.4 in the real-agy lab, 2026-10-21, `ctl
/// cell` of every banner cell): the banner is one fixed two-dimensional
/// gradient, the same on a dark and a light ground and at every launch —
/// gold to red over the apex, green to teal down the left leg, violet to
/// blue down the right — and its `▀` cells carry a colour in their
/// background too, so the logo is a solid `A`, not an outline. The icon is
/// that banner texel for texel ([`AGY_SD`]: one ink a texel, in the order
/// they first appear, row by row). The name takes the ink agy prints its
/// own product name in (`Antigravity CLI`, bold): `#5c5cff` on a dark
/// ground, `#004da3` on paper. The ramp strings in its binary
/// (`#FF7518…#9B5DE5` and the deep `#C2410C…#6D28D9`) are not what the
/// banner draws.
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

/// Muse Code: the name and the diamond's outline in Meta blue `#0082fb`,
/// the brand main's footer gave Muse on 2026-10-21 (its blue diamond, "the
/// whole brand is Meta blue, not purple"), which replaced the violet
/// `#a882ff` this mark first kept; the core is Muse's own lighter blue,
/// MEASURED from 1.4.10's caret `❯` and its bold argv rows. The edge `n` is
/// the outline at half alpha.
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

/// Gemini CLI. MEASURED: 0.46.0's `GradientColors: ["#4796E4", "#847ACE",
/// "#C3677F"]` (its default theme, light and dark). The name's letter `k` of
/// six sits at `t = k/5` along it, the icon's eight stops at `t = k/7`, one a
/// band of columns left to right; `a`–`h` are stops 1–8 at half alpha.
const GEMINI: Brand = Brand {
    word: "gemini",
    alab_word: None,
    name: Inks::both(&[
        0x47_96E4, 0x5F_8BDB, 0x78_80D2, 0x91_76BE, 0xAA_6F9F, 0xC3_677F,
    ]),
    icon: Inks::both(&[
        0x47_96E4, 0x58_8EDE, 0x6A_86D7, 0x7B_7ED1, 0x8D_77C3, 0x9F_72AC, 0xB1_6C96, 0xC3_677F,
    ]),
    chars: b"12345678abcdefgh",
    texels: &[
        Texel::Ink(0),
        Texel::Ink(1),
        Texel::Ink(2),
        Texel::Ink(3),
        Texel::Ink(4),
        Texel::Ink(5),
        Texel::Ink(6),
        Texel::Ink(7),
        Texel::Half(0),
        Texel::Half(1),
        Texel::Half(2),
        Texel::Half(3),
        Texel::Half(4),
        Texel::Half(5),
        Texel::Half(6),
        Texel::Half(7),
    ],
    sd: GEMINI_SD,
    hd: Some(GEMINI_HD),
};

/// Grok: xAI's monochrome, flipped with the ground. Ready as data; a take of
/// Grok's own screen confirms the ink before a program maps onto it.
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

// Every row's inks are well formed, checked at compile time, so no ink read
// below can miss: a texel per character, every texel's ink in both lists,
// both icon lists one length, and a name ink for the word or for each letter.
const _: () = {
    let mut m = 0;
    while m < CliMark::ALL.len() {
        let b = brand(CliMark::ALL[m]);
        assert!(b.texels.len() == b.chars.len());
        assert!(b.icon.dark.len() == b.icon.paper.len() && !b.icon.dark.is_empty());
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
        // An ALab build's name has letters, and inks for them: one for the
        // whole name, or one a letter of it.
        if let Some(alab) = b.alab_word {
            let a = alab.len();
            assert!(a > 0);
            assert!(b.name.dark.len() == 1 || b.name.dark.len() == a);
            assert!(b.name.paper.len() == 1 || b.name.paper.len() == a);
        }
        m += 1;
    }
};

/// Where a master sits in the icon's box: the fit law's choice ([`fit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fit {
    /// The drawing.
    pub master: Master,
    /// Its integer scale: one texel is `scale × scale` pixels.
    pub scale: u16,
    /// The drawn width in pixels (`w · scale`).
    pub dw: u16,
    /// The drawn height in pixels (`h · scale`).
    pub dh: u16,
    /// The drawing's left edge in the box.
    pub dx: u16,
    /// The drawing's top edge in the box.
    pub dy: u16,
}

impl Fit {
    /// The effective scale: the design scale times the drawn scale, in SD
    /// texels' worth of pixels per texel. The fit law maximises it.
    #[must_use]
    pub const fn effective(self) -> u32 {
        self.master.design_scale() as u32 * self.scale as u32
    }
}

/// THE FIT LAW: where `mark` sits in the icon's box of a `cw × ch` cell (the
/// box is `2·cw` wide, its [`ICON_CELLS`], and `ch` tall), or `None` when no
/// master fits, and the lockup keeps its cells with no sprite.
///
/// The candidates are every master at every integer scale `s ≥ 1` with
/// `w·s ≤ 2·cw` and `h·s ≤ ch − 2` ([`AIR_PX`]); the choice is the largest
/// [`Fit::effective`] scale, a tie going to the HD master, the finer drawing;
/// the placement is centred, floored. An integer scale is whole texel
/// blocks, so no scale blurs.
#[must_use]
pub fn fit(mark: CliMark, cw: u16, ch: u16) -> Option<Fit> {
    let box_w = u32::from(ICON_CELLS) * u32::from(cw);
    let box_h = u32::from(ch);
    let room_h = box_h.checked_sub(u32::from(AIR_PX))?;
    let b = brand(mark);
    let mut best: Option<Fit> = None;
    for master in [Master::Sd, Master::Hd] {
        let Some(rows) = b.master(master) else {
            continue;
        };
        let (w, h) = (
            rows.first().map_or(0, |r| r.len()) as u32,
            rows.len() as u32,
        );
        if w == 0 || h == 0 {
            continue;
        }
        // The largest scale in the box, and in a sprite's u16 sides.
        let most = u32::from(u16::MAX);
        let s = (box_w / w).min(room_h / h).min(most / w).min(most / h);
        let Ok(scale) = u16::try_from(s) else {
            continue;
        };
        if scale == 0 {
            continue;
        }
        let (dw, dh) = (w * s, h * s);
        let cand = Fit {
            master,
            scale,
            dw: dw as u16,
            dh: dh as u16,
            dx: ((box_w - dw) / 2) as u16,
            dy: ((box_h - dh) / 2) as u16,
        };
        // SD is offered first, so an equal HD replaces it.
        if best.is_none_or(|kept| cand.effective() >= kept.effective()) {
            best = Some(cand);
        }
    }
    best
}

/// `mark`'s drawing `master` as an interned Index8 art, built once per
/// process; `None` where the mark has no such drawing.
#[must_use]
pub fn art(mark: CliMark, master: Master) -> Option<&'static Arc<FxArt>> {
    static ARTS: [OnceLock<[Option<Arc<FxArt>>; 2]>; CliMark::ALL.len()] =
        [const { OnceLock::new() }; CliMark::ALL.len()];
    let arts = ARTS[mark as usize].get_or_init(|| {
        let b = brand(mark);
        [Master::Sd, Master::Hd]
            .map(|m| b.master(m).and_then(|rows| fx_art::indexed(rows, b.chars)))
    });
    arts[master as usize].as_ref()
}

/// The ink of letter `letter` of `mark`'s name on a ground of polarity
/// `dark`, before the floor (the last letter's for a letter past the word).
#[must_use]
pub fn name_ink(mark: CliMark, letter: usize, dark: bool) -> u32 {
    let inks = brand(mark).name.on(dark);
    inks[letter.min(inks.len() - 1)]
}

/// The ink letter `letter` of `mark`'s name is drawn in on `ground`
/// (`0x00RRGGBB`): its brand ink for the ground's polarity, floored to
/// [`NAME_CONTRAST`] with its hue held.
#[must_use]
pub fn name_fg(mark: CliMark, letter: usize, ground: u32) -> u32 {
    let ink = name_ink(mark, letter, dark_ground(ground));
    floor_contrast_keeping_hue(ink, ground, NAME_CONTRAST)
}

/// `mark`'s icon palette on `ground` (`0x00RRGGBB`): entry `k` paints texel
/// `k + 1`, as [`Texel`] says. Each full ink is the brand's for the ground's
/// polarity, floored to [`ICON_CONTRAST`] with its hue held, so on every dark
/// built-in ground it is the brand's exact bytes; a half-tone is its full
/// ink's floored value at [`HALF_ALPHA`]; a hole is never floored.
#[must_use]
pub fn icon_palette(mark: CliMark, ground: u32) -> Vec<FxInk> {
    let b = brand(mark);
    let inks = b.icon.on(dark_ground(ground));
    let floored = |i: u8| floor_contrast_keeping_hue(inks[usize::from(i)], ground, ICON_CONTRAST);
    b.texels
        .iter()
        .map(|&t| match t {
            Texel::Ink(i) => FxInk::over(floored(i), 255),
            Texel::Half(i) => FxInk::over(premul_rgb(floored(i), HALF_ALPHA), HALF_ALPHA),
            Texel::Hole(rgb) => FxInk::over(rgb, 255),
        })
        .collect()
}

/// `mark`'s payload for one icon box: one sprite of the fitted master at
/// `(dx, dy)`, palette base 0, over [`icon_palette`] on `ground`, sealed.
/// Box-local, so the host lays it at the icon's first cell ([`layer`]). Every
/// call seals a new content: the host keeps the last few by
/// `(mark, fit, ground)` and hands the kept `Arc` at rest. Empty for a
/// master the mark does not have.
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

/// The mark's layer: `content` over the glyphs (the slot the card's kitty
/// is in), in grid pixels, by the byte-exact law, so the CPU and the GPU
/// draw the same bytes. `offset` is the icon's first cell
/// (`(col · cw, row · ch)`), and `clip` the pane's box, so a mark never draws
/// past its pane.
#[must_use]
pub fn layer(id: u64, content: Arc<FxContent>, offset: (i32, i32), clip: PxRect) -> FxLayer {
    let mut layer = FxLayer::new(id, content, FxSlot::Over, FxSpace::Grid, FxBlend::Exact);
    layer.offset = offset;
    layer.clip = clip;
    layer
}

// ---- The masters ------------------------------------------------------------
//
// Transcribed from the judged spec's pixel maps (top-line icons, §2.4). One
// string a row; `'.'` is transparent and every other byte is one of its
// brand's `chars`. Upper case and digits are full ink; Codex's `o`, Muse's
// `n`, Gemini's `a`–`h` and Grok's `g` are half-tones; Claude's `k` is its
// eyes' hole and Muse's `m` its core.

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

/// agy's own launch banner (` ▄▀▀▄` / `▀▀▀▀▀▀` / `▀▀▀▀▀▀▀▀` /
/// `▄▀▀    ▀▀▄` / `▄▀▀      ▀▀▄`, 1.4.0 to 1.4.4), one banner half-cell a
/// pixel in the colour 1.4.4 paints it: a `▀` is its foreground over its
/// background, so the apex and the bar are solid and only the two cells
/// under the bar's middle are the ground. The one master: at 2x the law
/// draws it at ×2.
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

/// Gemini CLI's four-point sparkle, SD 12×12: two-pixel arms tapering to
/// half-tone tips, column `k` in gradient stop `1 + round(k·7/11)`.
const GEMINI_SD: &[&str] = &[
    ".....de.....",
    ".....45.....",
    ".....45.....",
    "....4455....",
    "...344556...",
    "a2234455677h",
    "a2234455677h",
    "...344556...",
    "....4455....",
    ".....45.....",
    ".....45.....",
    ".....de.....",
];

/// Gemini CLI's four-point sparkle, HD 24×24: an astroid (pinch 0.70),
/// column `k` in gradient stop `1 + round(k·7/23)`.
const GEMINI_HD: &[&str] = &[
    "........................",
    "...........de...........",
    "...........45...........",
    "..........d45e..........",
    "..........4455..........",
    ".........444555.........",
    "........c444555f........",
    ".......c34445556f.......",
    "......c3344455566f......",
    ".....33334445556666.....",
    "...b2333344455566667g...",
    ".a22233334445556666777h.",
    ".a22233334445556666777h.",
    "...b2333344455566667g...",
    ".....33334445556666.....",
    "......c3344455566f......",
    ".......c34445556f.......",
    "........c444555f........",
    ".........444555.........",
    "..........4455..........",
    "..........d45e..........",
    "...........45...........",
    "...........de...........",
    "........................",
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

    /// ICONS READ AT 3:1 ON EVERY GROUND (spec §3.3, WCAG 1.4.11): on every
    /// built-in theme's ground and on Codex's slab, each full icon ink is
    /// floored to 3:1 or better with its hue held, a half-tone is its full
    /// ink's floored value at half alpha, a hole is its exact bytes, and on
    /// Default every ink is the brand's exact bytes.
    /// NEGATIVE CONTROL: Muse's raw core on Gruvbox Light is under 3:1
    /// (2.34), so there the floor does the work.
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

    /// NAMES READ AT 4.5:1 ON EVERY GROUND (spec §3.2, WCAG 1.4.3): every
    /// letter of every name a lockup prints — each mark's word, and
    /// codex-alab's longer name — on every built-in theme's ground and on
    /// Codex's slab, is drawn in an ink that reads at 4.5:1 or better, its
    /// HSV hue within 2° of the brand's own; on Default each is the brand's
    /// exact bytes wherever those already read.
    /// NEGATIVE CONTROLS: Claude's raw `#d77757` on Default Light reads under
    /// 4.5:1 (2.94), so there the floor does the work; and codex-alab's
    /// last letter, past the vendor's word, reads under 4.5:1 in Codex's raw
    /// blue on Codex's slab (3.65), so there too.
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

    /// EVERY BUILD'S NAME IS ITS OWN (the owner calls aterm's Codex build
    /// "codex-alab"): codex-alab's lockup prints `codex-alab`, five cells
    /// wider than vendor `codex`'s, and every other mark prints its word for
    /// either build — none names an ALab build apart.
    /// NEGATIVE CONTROL: the vendor build of Codex prints `codex`.
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

    /// EVERY MARK IS CRISP AND CENTRED (spec §2.2–§2.4): every master is
    /// rectangular, holds only its chars, builds as an art of its size, and
    /// keeps its symmetry (Claude, agy, Muse and Gemini mirror; Codex, Muse
    /// and Gemini turn half round); and the fit at each measured cell is
    /// §2.2's: the master, an integer scale, whole texel blocks inside the
    /// box with a pixel of air above and below, centred.
    /// NEGATIVE CONTROL: a 6×12 cell (box 12×12, so a mark may be at most 12
    /// wide and 10 tall) refuses every master but agy's, the lockup keeping
    /// its cells with no sprite; a ragged master and a stray byte are faults.
    #[test]
    fn every_mark_is_crisp_and_centred() {
        use CliMark::{Agy, Claude, Codex, Gemini, Grok, Muse};
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
                if matches!(mark, Claude | Agy | Muse | Gemini) {
                    assert!(mirrored(rows), "{mark:?} {master:?} mirrors");
                }
                if matches!(mark, Codex | Muse | Gemini) {
                    assert!(turned(rows), "{mark:?} {master:?} turns half round");
                }
            }
        }
        // §2.2's table, with the 9×23 Windows cell its law gives: the cell,
        // the mark, its master and scale, and where it sits in the box.
        let table = [
            ((7, 14), Claude, Sd, 1, (0, 2)),
            ((7, 14), Codex, Sd, 1, (1, 1)),
            ((7, 14), Agy, Sd, 1, (1, 2)),
            ((7, 14), Muse, Sd, 1, (1, 1)),
            ((7, 14), Gemini, Sd, 1, (1, 1)),
            ((7, 14), Grok, Sd, 1, (1, 1)),
            ((9, 23), Claude, Sd, 1, (2, 6)),
            ((9, 23), Codex, Sd, 1, (3, 5)),
            ((9, 23), Agy, Sd, 1, (3, 6)),
            ((9, 23), Muse, Sd, 1, (3, 5)),
            ((9, 23), Gemini, Sd, 1, (3, 5)),
            ((9, 23), Grok, Sd, 1, (3, 5)),
            ((11, 21), Claude, Sd, 1, (4, 5)),
            ((11, 21), Codex, Sd, 1, (5, 4)),
            ((11, 21), Agy, Sd, 1, (5, 5)),
            ((11, 21), Muse, Sd, 1, (5, 4)),
            ((11, 21), Gemini, Sd, 1, (5, 4)),
            ((11, 21), Grok, Sd, 1, (5, 4)),
            ((15, 28), Claude, Hd, 1, (1, 6)),
            ((15, 28), Codex, Hd, 1, (3, 2)),
            ((15, 28), Agy, Sd, 2, (3, 4)),
            ((15, 28), Muse, Hd, 1, (3, 2)),
            ((15, 28), Gemini, Hd, 1, (3, 2)),
            ((15, 28), Grok, Hd, 1, (3, 2)),
            ((22, 42), Claude, Sd, 3, (1, 6)),
            ((22, 42), Codex, Sd, 3, (4, 3)),
            ((22, 42), Agy, Sd, 3, (4, 6)),
            ((22, 42), Muse, Sd, 3, (4, 3)),
            ((22, 42), Gemini, Sd, 3, (4, 3)),
            ((22, 42), Grok, Sd, 3, (4, 3)),
            ((30, 56), Claude, Hd, 2, (2, 12)),
            ((30, 56), Codex, Hd, 2, (6, 4)),
            ((30, 56), Agy, Sd, 5, (0, 3)),
            ((30, 56), Muse, Hd, 2, (6, 4)),
            ((30, 56), Gemini, Hd, 2, (6, 4)),
            ((30, 56), Grok, Hd, 2, (6, 4)),
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

    /// agy 1.4.4's launch banner as the real-agy lab read it (`aterm ctl cell`
    /// of every cell, 2026-10-21; the same on Default and Default Light and
    /// at a second launch): each cell's column and row from the banner's
    /// top left, its glyph, its foreground, and its background (`None`: the
    /// pane's own ground).
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
    /// ground over its foreground. `as_text` reads it as its glyphs alone, a
    /// `▀`'s lower half always the ground.
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

    /// AGY'S MARK IS ITS BANNER (spec §2.4 and §10.2.3: the lab's cell read
    /// decides): agy's master, each texel in its raw ink, on either
    /// polarity, is the banner 1.4.4 paints, one half-cell a pixel.
    /// NEGATIVE CONTROL: the banner read as text alone (each `▀`'s
    /// background dropped, all a text fixture shows) is a hollow arch, not
    /// the master: the reading the first drawing was made from.
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

    /// THE MARKS ARE THEIR MAPS ON THE CPU (spec §2.3, §6.4): each mark's
    /// layer at 7×14 and 15×28, laid at a cell of a pane on Default, Default
    /// Light and Codex's slab, paints exactly its map, texel for texel,
    /// half-tones blended exactly, and nothing outside its box.
    /// NEGATIVE CONTROL: the same layer laid one pixel off is caught.
    #[test]
    fn the_marks_are_their_maps_on_the_cpu() {
        for (cw, ch) in [(7u16, 14u16), (15, 28)] {
            // Three boxes by three rows; the mark's box is the middle one.
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
