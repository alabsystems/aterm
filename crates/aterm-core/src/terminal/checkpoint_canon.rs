// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Canonical bytes of a [`TerminalCheckpoint`] and a [`TerminalConfig`]: every field classified, one encoding per value.

use core::fmt;

use aterm_parser::{HookedDcsCarry, ParserCarry, State, TmuxPassthrough};
use aterm_types::charset::{
    CharacterSet, CharacterSet96, CharacterSetState, GlMapping, GrMapping, SingleShift,
};
use aterm_types::mouse::{MouseEncoding, MouseMode};
use aterm_types::{
    Appearance, BiDiMode, ColorPalette, CursorStyle, ImageFormat, ImageScaling, KittyKeyboardFlags,
    KittyKeyboardStateSnapshot, ParagraphDirection, Rgb, SourceRect, TaskbarProgress,
    TerminalModes, VtLevel, XtermKeyboardState,
};

use super::checkpoint::{
    DcsQueryCarry, DcsQueryKind, GridCursorRepr, KittyFloatParentCarry, KittyFloatingCarry,
    KittyFrameCarry, KittyImageCarry, KittyLayoutCarry, KittyParentCarry, KittyPlayStateCarry,
    KittyRelationCarry, KittyVariantCarry, KittyVariantSourceCarry, KittyVirtualCarry,
    KittyVirtualImageCarry, LooseImageCarry, PANE_HISTORY_IMAGE_LINES, PaneStepCarry,
    PlacedGridCarry, PlacedImageSourceCarry, PlacedSpanCarry, SavedCursorPendingWrap,
    SavedCursorRepr, SavedCursorResized, ShellIntegrationNonce, StyleRepr, TerminalCheckpoint,
};
use super::checkpoint_holder::{
    HolderAnchorRepr, HolderMarkRepr, HolderRepr, HolderSealRepr, HolderSpanRepr,
};
use super::checkpoint_output::{
    HyperlinkRepr, OutputStateRepr, SavedModeRepr, SgrPushRepr, Vt52AddressRepr,
};
use super::checkpoint_state::{
    ColorOverridesRepr, ColorRepr, ColorSlotRepr, ColorStackEntryRepr, CwdReportRepr,
    OutputBlockRepr, PaletteOverrideRepr, ShellRepr, ShellSpanRepr, TitleRepr,
};
use crate::config::{BiDiConfig, TerminalConfig};
use crate::grid::{CellFlags, MAX_GRID_COLS, MAX_GRID_ROWS, PackedColor};
use crate::scrollback::{deserialize_lines_strict, max_linked_record_bytes};

/// The version [`TerminalCheckpoint::canonical`] writes after its magic.
pub const CHECKPOINT_CANON_VERSION: u32 = 1;

/// The version [`TerminalConfig::canonical`] writes after its magic.
pub const CONFIG_CANON_VERSION: u32 = 1;

/// The version [`PaneStepCarry::canonical`] writes after its magic.
pub const PANE_CARRY_CANON_VERSION: u32 = 6;

/// The version [`policy_canonical`] writes after its magic.
pub const POLICY_CANON_VERSION: u32 = 1;

const CHECKPOINT_MAGIC: &[u8] = b"aterm-checkpoint-canon\0";
const CONFIG_MAGIC: &[u8] = b"aterm-config-canon\0";
const PANE_CARRY_MAGIC: &[u8] = b"aterm-pane-carry-canon\0";
const POLICY_MAGIC: &[u8] = b"aterm-policy-canon\0";

/// The tag a value of a foreign enum this build does not name encodes as, followed by its `Debug` text; decoding refuses it.
const UNNAMED: u8 = 0xFF;

/// Why bytes are not the canonical encoding of a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonFault {
    /// The field the decode stopped at.
    pub field: &'static str,
    /// What is wrong with it.
    pub why: &'static str,
}

impl fmt::Display for CanonFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not canonical at `{}`: {}", self.field, self.why)
    }
}

impl std::error::Error for CanonFault {}

const fn fault(field: &'static str, why: &'static str) -> CanonFault {
    CanonFault { field, why }
}

struct W(Vec<u8>);

impl W {
    fn raw(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.raw(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.raw(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.raw(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.raw(&v.to_le_bytes());
    }
    fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }
    fn len(&mut self, n: usize) {
        self.u64(u64::try_from(n).unwrap_or(u64::MAX));
    }
    fn bytes(&mut self, b: &[u8]) {
        self.len(b.len());
        self.raw(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn opt<T>(&mut self, v: Option<T>, f: impl FnOnce(&mut Self, T)) {
        match v {
            None => self.u8(0),
            Some(x) => {
                self.u8(1);
                f(self, x);
            }
        }
    }
    fn rgb(&mut self, c: Rgb) {
        self.raw(&[c.r, c.g, c.b]);
    }
    fn opt_rgb(&mut self, c: Option<Rgb>) {
        self.opt(c, Self::rgb);
    }
    fn opt_u64(&mut self, v: Option<u64>) {
        self.opt(v, Self::u64);
    }
    fn opt_str(&mut self, v: Option<&str>) {
        self.opt(v, Self::str);
    }
    fn unnamed(&mut self, debug: impl fmt::Debug) {
        self.u8(UNNAMED);
        self.str(&format!("{debug:?}"));
    }
}

struct R<'a> {
    b: &'a [u8],
}

impl<'a> R<'a> {
    fn take(&mut self, field: &'static str, n: usize) -> Result<&'a [u8], CanonFault> {
        if self.b.len() < n {
            return Err(fault(field, "the bytes end inside it"));
        }
        let (head, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(head)
    }
    fn arr<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], CanonFault> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(field, N)?);
        Ok(out)
    }
    fn u8(&mut self, field: &'static str) -> Result<u8, CanonFault> {
        Ok(self.arr::<1>(field)?[0])
    }
    fn u16(&mut self, field: &'static str) -> Result<u16, CanonFault> {
        Ok(u16::from_le_bytes(self.arr(field)?))
    }
    fn u32(&mut self, field: &'static str) -> Result<u32, CanonFault> {
        Ok(u32::from_le_bytes(self.arr(field)?))
    }
    fn u64(&mut self, field: &'static str) -> Result<u64, CanonFault> {
        Ok(u64::from_le_bytes(self.arr(field)?))
    }
    fn i32(&mut self, field: &'static str) -> Result<i32, CanonFault> {
        Ok(i32::from_le_bytes(self.arr(field)?))
    }
    fn bool(&mut self, field: &'static str) -> Result<bool, CanonFault> {
        match self.u8(field)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(fault(field, "a bool is 0 or 1")),
        }
    }
    /// A length of elements each at least `min` bytes, bounded by what is left.
    fn count(&mut self, field: &'static str, min: usize) -> Result<usize, CanonFault> {
        let n =
            usize::try_from(self.u64(field)?).map_err(|_| fault(field, "a length past usize"))?;
        if n.saturating_mul(min.max(1)) > self.b.len() {
            return Err(fault(field, "a length past the bytes left"));
        }
        Ok(n)
    }
    fn bytes(&mut self, field: &'static str) -> Result<&'a [u8], CanonFault> {
        let n = self.count(field, 1)?;
        self.take(field, n)
    }
    fn string(&mut self, field: &'static str) -> Result<String, CanonFault> {
        let b = self.bytes(field)?;
        core::str::from_utf8(b)
            .map(str::to_owned)
            .map_err(|_| fault(field, "a string is UTF-8"))
    }
    fn opt<T>(
        &mut self,
        field: &'static str,
        f: impl FnOnce(&mut Self) -> Result<T, CanonFault>,
    ) -> Result<Option<T>, CanonFault> {
        match self.u8(field)? {
            0 => Ok(None),
            1 => f(self).map(Some),
            _ => Err(fault(field, "an option tag is 0 or 1")),
        }
    }
    fn vec<T>(
        &mut self,
        field: &'static str,
        min: usize,
        mut f: impl FnMut(&mut Self) -> Result<T, CanonFault>,
    ) -> Result<Vec<T>, CanonFault> {
        let n = self.count(field, min)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(f(self)?);
        }
        Ok(out)
    }
    fn rgb(&mut self, field: &'static str) -> Result<Rgb, CanonFault> {
        let [r, g, b] = self.arr(field)?;
        Ok(Rgb { r, g, b })
    }
    fn opt_rgb(&mut self, field: &'static str) -> Result<Option<Rgb>, CanonFault> {
        self.opt(field, |d| d.rgb(field))
    }
    fn opt_u64(&mut self, field: &'static str) -> Result<Option<u64>, CanonFault> {
        self.opt(field, |d| d.u64(field))
    }
    fn opt_string(&mut self, field: &'static str) -> Result<Option<String>, CanonFault> {
        self.opt(field, |d| d.string(field))
    }
    fn magic(&mut self, field: &'static str, magic: &[u8], version: u32) -> Result<(), CanonFault> {
        if self.take(field, magic.len())? != magic {
            return Err(fault(field, "the magic is not this encoding's"));
        }
        if self.u32(field)? != version {
            return Err(fault(field, "a version this build does not read"));
        }
        Ok(())
    }
    fn end(&self, field: &'static str) -> Result<(), CanonFault> {
        if self.b.is_empty() {
            Ok(())
        } else {
            Err(fault(field, "bytes follow the value"))
        }
    }
}

fn enc_cursor_style(w: &mut W, v: CursorStyle) {
    w.u8(v as u8);
}

fn dec_cursor_style(d: &mut R<'_>, field: &'static str) -> Result<CursorStyle, CanonFault> {
    Ok(match d.u8(field)? {
        1 => CursorStyle::BlinkingBlock,
        2 => CursorStyle::SteadyBlock,
        3 => CursorStyle::BlinkingUnderline,
        4 => CursorStyle::SteadyUnderline,
        5 => CursorStyle::BlinkingBar,
        6 => CursorStyle::SteadyBar,
        7 => CursorStyle::Hidden,
        8 => CursorStyle::HollowBlock,
        9 => CursorStyle::Bolt,
        _ => return Err(fault(field, "a cursor style this build does not name")),
    })
}

fn dec_mouse_mode(d: &mut R<'_>) -> Result<MouseMode, CanonFault> {
    Ok(match d.u8("modes.mouse_mode")? {
        0 => MouseMode::None,
        1 => MouseMode::Normal,
        2 => MouseMode::ButtonEvent,
        3 => MouseMode::AnyEvent,
        4 => MouseMode::X10,
        _ => return Err(fault("modes.mouse_mode", "a mode this build does not name")),
    })
}

fn dec_mouse_encoding(d: &mut R<'_>) -> Result<MouseEncoding, CanonFault> {
    Ok(match d.u8("modes.mouse_encoding")? {
        0 => MouseEncoding::X10,
        1 => MouseEncoding::Utf8,
        2 => MouseEncoding::Sgr,
        3 => MouseEncoding::Urxvt,
        4 => MouseEncoding::SgrPixel,
        _ => {
            return Err(fault(
                "modes.mouse_encoding",
                "an encoding this build does not name",
            ));
        }
    })
}

fn dec_vt_level(d: &mut R<'_>) -> Result<VtLevel, CanonFault> {
    Ok(match d.u8("modes.vt_level")? {
        0 => VtLevel::VT100,
        1 => VtLevel::VT220,
        2 => VtLevel::VT240,
        24 => VtLevel::VT320,
        18 => VtLevel::VT330,
        19 => VtLevel::VT340,
        41 => VtLevel::VT420,
        61 => VtLevel::VT510,
        64 => VtLevel::VT520,
        65 => VtLevel::VT525,
        _ => return Err(fault("modes.vt_level", "a level this build does not name")),
    })
}

fn enc_appearance(w: &mut W, v: Appearance) {
    w.u8(match v {
        Appearance::Dark => 0,
        Appearance::Light => 1,
    });
}

fn dec_appearance(d: &mut R<'_>, field: &'static str) -> Result<Appearance, CanonFault> {
    match d.u8(field)? {
        0 => Ok(Appearance::Dark),
        1 => Ok(Appearance::Light),
        _ => Err(fault(field, "an appearance is dark or light")),
    }
}

fn dec_bidi_mode(d: &mut R<'_>, field: &'static str) -> Result<BiDiMode, CanonFault> {
    Ok(match d.u8(field)? {
        0 => BiDiMode::Disabled,
        1 => BiDiMode::Implicit,
        2 => BiDiMode::Explicit,
        _ => return Err(fault(field, "a bidi mode this build does not name")),
    })
}

fn dec_direction(d: &mut R<'_>, field: &'static str) -> Result<ParagraphDirection, CanonFault> {
    Ok(match d.u8(field)? {
        0 => ParagraphDirection::Auto,
        1 => ParagraphDirection::AutoRtl,
        2 => ParagraphDirection::Ltr,
        3 => ParagraphDirection::Rtl,
        _ => return Err(fault(field, "a direction this build does not name")),
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive destructure of the modes, a line a field"
)]
fn enc_modes(w: &mut W, m: &TerminalModes) {
    let TerminalModes {
        cursor_visible,
        cursor_style,
        application_cursor_keys,
        alternate_screen,
        auto_wrap,
        origin_mode,
        insert_mode,
        new_line_mode,
        bracketed_paste,
        allow_osc52_query,
        allow_osc52_set,
        mouse_mode,
        mouse_encoding,
        focus_reporting,
        synchronized_output,
        reverse_video,
        cursor_blink,
        application_keypad,
        column_mode_132,
        reverse_wraparound,
        left_right_margin_mode,
        vt52_mode,
        grapheme_cluster_mode,
        vt_level,
        color_scheme,
        report_color_scheme,
        backarrow_sends_bs,
        special_modifiers,
        meta_send_escape,
        alt_send_escape,
        mode_1045,
        win32_input_mode,
        win32_input_enabled,
        win32_input_openconsole,
        in_band_size_reports,
        bidi_mode,
        bidi_direction,
        bidi_box_mirroring,
        bidi_autodetection,
        alternate_scroll,
        bidi_arrow_swap,
        sixel_display_mode,
        stream_attribute_extent,
        allow_window_ops,
        allow_notifications,
        allow_session_memory,
        allow_palette_reconfigure,
        require_shell_integration_nonce,
        kitty_keyboard_enabled,
        deccolm_enable,
        decncsm,
        ambiguous_width_double,
    } = *m;
    w.bool(cursor_visible);
    enc_cursor_style(w, cursor_style);
    for b in [
        application_cursor_keys,
        alternate_screen,
        auto_wrap,
        origin_mode,
        insert_mode,
        new_line_mode,
        bracketed_paste,
        allow_osc52_query,
        allow_osc52_set,
    ] {
        w.bool(b);
    }
    w.u8(mouse_mode as u8);
    w.u8(mouse_encoding as u8);
    for b in [
        focus_reporting,
        synchronized_output,
        reverse_video,
        cursor_blink,
        application_keypad,
        column_mode_132,
        reverse_wraparound,
        left_right_margin_mode,
        vt52_mode,
        grapheme_cluster_mode,
    ] {
        w.bool(b);
    }
    w.u8(vt_level as u8);
    enc_appearance(w, color_scheme);
    for b in [
        report_color_scheme,
        backarrow_sends_bs,
        special_modifiers,
        meta_send_escape,
        alt_send_escape,
        mode_1045,
        win32_input_mode,
        win32_input_enabled,
        win32_input_openconsole,
        in_band_size_reports,
    ] {
        w.bool(b);
    }
    w.u8(bidi_mode as u8);
    w.u8(bidi_direction as u8);
    for b in [
        bidi_box_mirroring,
        bidi_autodetection,
        alternate_scroll,
        bidi_arrow_swap,
        sixel_display_mode,
        stream_attribute_extent,
        allow_window_ops,
        allow_notifications,
        allow_session_memory,
        allow_palette_reconfigure,
        require_shell_integration_nonce,
        kitty_keyboard_enabled,
        deccolm_enable,
        decncsm,
        ambiguous_width_double,
    ] {
        w.bool(b);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the modes decoded in wire order, a line a field"
)]
fn dec_modes(d: &mut R<'_>) -> Result<TerminalModes, CanonFault> {
    let f = "modes";
    let cursor_visible = d.bool(f)?;
    let cursor_style = dec_cursor_style(d, "modes.cursor_style")?;
    let application_cursor_keys = d.bool(f)?;
    let alternate_screen = d.bool(f)?;
    let auto_wrap = d.bool(f)?;
    let origin_mode = d.bool(f)?;
    let insert_mode = d.bool(f)?;
    let new_line_mode = d.bool(f)?;
    let bracketed_paste = d.bool(f)?;
    let allow_osc52_query = d.bool(f)?;
    let allow_osc52_set = d.bool(f)?;
    let mouse_mode = dec_mouse_mode(d)?;
    let mouse_encoding = dec_mouse_encoding(d)?;
    let focus_reporting = d.bool(f)?;
    let synchronized_output = d.bool(f)?;
    let reverse_video = d.bool(f)?;
    let cursor_blink = d.bool(f)?;
    let application_keypad = d.bool(f)?;
    let column_mode_132 = d.bool(f)?;
    let reverse_wraparound = d.bool(f)?;
    let left_right_margin_mode = d.bool(f)?;
    let vt52_mode = d.bool(f)?;
    let grapheme_cluster_mode = d.bool(f)?;
    let vt_level = dec_vt_level(d)?;
    let color_scheme = dec_appearance(d, "modes.color_scheme")?;
    let report_color_scheme = d.bool(f)?;
    let backarrow_sends_bs = d.bool(f)?;
    let special_modifiers = d.bool(f)?;
    let meta_send_escape = d.bool(f)?;
    let alt_send_escape = d.bool(f)?;
    let mode_1045 = d.bool(f)?;
    let win32_input_mode = d.bool(f)?;
    let win32_input_enabled = d.bool(f)?;
    let win32_input_openconsole = d.bool(f)?;
    let in_band_size_reports = d.bool(f)?;
    let bidi_mode = dec_bidi_mode(d, "modes.bidi_mode")?;
    let bidi_direction = dec_direction(d, "modes.bidi_direction")?;
    let bidi_box_mirroring = d.bool(f)?;
    let bidi_autodetection = d.bool(f)?;
    let alternate_scroll = d.bool(f)?;
    let bidi_arrow_swap = d.bool(f)?;
    let sixel_display_mode = d.bool(f)?;
    let stream_attribute_extent = d.bool(f)?;
    let allow_window_ops = d.bool(f)?;
    let allow_notifications = d.bool(f)?;
    let allow_session_memory = d.bool(f)?;
    let allow_palette_reconfigure = d.bool(f)?;
    let require_shell_integration_nonce = d.bool(f)?;
    let kitty_keyboard_enabled = d.bool(f)?;
    let deccolm_enable = d.bool(f)?;
    let decncsm = d.bool(f)?;
    let ambiguous_width_double = d.bool(f)?;
    Ok(TerminalModes {
        cursor_visible,
        cursor_style,
        application_cursor_keys,
        alternate_screen,
        auto_wrap,
        origin_mode,
        insert_mode,
        new_line_mode,
        bracketed_paste,
        allow_osc52_query,
        allow_osc52_set,
        mouse_mode,
        mouse_encoding,
        focus_reporting,
        synchronized_output,
        reverse_video,
        cursor_blink,
        application_keypad,
        column_mode_132,
        reverse_wraparound,
        left_right_margin_mode,
        vt52_mode,
        grapheme_cluster_mode,
        vt_level,
        color_scheme,
        report_color_scheme,
        backarrow_sends_bs,
        special_modifiers,
        meta_send_escape,
        alt_send_escape,
        mode_1045,
        win32_input_mode,
        win32_input_enabled,
        win32_input_openconsole,
        in_band_size_reports,
        bidi_mode,
        bidi_direction,
        bidi_box_mirroring,
        bidi_autodetection,
        alternate_scroll,
        bidi_arrow_swap,
        sixel_display_mode,
        stream_attribute_extent,
        allow_window_ops,
        allow_notifications,
        allow_session_memory,
        allow_palette_reconfigure,
        require_shell_integration_nonce,
        kitty_keyboard_enabled,
        deccolm_enable,
        decncsm,
        ambiguous_width_double,
    })
}

fn dec_charset94(d: &mut R<'_>, field: &'static str) -> Result<CharacterSet, CanonFault> {
    const ALL: [CharacterSet; 14] = [
        CharacterSet::Ascii,
        CharacterSet::DecLineDrawing,
        CharacterSet::DecSupplemental,
        CharacterSet::UnitedKingdom,
        CharacterSet::Dutch,
        CharacterSet::Finnish,
        CharacterSet::French,
        CharacterSet::FrenchCanadian,
        CharacterSet::German,
        CharacterSet::Italian,
        CharacterSet::NorwegianDanish,
        CharacterSet::Spanish,
        CharacterSet::Swedish,
        CharacterSet::Swiss,
    ];
    let code = d.u8(field)?;
    ALL.into_iter()
        .find(|c| *c as u8 == code)
        .ok_or_else(|| fault(field, "a character set this build does not name"))
}

fn enc_charset96(w: &mut W, v: Option<CharacterSet96>) {
    w.opt(v, |w, c| match c {
        CharacterSet96::IsoLatin1Supplemental => w.u8(0),
        CharacterSet96::IsoLatin2Supplemental => w.u8(1),
        CharacterSet96::IsoLatin5Supplemental => w.u8(2),
        CharacterSet96::IsoLatinCyrillic => w.u8(3),
        CharacterSet96::IsoLatinGreek => w.u8(4),
        other => w.unnamed(other),
    });
}

fn dec_charset96(d: &mut R<'_>) -> Result<Option<CharacterSet96>, CanonFault> {
    let f = "charset.g96";
    d.opt(f, |d| {
        Ok(match d.u8(f)? {
            0 => CharacterSet96::IsoLatin1Supplemental,
            1 => CharacterSet96::IsoLatin2Supplemental,
            2 => CharacterSet96::IsoLatin5Supplemental,
            3 => CharacterSet96::IsoLatinCyrillic,
            4 => CharacterSet96::IsoLatinGreek,
            _ => return Err(fault(f, "a 96-set this build does not name")),
        })
    })
}

fn enc_charset(w: &mut W, c: &CharacterSetState) {
    let CharacterSetState {
        g0,
        g1,
        g2,
        g3,
        gl,
        single_shift,
        gr,
        ..
    } = *c;
    for g in [g0, g1, g2, g3] {
        w.u8(g as u8);
    }
    match gl {
        GlMapping::G0 => w.u8(0),
        GlMapping::G1 => w.u8(1),
        GlMapping::G2 => w.u8(2),
        GlMapping::G3 => w.u8(3),
        other => w.unnamed(other),
    }
    w.u8(match single_shift {
        SingleShift::None => 0,
        SingleShift::Ss2 => 1,
        SingleShift::Ss3 => 2,
    });
    match gr {
        GrMapping::G1 => w.u8(0),
        GrMapping::G2 => w.u8(1),
        GrMapping::G3 => w.u8(2),
        other => w.unnamed(other),
    }
    for g in 1..=3 {
        enc_charset96(w, c.designated_96(g));
    }
}

fn dec_charset(d: &mut R<'_>) -> Result<CharacterSetState, CanonFault> {
    let f = "charset";
    let g0 = dec_charset94(d, "charset.g0")?;
    let g1 = dec_charset94(d, "charset.g1")?;
    let g2 = dec_charset94(d, "charset.g2")?;
    let g3 = dec_charset94(d, "charset.g3")?;
    let gl = match d.u8(f)? {
        0 => GlMapping::G0,
        1 => GlMapping::G1,
        2 => GlMapping::G2,
        3 => GlMapping::G3,
        _ => return Err(fault("charset.gl", "a mapping this build does not name")),
    };
    let single_shift = match d.u8(f)? {
        0 => SingleShift::None,
        1 => SingleShift::Ss2,
        2 => SingleShift::Ss3,
        _ => return Err(fault("charset.single_shift", "a shift is none, SS2 or SS3")),
    };
    let gr = match d.u8(f)? {
        0 => GrMapping::G1,
        1 => GrMapping::G2,
        2 => GrMapping::G3,
        _ => return Err(fault("charset.gr", "a mapping this build does not name")),
    };
    let mut c = CharacterSetState::new();
    c.g0 = g0;
    c.g1 = g1;
    c.g2 = g2;
    c.g3 = g3;
    c.gl = gl;
    c.single_shift = single_shift;
    c.gr = gr;
    for g in 1..=3 {
        if let Some(set) = dec_charset96(d)? {
            c.designate_96(g, set);
        }
    }
    Ok(c)
}

fn enc_grid_cursor(w: &mut W, c: &GridCursorRepr) {
    let GridCursorRepr {
        cursor_row,
        cursor_col,
        pending_wrap,
        scroll_top,
        scroll_bottom,
        margin_left,
        margin_right,
        tab_stops,
        tab_defaults_suppressed,
        handback_rows,
        handback_above,
    } = c;
    w.u16(*cursor_row);
    w.u16(*cursor_col);
    w.bool(*pending_wrap);
    w.u16(*scroll_top);
    w.u16(*scroll_bottom);
    w.u16(*margin_left);
    w.u16(*margin_right);
    w.len(tab_stops.len());
    for stop in tab_stops {
        w.bool(*stop);
    }
    w.bool(*tab_defaults_suppressed);
    w.i32(*handback_rows);
    w.u16(*handback_above);
}

fn dec_grid_cursor(d: &mut R<'_>) -> Result<GridCursorRepr, CanonFault> {
    let f = "cursor";
    Ok(GridCursorRepr {
        cursor_row: d.u16(f)?,
        cursor_col: d.u16(f)?,
        pending_wrap: d.bool(f)?,
        scroll_top: d.u16(f)?,
        scroll_bottom: d.u16(f)?,
        margin_left: d.u16(f)?,
        margin_right: d.u16(f)?,
        tab_stops: d.vec("cursor.tab_stops", 1, |d| d.bool("cursor.tab_stops"))?,
        tab_defaults_suppressed: d.bool(f)?,
        handback_rows: d.i32(f)?,
        handback_above: d.u16(f)?,
    })
}

fn enc_saved_cursor(w: &mut W, s: Option<&SavedCursorRepr>) {
    w.opt(s, |w, s| {
        let SavedCursorRepr {
            cursor_row,
            cursor_col,
            style_fg_bits,
            style_bg_bits,
            style_flag_bits,
            style_protected,
            origin_mode,
            auto_wrap,
            charset,
            pending_wrap,
            underline_color,
            resized,
        } = s;
        w.u16(*cursor_row);
        w.u16(*cursor_col);
        w.u32(*style_fg_bits);
        w.u32(*style_bg_bits);
        w.u16(*style_flag_bits);
        w.bool(*style_protected);
        w.bool(*origin_mode);
        w.bool(*auto_wrap);
        enc_charset(w, charset);
        w.bool(pending_wrap.get());
        w.opt(*underline_color, W::u32);
        w.bool(resized.get());
    });
}

fn dec_saved_cursor(d: &mut R<'_>) -> Result<Option<SavedCursorRepr>, CanonFault> {
    let f = "saved_cursor";
    d.opt(f, |d| {
        Ok(SavedCursorRepr {
            cursor_row: d.u16(f)?,
            cursor_col: d.u16(f)?,
            style_fg_bits: d.u32(f)?,
            style_bg_bits: d.u32(f)?,
            style_flag_bits: d.u16(f)?,
            style_protected: d.bool(f)?,
            origin_mode: d.bool(f)?,
            auto_wrap: d.bool(f)?,
            charset: dec_charset(d)?,
            pending_wrap: SavedCursorPendingWrap::new(d.bool(f)?),
            underline_color: d.opt(f, |d| d.u32(f))?,
            resized: SavedCursorResized::new(d.bool(f)?),
        })
    })
}

fn enc_kitty(w: &mut W, k: &KittyKeyboardStateSnapshot) {
    let KittyKeyboardStateSnapshot {
        flags,
        main_saved_flags,
        alt_saved_flags,
        main_stack,
        alt_stack,
        main_sp,
        alt_sp,
    } = *k;
    w.u8(flags.bits());
    w.opt(main_saved_flags, |w, f| w.u8(f.bits()));
    w.opt(alt_saved_flags, |w, f| w.u8(f.bits()));
    for f in main_stack.iter().chain(alt_stack.iter()) {
        w.u8(f.bits());
    }
    w.u8(main_sp);
    w.u8(alt_sp);
}

fn dec_kitty(d: &mut R<'_>) -> Result<KittyKeyboardStateSnapshot, CanonFault> {
    let f = "kitty_keyboard";
    let flags = |d: &mut R<'_>| d.u8(f).map(KittyKeyboardFlags::from_bits);
    let current = flags(d)?;
    let main_saved_flags = d.opt(f, flags)?;
    let alt_saved_flags = d.opt(f, flags)?;
    let mut main_stack = [KittyKeyboardFlags::from_bits(0); 8];
    for slot in &mut main_stack {
        *slot = flags(d)?;
    }
    let mut alt_stack = main_stack;
    for slot in &mut alt_stack {
        *slot = flags(d)?;
    }
    Ok(KittyKeyboardStateSnapshot {
        flags: current,
        main_saved_flags,
        alt_saved_flags,
        main_stack,
        alt_stack,
        main_sp: d.u8(f)?,
        alt_sp: d.u8(f)?,
    })
}

fn enc_xterm_keyboard(w: &mut W, x: XtermKeyboardState) {
    w.opt(x.modify_other_keys(), W::u8);
    w.u8(x.format_other_keys());
}

fn dec_xterm_keyboard(d: &mut R<'_>) -> Result<XtermKeyboardState, CanonFault> {
    let f = "xterm_keyboard";
    let mut x = XtermKeyboardState::new();
    match d.opt(f, |d| d.u8(f))? {
        Some(v) => x.set_modify_other_keys(v),
        None => x.disable_modify_other_keys(),
    }
    x.set_format_other_keys(d.u8(f)?);
    Ok(x)
}

fn enc_taskbar(w: &mut W, t: Option<TaskbarProgress>) {
    w.opt(t, |w, t| match t {
        TaskbarProgress::Hidden => w.u8(0),
        TaskbarProgress::Normal(p) => w.raw(&[1, p]),
        TaskbarProgress::Error(p) => w.raw(&[2, p]),
        TaskbarProgress::Indeterminate => w.u8(3),
        TaskbarProgress::Paused(p) => w.raw(&[4, p]),
        other => w.unnamed(other),
    });
}

fn dec_taskbar(d: &mut R<'_>) -> Result<Option<TaskbarProgress>, CanonFault> {
    let f = "taskbar_progress";
    d.opt(f, |d| {
        Ok(match d.u8(f)? {
            0 => TaskbarProgress::Hidden,
            1 => TaskbarProgress::Normal(d.u8(f)?),
            2 => TaskbarProgress::Error(d.u8(f)?),
            3 => TaskbarProgress::Indeterminate,
            4 => TaskbarProgress::Paused(d.u8(f)?),
            _ => return Err(fault(f, "a progress this build does not name")),
        })
    })
}

fn enc_parser(w: &mut W, p: Option<&ParserCarry>) {
    w.opt(p, |w, p| {
        let ParserCarry {
            state,
            params,
            subparam_mask,
            current_param,
            param_started,
            last_was_colon,
            intermediates,
            osc,
            osc_overflowed,
            utf8_tail,
            utf8_expected,
            string_utf8_tail,
            tmux,
        } = p;
        w.str(state.name());
        w.len(params.len());
        for v in params {
            w.u16(*v);
        }
        w.u32(*subparam_mask);
        w.u32(*current_param);
        w.bool(*param_started);
        w.bool(*last_was_colon);
        w.bytes(intermediates);
        w.bytes(osc);
        w.bool(*osc_overflowed);
        w.bytes(utf8_tail);
        w.u8(*utf8_expected);
        w.bytes(string_utf8_tail);
        w.str(tmux.name());
    });
}

fn dec_parser(d: &mut R<'_>) -> Result<Option<ParserCarry>, CanonFault> {
    let f = "parser";
    d.opt(f, |d| {
        let state = State::from_name(&d.string("parser.state")?)
            .ok_or_else(|| fault("parser.state", "a state this build does not name"))?;
        Ok(ParserCarry {
            state,
            params: d.vec("parser.params", 2, |d| d.u16("parser.params"))?,
            subparam_mask: d.u32(f)?,
            current_param: d.u32(f)?,
            param_started: d.bool(f)?,
            last_was_colon: d.bool(f)?,
            intermediates: d.bytes("parser.intermediates")?.to_vec(),
            osc: d.bytes("parser.osc")?.to_vec(),
            osc_overflowed: d.bool(f)?,
            utf8_tail: d.bytes("parser.utf8_tail")?.to_vec(),
            utf8_expected: d.u8(f)?,
            string_utf8_tail: d.bytes("parser.string_utf8_tail")?.to_vec(),
            tmux: TmuxPassthrough::from_name(&d.string("parser.tmux")?)
                .ok_or_else(|| fault("parser.tmux", "a wrapper this build does not name"))?,
        })
    })
}

fn enc_slot(w: &mut W, s: Option<&ColorSlotRepr>) {
    w.opt(s, |w, s| {
        let ColorSlotRepr { rgb } = s;
        w.opt_rgb(*rgb);
    });
}

fn dec_slot(d: &mut R<'_>, field: &'static str) -> Result<Option<ColorSlotRepr>, CanonFault> {
    d.opt(field, |d| {
        Ok(ColorSlotRepr {
            rgb: d.opt_rgb(field)?,
        })
    })
}

fn enc_palette_overrides(w: &mut W, p: &[PaletteOverrideRepr]) {
    w.len(p.len());
    for PaletteOverrideRepr { index, rgb } in p {
        w.u8(*index);
        w.rgb(*rgb);
    }
}

fn dec_palette_overrides(d: &mut R<'_>) -> Result<Vec<PaletteOverrideRepr>, CanonFault> {
    let f = "color.palette";
    d.vec(f, 4, |d| {
        Ok(PaletteOverrideRepr {
            index: d.u8(f)?,
            rgb: d.rgb(f)?,
        })
    })
}

fn enc_color(w: &mut W, c: &ColorRepr) {
    let ColorRepr {
        palette,
        foreground,
        background,
        cursor,
        selection_background,
        selection_foreground,
        stack,
        stack_overrides,
    } = c;
    enc_palette_overrides(w, palette);
    w.opt_rgb(*foreground);
    w.opt_rgb(*background);
    enc_slot(w, cursor.as_ref());
    enc_slot(w, selection_background.as_ref());
    enc_slot(w, selection_foreground.as_ref());
    w.len(stack.len());
    for entry in stack {
        let ColorStackEntryRepr {
            palette,
            foreground,
            background,
            cursor,
            selection_background,
            selection_foreground,
        } = entry;
        w.len(palette.len());
        for rgb in palette {
            w.rgb(*rgb);
        }
        w.rgb(*foreground);
        w.rgb(*background);
        w.opt_rgb(*cursor);
        w.opt_rgb(*selection_background);
        w.opt_rgb(*selection_foreground);
    }
    w.len(stack_overrides.len());
    for o in stack_overrides {
        let ColorOverridesRepr {
            palette,
            foreground,
            background,
            cursor,
            selection_background,
            selection_foreground,
        } = o;
        enc_palette_overrides(w, palette);
        w.opt_rgb(*foreground);
        w.opt_rgb(*background);
        enc_slot(w, cursor.as_ref());
        enc_slot(w, selection_background.as_ref());
        enc_slot(w, selection_foreground.as_ref());
    }
}

fn dec_color(d: &mut R<'_>) -> Result<ColorRepr, CanonFault> {
    let f = "color";
    Ok(ColorRepr {
        palette: dec_palette_overrides(d)?,
        foreground: d.opt_rgb(f)?,
        background: d.opt_rgb(f)?,
        cursor: dec_slot(d, "color.cursor")?,
        selection_background: dec_slot(d, "color.selection_background")?,
        selection_foreground: dec_slot(d, "color.selection_foreground")?,
        stack: d.vec("color.stack", 8, |d| {
            Ok(ColorStackEntryRepr {
                palette: d.vec("color.stack.palette", 3, |d| d.rgb("color.stack.palette"))?,
                foreground: d.rgb(f)?,
                background: d.rgb(f)?,
                cursor: d.opt_rgb(f)?,
                selection_background: d.opt_rgb(f)?,
                selection_foreground: d.opt_rgb(f)?,
            })
        })?,
        stack_overrides: d.vec("color.stack_overrides", 8, |d| {
            Ok(ColorOverridesRepr {
                palette: dec_palette_overrides(d)?,
                foreground: d.opt_rgb(f)?,
                background: d.opt_rgb(f)?,
                cursor: dec_slot(d, "color.stack_overrides.cursor")?,
                selection_background: dec_slot(d, "color.stack_overrides.selection_background")?,
                selection_foreground: dec_slot(d, "color.stack_overrides.selection_foreground")?,
            })
        })?,
    })
}

fn enc_span(w: &mut W, s: &ShellSpanRepr) {
    let ShellSpanRepr {
        prompt_start_row,
        prompt_start_col,
        command_start_row,
        command_start_col,
        output_start_row,
        end_row,
        exit_code,
        working_directory,
        commandline,
        prompt_time_ms,
        command_input_start_time_ms,
        command_exec_start_time_ms,
        command_end_time_ms,
    } = s;
    w.u64(*prompt_start_row);
    w.u16(*prompt_start_col);
    w.opt_u64(*command_start_row);
    w.opt(*command_start_col, W::u16);
    w.opt_u64(*output_start_row);
    w.opt_u64(*end_row);
    w.opt(*exit_code, W::i32);
    w.opt_str(working_directory.as_deref());
    w.opt_str(commandline.as_deref());
    w.opt_u64(*prompt_time_ms);
    w.opt_u64(*command_input_start_time_ms);
    w.opt_u64(*command_exec_start_time_ms);
    w.opt_u64(*command_end_time_ms);
}

fn dec_span(d: &mut R<'_>) -> Result<ShellSpanRepr, CanonFault> {
    let f = "shell.span";
    Ok(ShellSpanRepr {
        prompt_start_row: d.u64(f)?,
        prompt_start_col: d.u16(f)?,
        command_start_row: d.opt_u64(f)?,
        command_start_col: d.opt(f, |d| d.u16(f))?,
        output_start_row: d.opt_u64(f)?,
        end_row: d.opt_u64(f)?,
        exit_code: d.opt(f, |d| d.i32(f))?,
        working_directory: d.opt_string(f)?,
        commandline: d.opt_string(f)?,
        prompt_time_ms: d.opt_u64(f)?,
        command_input_start_time_ms: d.opt_u64(f)?,
        command_exec_start_time_ms: d.opt_u64(f)?,
        command_end_time_ms: d.opt_u64(f)?,
    })
}

fn enc_block(w: &mut W, b: &OutputBlockRepr) {
    let OutputBlockRepr {
        id,
        state,
        collapsed,
        text_cleared,
        command_text_cleared,
        span,
    } = b;
    w.u64(*id);
    w.u8(*state);
    w.bool(*collapsed);
    w.bool(*text_cleared);
    w.bool(*command_text_cleared);
    enc_span(w, span);
}

fn dec_block(d: &mut R<'_>) -> Result<OutputBlockRepr, CanonFault> {
    let f = "shell.block";
    Ok(OutputBlockRepr {
        id: d.u64(f)?,
        state: d.u8(f)?,
        collapsed: d.bool(f)?,
        text_cleared: d.bool(f)?,
        command_text_cleared: d.bool(f)?,
        span: dec_span(d)?,
    })
}

fn enc_cwd(w: &mut W, c: Option<&CwdReportRepr>) {
    w.opt(c, |w, c| {
        let CwdReportRepr { path, host } = c;
        w.opt_str(path.as_deref());
        w.opt_str(host.as_deref());
    });
}

fn dec_cwd(d: &mut R<'_>, field: &'static str) -> Result<Option<CwdReportRepr>, CanonFault> {
    d.opt(field, |d| {
        Ok(CwdReportRepr {
            path: d.opt_string(field)?,
            host: d.opt_string(field)?,
        })
    })
}

fn enc_shell(w: &mut W, s: &ShellRepr) {
    let ShellRepr {
        phase,
        state,
        current_mark,
        command_marks,
        current_block,
        output_blocks,
        next_block_id,
        completed_seq,
        started_seq,
        signs_cwd,
        unmarked_run,
        phase_stale,
        held_cwd,
        cwd_before_job,
    } = s;
    w.u8(*phase);
    w.u8(*state);
    w.opt(current_mark.as_ref(), enc_span);
    w.len(command_marks.len());
    for m in command_marks {
        enc_span(w, m);
    }
    w.opt(current_block.as_ref(), enc_block);
    w.len(output_blocks.len());
    for b in output_blocks {
        enc_block(w, b);
    }
    w.u64(*next_block_id);
    w.u64(*completed_seq);
    w.u64(*started_seq);
    w.bool(*signs_cwd);
    w.bool(*unmarked_run);
    w.bool(*phase_stale);
    enc_cwd(w, held_cwd.as_ref());
    enc_cwd(w, cwd_before_job.as_ref());
}

fn dec_shell(d: &mut R<'_>) -> Result<ShellRepr, CanonFault> {
    let f = "shell";
    Ok(ShellRepr {
        phase: d.u8(f)?,
        state: d.u8(f)?,
        current_mark: d.opt(f, dec_span)?,
        command_marks: d.vec("shell.command_marks", 8, dec_span)?,
        current_block: d.opt(f, dec_block)?,
        output_blocks: d.vec("shell.output_blocks", 8, dec_block)?,
        next_block_id: d.u64(f)?,
        completed_seq: d.u64(f)?,
        started_seq: d.u64(f)?,
        signs_cwd: d.bool(f)?,
        unmarked_run: d.bool(f)?,
        phase_stale: d.bool(f)?,
        held_cwd: dec_cwd(d, "shell.held_cwd")?,
        cwd_before_job: dec_cwd(d, "shell.cwd_before_job")?,
    })
}

fn enc_title(w: &mut W, t: Option<&TitleRepr>) {
    w.opt(t, |w, t| {
        let TitleRepr {
            window,
            icon,
            stack,
            stack_saved,
        } = t;
        w.str(window);
        w.str(icon);
        w.len(stack.len());
        for (icon, window) in stack {
            w.str(icon);
            w.str(window);
        }
        w.len(stack_saved.len());
        for (icon, window) in stack_saved {
            w.bool(*icon);
            w.bool(*window);
        }
    });
}

fn dec_title(d: &mut R<'_>) -> Result<Option<TitleRepr>, CanonFault> {
    let f = "title";
    d.opt(f, |d| {
        Ok(TitleRepr {
            window: d.string(f)?,
            icon: d.string(f)?,
            stack: d.vec("title.stack", 16, |d| Ok((d.string(f)?, d.string(f)?)))?,
            stack_saved: d.vec("title.stack_saved", 2, |d| Ok((d.bool(f)?, d.bool(f)?)))?,
        })
    })
}

fn enc_output(w: &mut W, o: &OutputStateRepr) {
    let OutputStateRepr {
        rep,
        underline_color,
        hyperlink,
        vt52_address,
        zwj_pending,
        sgr_stack,
        saved_modes,
        program_state,
        program_state_began,
    } = o;
    w.opt(*rep, |w, c| w.u32(u32::from(c)));
    w.opt(*underline_color, W::u32);
    w.opt(hyperlink.as_ref(), |w, h| {
        let HyperlinkRepr { uri, id } = h;
        w.str(uri);
        w.opt_str(id.as_deref());
    });
    w.opt(vt52_address.as_ref(), |w, a| {
        let Vt52AddressRepr { row } = a;
        w.opt(*row, W::u8);
    });
    w.bool(*zwj_pending);
    w.len(sgr_stack.len());
    for push in sgr_stack {
        let SgrPushRepr {
            fg,
            bg,
            flags,
            protected,
            underline_color,
            mask,
        } = push;
        w.u32(*fg);
        w.u32(*bg);
        w.u16(*flags);
        w.bool(*protected);
        w.opt(*underline_color, W::u32);
        w.u16(*mask);
    }
    w.len(saved_modes.len());
    for SavedModeRepr { mode, set } in saved_modes {
        w.u16(*mode);
        w.bool(*set);
    }
    w.opt_str(program_state.as_deref());
    w.opt_u64(*program_state_began);
}

fn dec_output(d: &mut R<'_>) -> Result<OutputStateRepr, CanonFault> {
    let f = "output_state";
    Ok(OutputStateRepr {
        rep: d.opt(f, |d| {
            char::from_u32(d.u32(f)?).ok_or_else(|| fault("output_state.rep", "not a char"))
        })?,
        underline_color: d.opt(f, |d| d.u32(f))?,
        hyperlink: d.opt(f, |d| {
            Ok(HyperlinkRepr {
                uri: d.string(f)?,
                id: d.opt_string(f)?,
            })
        })?,
        vt52_address: d.opt(f, |d| {
            Ok(Vt52AddressRepr {
                row: d.opt(f, |d| d.u8(f))?,
            })
        })?,
        zwj_pending: d.bool(f)?,
        sgr_stack: d.vec("output_state.sgr_stack", 14, |d| {
            Ok(SgrPushRepr {
                fg: d.u32(f)?,
                bg: d.u32(f)?,
                flags: d.u16(f)?,
                protected: d.bool(f)?,
                underline_color: d.opt(f, |d| d.u32(f))?,
                mask: d.u16(f)?,
            })
        })?,
        saved_modes: d.vec("output_state.saved_modes", 3, |d| {
            Ok(SavedModeRepr {
                mode: d.u16(f)?,
                set: d.bool(f)?,
            })
        })?,
        program_state: d.opt_string(f)?,
        program_state_began: d.opt_u64(f)?,
    })
}

fn enc_holder(w: &mut W, h: &HolderRepr) {
    let HolderRepr {
        current,
        clock,
        marks,
        anchors,
        anchors_lost,
        seal_epoch,
        seed_untrusted,
    } = h;
    w.opt(*current, W::u8);
    w.u64(*clock);
    w.len(marks.len());
    for m in marks {
        let HolderMarkRepr {
            token,
            spans,
            tainted,
            alt_fresh,
            seal,
            suspended_at,
        } = m;
        w.u64(*token);
        w.len(spans.len());
        for HolderSpanRepr { start, end } in spans {
            w.u16(*start);
            w.opt(*end, W::u16);
        }
        w.bool(*tainted);
        w.bool(*alt_fresh);
        w.opt(seal.as_ref(), |w, s| {
            let HolderSealRepr { rows, base, epoch } = s;
            w.len(rows.len());
            for (row, print) in rows {
                w.u64(*row);
                w.u64(*print);
            }
            w.u64(*base);
            w.u64(*epoch);
        });
        w.u64(*suspended_at);
    }
    w.len(anchors.len());
    for HolderAnchorRepr { row, end, live } in anchors {
        w.u64(*row);
        w.bool(*end);
        w.bool(*live);
    }
    w.bool(*anchors_lost);
    w.u64(*seal_epoch);
    w.bool(*seed_untrusted);
}

fn dec_holder(d: &mut R<'_>) -> Result<HolderRepr, CanonFault> {
    let f = "holder";
    Ok(HolderRepr {
        current: d.opt(f, |d| d.u8(f))?,
        clock: d.u64(f)?,
        marks: d.vec("holder.marks", 28, |d| {
            Ok(HolderMarkRepr {
                token: d.u64(f)?,
                spans: d.vec("holder.spans", 3, |d| {
                    Ok(HolderSpanRepr {
                        start: d.u16(f)?,
                        end: d.opt(f, |d| d.u16(f))?,
                    })
                })?,
                tainted: d.bool(f)?,
                alt_fresh: d.bool(f)?,
                seal: d.opt(f, |d| {
                    Ok(HolderSealRepr {
                        rows: d.vec("holder.seal.rows", 16, |d| Ok((d.u64(f)?, d.u64(f)?)))?,
                        base: d.u64(f)?,
                        epoch: d.u64(f)?,
                    })
                })?,
                suspended_at: d.u64(f)?,
            })
        })?,
        anchors: d.vec("holder.anchors", 10, |d| {
            Ok(HolderAnchorRepr {
                row: d.u64(f)?,
                end: d.bool(f)?,
                live: d.bool(f)?,
            })
        })?,
        anchors_lost: d.bool(f)?,
        seal_epoch: d.u64(f)?,
        seed_untrusted: d.bool(f)?,
    })
}

/// The strict decode of a grid body of `cols` columns, under the bounds the crash-restore records use: its record count.
fn grid_records(blob: &[u8], cols: u16) -> Option<usize> {
    let cells = usize::from(cols);
    let content_cap = cells.saturating_mul(256);
    let record_cap = 16usize
        .saturating_mul(1024)
        .saturating_add(cells.saturating_mul(512));
    deserialize_lines_strict(
        blob,
        blob.len(),
        cells,
        content_cap,
        max_linked_record_bytes(record_cap, cells),
    )
    .map(|lines| lines.len())
}

impl TerminalCheckpoint {
    /// This checkpoint's canonical bytes: two checkpoints encode alike exactly when they are equal, every field in declaration order.
    #[must_use]
    pub fn canonical(&self) -> Vec<u8> {
        let TerminalCheckpoint {
            rows,
            cols,
            grid,
            history_lines,
            cursor,
            alt_grid,
            alt_cursor,
            saved_cursor_main,
            saved_cursor_alt,
            modes,
            style,
            charset,
            kitty_keyboard,
            xterm_keyboard,
            taskbar_progress,
            secure_keyboard_entry,
            current_working_directory,
            parser_ground,
            parser,
            shell_integration_nonce,
            absolute_row_counter,
            alt_absolute_row_counter,
            color,
            shell,
            shell_integration_rev,
            title,
            output_state,
            holder,
        } = self;
        let mut w = W(Vec::with_capacity(grid.len() + 1024));
        w.raw(CHECKPOINT_MAGIC);
        w.u32(CHECKPOINT_CANON_VERSION);
        w.u16(*rows);
        w.u16(*cols);
        w.bytes(grid);
        w.u32(*history_lines);
        enc_grid_cursor(&mut w, cursor);
        w.opt(alt_grid.as_deref(), W::bytes);
        w.opt(alt_cursor.as_ref(), enc_grid_cursor);
        enc_saved_cursor(&mut w, saved_cursor_main.as_ref());
        enc_saved_cursor(&mut w, saved_cursor_alt.as_ref());
        enc_modes(&mut w, modes);
        let StyleRepr {
            fg,
            bg,
            flags,
            protected,
        } = *style;
        w.u32(fg.0);
        w.u32(bg.0);
        w.u16(flags.0);
        w.bool(protected);
        enc_charset(&mut w, charset);
        enc_kitty(&mut w, kitty_keyboard);
        enc_xterm_keyboard(&mut w, *xterm_keyboard);
        enc_taskbar(&mut w, *taskbar_progress);
        w.bool(*secure_keyboard_entry);
        w.opt_str(current_working_directory.as_deref());
        w.bool(*parser_ground);
        enc_parser(&mut w, parser.as_ref());
        w.opt(shell_integration_nonce.as_ref(), |w, n| w.raw(&n.0));
        w.u64(*absolute_row_counter);
        w.u64(*alt_absolute_row_counter);
        enc_color(&mut w, color);
        enc_shell(&mut w, shell);
        w.opt_str(shell_integration_rev.as_deref());
        enc_title(&mut w, title.as_ref());
        enc_output(&mut w, output_state);
        enc_holder(&mut w, holder);
        w.0
    }

    /// The checkpoint `bytes` canonically encode ([`Self::canonical`]), its geometry and grid bodies checked as strictly as a crash-restore record's.
    ///
    /// # Errors
    /// The first field that is not canonical, or that names a geometry or grid no restore takes.
    #[allow(
        clippy::too_many_lines,
        reason = "every checkpoint field decoded in wire order, then checked"
    )]
    pub fn from_canonical(bytes: &[u8]) -> Result<Self, CanonFault> {
        let mut d = R { b: bytes };
        d.magic("magic", CHECKPOINT_MAGIC, CHECKPOINT_CANON_VERSION)?;
        let rows = d.u16("rows")?;
        let cols = d.u16("cols")?;
        if rows == 0 || cols == 0 || rows > MAX_GRID_ROWS || cols > MAX_GRID_COLS {
            return Err(fault("rows", "a geometry no grid takes"));
        }
        let grid = d.bytes("grid")?.to_vec();
        let history_lines = d.u32("history_lines")?;
        let main =
            grid_records(&grid, cols).ok_or_else(|| fault("grid", "fails the strict decode"))?;
        let expected = usize::try_from(history_lines)
            .ok()
            .and_then(|h| h.checked_add(usize::from(rows)));
        if expected != Some(main) {
            return Err(fault(
                "history_lines",
                "the grid holds another count of lines",
            ));
        }
        let cursor = dec_grid_cursor(&mut d)?;
        let alt_grid = d.opt("alt_grid", |d| d.bytes("alt_grid").map(<[u8]>::to_vec))?;
        let alt_cursor = d.opt("alt_cursor", dec_grid_cursor)?;
        match (&alt_grid, &alt_cursor) {
            (None, None) => {}
            (Some(alt), Some(_)) => {
                let records = grid_records(alt, cols)
                    .ok_or_else(|| fault("alt_grid", "fails the strict decode"))?;
                if records < usize::from(rows) {
                    return Err(fault("alt_grid", "fewer lines than the screen"));
                }
            }
            _ => {
                return Err(fault(
                    "alt_cursor",
                    "an inactive grid comes with its cursor",
                ));
            }
        }
        let saved_cursor_main = dec_saved_cursor(&mut d)?;
        let saved_cursor_alt = dec_saved_cursor(&mut d)?;
        let modes = dec_modes(&mut d)?;
        let style = StyleRepr {
            fg: PackedColor(d.u32("style")?),
            bg: PackedColor(d.u32("style")?),
            flags: CellFlags(d.u16("style")?),
            protected: d.bool("style")?,
        };
        let charset = dec_charset(&mut d)?;
        let kitty_keyboard = dec_kitty(&mut d)?;
        let xterm_keyboard = dec_xterm_keyboard(&mut d)?;
        let taskbar_progress = dec_taskbar(&mut d)?;
        let secure_keyboard_entry = d.bool("secure_keyboard_entry")?;
        let current_working_directory = d.opt_string("current_working_directory")?;
        let parser_ground = d.bool("parser_ground")?;
        let parser = dec_parser(&mut d)?;
        let shell_integration_nonce = d.opt("shell_integration_nonce", |d| {
            d.arr::<32>("shell_integration_nonce")
                .map(ShellIntegrationNonce)
        })?;
        let absolute_row_counter = d.u64("absolute_row_counter")?;
        let alt_absolute_row_counter = d.u64("alt_absolute_row_counter")?;
        let color = dec_color(&mut d)?;
        let shell = dec_shell(&mut d)?;
        let shell_integration_rev = d.opt_string("shell_integration_rev")?;
        let title = dec_title(&mut d)?;
        let output_state = dec_output(&mut d)?;
        let holder = dec_holder(&mut d)?;
        d.end("holder")?;
        let checkpoint = TerminalCheckpoint {
            rows,
            cols,
            grid,
            history_lines,
            cursor,
            alt_grid,
            alt_cursor,
            saved_cursor_main,
            saved_cursor_alt,
            modes,
            style,
            charset,
            kitty_keyboard,
            xterm_keyboard,
            taskbar_progress,
            secure_keyboard_entry,
            current_working_directory,
            parser_ground,
            parser,
            shell_integration_nonce,
            absolute_row_counter,
            alt_absolute_row_counter,
            color,
            shell,
            shell_integration_rev,
            title,
            output_state,
            holder,
        };
        if checkpoint.canonical() != bytes {
            return Err(fault("canonical", "the value re-encodes to other bytes"));
        }
        Ok(checkpoint)
    }
}

impl TerminalConfig {
    /// This config's canonical bytes, the palette as its 256 resolved colours and each `f32` as its bits.
    #[must_use]
    pub fn canonical(&self) -> Vec<u8> {
        let TerminalConfig {
            cursor_style,
            cursor_blink,
            cursor_color,
            cursor_visible,
            default_foreground,
            default_background,
            selection_background,
            selection_foreground,
            custom_palette,
            color_scheme,
            scrollback_limit,
            auto_wrap,
            focus_reporting,
            bracketed_paste,
            allow_osc52_query,
            allow_window_ops,
            allow_notifications,
            allow_palette_reconfigure,
            memory_budget,
            sync_timeout_ms,
            ambiguous_width_double,
            bold_is_bright,
            faint_opacity,
            indexed_minimum_contrast,
            bidi,
        } = self;
        let BiDiConfig {
            mode,
            direction,
            reorder_nsm,
            alternate_screen,
        } = bidi;
        let mut w = W(Vec::with_capacity(1024));
        w.raw(CONFIG_MAGIC);
        w.u32(CONFIG_CANON_VERSION);
        enc_cursor_style(&mut w, *cursor_style);
        w.bool(*cursor_blink);
        w.opt_rgb(*cursor_color);
        w.bool(*cursor_visible);
        w.rgb(*default_foreground);
        w.rgb(*default_background);
        w.opt_rgb(*selection_background);
        w.opt_rgb(*selection_foreground);
        w.opt(custom_palette.as_ref(), |w, p| {
            for i in 0..=u8::MAX {
                w.rgb(p.get(i));
            }
        });
        w.opt(*color_scheme, enc_appearance);
        w.opt(*scrollback_limit, W::len);
        for b in [
            *auto_wrap,
            *focus_reporting,
            *bracketed_paste,
            *allow_osc52_query,
            *allow_window_ops,
            *allow_notifications,
            *allow_palette_reconfigure,
        ] {
            w.bool(b);
        }
        w.len(*memory_budget);
        w.u64(*sync_timeout_ms);
        w.bool(*ambiguous_width_double);
        w.bool(*bold_is_bright);
        w.u32(faint_opacity.to_bits());
        w.u32(indexed_minimum_contrast.to_bits());
        w.u8(*mode as u8);
        w.u8(*direction as u8);
        w.bool(*reorder_nsm);
        w.bool(*alternate_screen);
        w.0
    }

    /// The config `bytes` canonically encode ([`Self::canonical`]).
    ///
    /// # Errors
    /// The first field that is not canonical.
    pub fn from_canonical(bytes: &[u8]) -> Result<Self, CanonFault> {
        let mut d = R { b: bytes };
        d.magic("config.magic", CONFIG_MAGIC, CONFIG_CANON_VERSION)?;
        let f = "config";
        let usize_of = |v: u64, field| usize::try_from(v).map_err(|_| fault(field, "past usize"));
        let cursor_style = dec_cursor_style(&mut d, "config.cursor_style")?;
        let cursor_blink = d.bool(f)?;
        let cursor_color = d.opt_rgb(f)?;
        let cursor_visible = d.bool(f)?;
        let default_foreground = d.rgb(f)?;
        let default_background = d.rgb(f)?;
        let selection_background = d.opt_rgb(f)?;
        let selection_foreground = d.opt_rgb(f)?;
        let custom_palette = d.opt("config.custom_palette", |d| {
            let mut p = ColorPalette::new();
            for i in 0..=u8::MAX {
                p.set(i, d.rgb("config.custom_palette")?);
            }
            Ok(p)
        })?;
        let color_scheme = d.opt(f, |d| dec_appearance(d, "config.color_scheme"))?;
        let scrollback_limit = d.opt(f, |d| usize_of(d.u64(f)?, "config.scrollback_limit"))?;
        let auto_wrap = d.bool(f)?;
        let focus_reporting = d.bool(f)?;
        let bracketed_paste = d.bool(f)?;
        let allow_osc52_query = d.bool(f)?;
        let allow_window_ops = d.bool(f)?;
        let allow_notifications = d.bool(f)?;
        let allow_palette_reconfigure = d.bool(f)?;
        let memory_budget = usize_of(d.u64(f)?, "config.memory_budget")?;
        let sync_timeout_ms = d.u64(f)?;
        let ambiguous_width_double = d.bool(f)?;
        let bold_is_bright = d.bool(f)?;
        let faint_opacity = f32::from_bits(d.u32(f)?);
        let indexed_minimum_contrast = f32::from_bits(d.u32(f)?);
        let bidi = BiDiConfig {
            mode: dec_bidi_mode(&mut d, "config.bidi.mode")?,
            direction: dec_direction(&mut d, "config.bidi.direction")?,
            reorder_nsm: d.bool(f)?,
            alternate_screen: d.bool(f)?,
        };
        d.end("config")?;
        let config = TerminalConfig {
            cursor_style,
            cursor_blink,
            cursor_color,
            cursor_visible,
            default_foreground,
            default_background,
            selection_background,
            selection_foreground,
            custom_palette,
            color_scheme,
            scrollback_limit,
            auto_wrap,
            focus_reporting,
            bracketed_paste,
            allow_osc52_query,
            allow_window_ops,
            allow_notifications,
            allow_palette_reconfigure,
            memory_budget,
            sync_timeout_ms,
            ambiguous_width_double,
            bold_is_bright,
            faint_opacity,
            indexed_minimum_contrast,
            bidi,
        };
        if config.canonical() != bytes {
            return Err(fault(
                "config.canonical",
                "the value re-encodes to other bytes",
            ));
        }
        Ok(config)
    }
}

fn enc_handback(w: &mut W, carry: aterm_grid::GridHandbackCarry) {
    let aterm_grid::CanonicalHandbackCarry {
        edge_kind,
        rows,
        above,
        pushed,
        seam_gap,
        seam_top,
        seam_bottom,
        skew,
        resize_law,
    } = carry.to_canonical();
    w.u8(edge_kind);
    w.u16(rows);
    w.u16(above);
    w.u16(pushed);
    w.u16(seam_gap);
    w.bool(seam_top);
    w.bool(seam_bottom);
    w.u16(skew);
    w.u8(resize_law);
}

fn dec_handback(
    d: &mut R<'_>,
    field: &'static str,
) -> Result<aterm_grid::GridHandbackCarry, CanonFault> {
    let raw = aterm_grid::CanonicalHandbackCarry {
        edge_kind: d.u8(field)?,
        rows: d.u16(field)?,
        above: d.u16(field)?,
        pushed: d.u16(field)?,
        seam_gap: d.u16(field)?,
        seam_top: d.bool(field)?,
        seam_bottom: d.bool(field)?,
        skew: d.u16(field)?,
        resize_law: d.u8(field)?,
    };
    aterm_grid::GridHandbackCarry::from_canonical(raw)
        .ok_or_else(|| fault(field, "non-canonical grid handback carry"))
}

fn validate_nonneg_f64_bits(bits: u64, field: &'static str) -> Result<(), CanonFault> {
    let v = f64::from_bits(bits);
    if !v.is_finite() || v < 0.0 || (v == 0.0 && bits != 0) {
        return Err(fault(
            field,
            "tokens must be finite, non-negative, and +0.0",
        ));
    }
    Ok(())
}

type DecodedHostShellCarry = (
    bool,
    bool,
    Option<ShellIntegrationNonce>,
    Option<(Option<ShellIntegrationNonce>, bool)>,
);

fn dec_host_shell_carry(d: &mut R<'_>) -> Result<DecodedHostShellCarry, CanonFault> {
    let foreground_job = d.bool("pane_carry.foreground_job")?;
    let probe_job_active = d.bool("pane_carry.probe_job_active")?;
    if foreground_job && probe_job_active {
        return Err(fault(
            "pane_carry.probe_job_active",
            "probe_job_active requires foreground_job == false",
        ));
    }
    let awaiting_shell_nonce = d.opt("pane_carry.awaiting_shell_nonce", |d| {
        Ok(ShellIntegrationNonce(
            d.arr("pane_carry.awaiting_shell_nonce")?,
        ))
    })?;
    let typed_rekey_prior = d.opt("pane_carry.typed_rekey_prior", |d| {
        let pn = d.opt("pane_carry.typed_rekey_prior.nonce", |d| {
            Ok(ShellIntegrationNonce(
                d.arr("pane_carry.typed_rekey_prior.nonce")?,
            ))
        })?;
        let pa = d.bool("pane_carry.typed_rekey_prior.awaiting")?;
        if pn.is_none() && pa {
            return Err(fault(
                "pane_carry.typed_rekey_prior",
                "awaiting prior requires a prior nonce",
            ));
        }
        Ok((pn, pa))
    })?;
    if awaiting_shell_nonce.is_none() && typed_rekey_prior.is_some() {
        return Err(fault(
            "pane_carry.typed_rekey_prior",
            "typed_rekey_prior requires awaiting_shell_nonce",
        ));
    }
    Ok((
        foreground_job,
        probe_job_active,
        awaiting_shell_nonce,
        typed_rekey_prior,
    ))
}

fn enc_dcs_query(w: &mut W, q: &DcsQueryCarry) {
    let DcsQueryCarry {
        kind,
        data,
        sequence_bytes,
        parser:
            HookedDcsCarry {
                st_pending,
                string_utf8_tail,
                tmux,
            },
    } = q;
    w.u8(match kind {
        DcsQueryKind::Decrqss => 0,
        DcsQueryKind::Xtgettcap => 1,
    });
    w.bytes(data);
    w.u32(*sequence_bytes);
    w.bool(*st_pending);
    w.bytes(string_utf8_tail);
    w.str(tmux.name());
}

fn dec_dcs_query(d: &mut R<'_>) -> Result<DcsQueryCarry, CanonFault> {
    let f = "pane_carry.dcs_query";
    let kind = match d.u8(f)? {
        0 => DcsQueryKind::Decrqss,
        1 => DcsQueryKind::Xtgettcap,
        _ => return Err(fault(f, "dcs_query kind is 0 (Decrqss) or 1 (Xtgettcap)")),
    };
    let data = d.bytes("pane_carry.dcs_query.data")?.to_vec();
    let sequence_bytes = d.u32("pane_carry.dcs_query.sequence_bytes")?;
    let seq = usize::try_from(sequence_bytes).unwrap_or(usize::MAX);
    if seq > super::MAX_DCS_GLOBAL_BUDGET || data.len() != seq.min(kind.data_cap()) {
        return Err(fault(
            "pane_carry.dcs_query.sequence_bytes",
            "sequence_bytes must stay within MAX_DCS_GLOBAL_BUDGET and match capped data length",
        ));
    }
    let parser = HookedDcsCarry {
        st_pending: d.bool("pane_carry.dcs_query.st_pending")?,
        string_utf8_tail: d.bytes("pane_carry.dcs_query.string_utf8_tail")?.to_vec(),
        tmux: TmuxPassthrough::from_name(&d.string("pane_carry.dcs_query.tmux")?).ok_or_else(
            || {
                fault(
                    "pane_carry.dcs_query.tmux",
                    "a wrapper this build does not name",
                )
            },
        )?,
    };
    if aterm_parser::Parser::new()
        .restore_hooked_dcs(&parser)
        .is_err()
    {
        return Err(fault(
            "pane_carry.dcs_query.parser",
            "non-canonical hooked DCS parser carry",
        ));
    }
    Ok(DcsQueryCarry {
        kind,
        data,
        sequence_bytes,
        parser,
    })
}

fn enc_kitty_layout(w: &mut W, l: &KittyLayoutCarry) {
    let KittyLayoutCarry {
        cols,
        rows,
        z_index,
        cell_offset_px: (off_x, off_y),
        scaling,
        source_rect,
    } = l;
    w.u16(*cols);
    w.u16(*rows);
    w.i32(*z_index);
    w.u16(*off_x);
    w.u16(*off_y);
    w.u8(match scaling {
        ImageScaling::Fit => 0,
        ImageScaling::Stretch => 1,
        ImageScaling::PixelExact => 2,
    });
    w.opt(*source_rect, |w, r| {
        w.u32(r.x);
        w.u32(r.y);
        w.u32(r.width);
        w.u32(r.height);
    });
}

fn dec_kitty_layout(d: &mut R<'_>) -> Result<KittyLayoutCarry, CanonFault> {
    let cols = d.u16("pane_carry.kitty_layout.cols")?;
    let rows = d.u16("pane_carry.kitty_layout.rows")?;
    if cols == 0 || cols > MAX_GRID_COLS || rows == 0 || rows > MAX_GRID_ROWS {
        return Err(fault(
            "pane_carry.kitty_layout.cols",
            "cols and rows must stay within 1..=MAX_GRID",
        ));
    }
    let z_index = d.i32("pane_carry.kitty_layout.z_index")?;
    let off_x = d.u16("pane_carry.kitty_layout.cell_offset_px")?;
    let off_y = d.u16("pane_carry.kitty_layout.cell_offset_px")?;
    let scaling = match d.u8("pane_carry.kitty_layout.scaling")? {
        0 => ImageScaling::Fit,
        1 => ImageScaling::Stretch,
        2 => ImageScaling::PixelExact,
        _ => {
            return Err(fault(
                "pane_carry.kitty_layout.scaling",
                "scaling tag is 0 (Fit), 1 (Stretch) or 2 (PixelExact)",
            ));
        }
    };
    let source_rect = d.opt("pane_carry.kitty_layout.source_rect", |d| {
        let x = d.u32("pane_carry.kitty_layout.source_rect")?;
        let y = d.u32("pane_carry.kitty_layout.source_rect")?;
        let width = d.u32("pane_carry.kitty_layout.source_rect")?;
        let height = d.u32("pane_carry.kitty_layout.source_rect")?;
        if width == 0
            || height == 0
            || x.checked_add(width).is_none()
            || y.checked_add(height).is_none()
        {
            return Err(fault(
                "pane_carry.kitty_layout.source_rect",
                "source_rect must have non-zero size without u32 overflow",
            ));
        }
        Ok(SourceRect {
            x,
            y,
            width,
            height,
        })
    })?;
    Ok(KittyLayoutCarry {
        cols,
        rows,
        z_index,
        cell_offset_px: (off_x, off_y),
        scaling,
        source_rect,
    })
}

fn enc_kitty_raster(w: &mut W, format: ImageFormat, bytes: &[u8]) {
    match format {
        ImageFormat::Png => {
            w.u8(0);
            w.bytes(bytes);
        }
        ImageFormat::RawRgba8 { width, height } => {
            w.u8(1);
            w.u16(width);
            w.u16(height);
            w.bytes(bytes);
        }
        ImageFormat::Unknown => w.unnamed(format),
    }
}

fn dec_kitty_raster(d: &mut R<'_>) -> Result<(ImageFormat, Vec<u8>), CanonFault> {
    use super::handler::MAX_KITTY_IMAGE_BYTES;
    use super::kitty_graphics::MAX_KITTY_DIMENSION;
    match d.u8("pane_carry.kitty_raster.format")? {
        0 => {
            let bytes = d.bytes("pane_carry.kitty_raster.bytes")?.to_vec();
            if bytes.is_empty() || bytes.len() > MAX_KITTY_IMAGE_BYTES {
                return Err(fault(
                    "pane_carry.kitty_raster.bytes",
                    "PNG payload must be 1..=MAX_KITTY_IMAGE_BYTES",
                ));
            }
            Ok((ImageFormat::Png, bytes))
        }
        1 => {
            let width = d.u16("pane_carry.kitty_raster.format")?;
            let height = d.u16("pane_carry.kitty_raster.format")?;
            if width == 0
                || height == 0
                || u32::from(width) > MAX_KITTY_DIMENSION
                || u32::from(height) > MAX_KITTY_DIMENSION
            {
                return Err(fault(
                    "pane_carry.kitty_raster.format",
                    "RGBA dimensions must be 1..=MAX_KITTY_DIMENSION",
                ));
            }
            let expected = usize::from(width)
                .saturating_mul(usize::from(height))
                .saturating_mul(4);
            let bytes = d.bytes("pane_carry.kitty_raster.bytes")?.to_vec();
            if expected > MAX_KITTY_IMAGE_BYTES || bytes.len() != expected {
                return Err(fault(
                    "pane_carry.kitty_raster.bytes",
                    "RGBA payload must equal 4 * width * height within MAX_KITTY_IMAGE_BYTES",
                ));
            }
            Ok((ImageFormat::RawRgba8 { width, height }, bytes))
        }
        _ => Err(fault(
            "pane_carry.kitty_raster.format",
            "format tag is 0 (Png) or 1 (RawRgba8)",
        )),
    }
}

fn enc_kitty_store(w: &mut W, store: &[KittyImageCarry]) {
    w.len(store.len());
    for entry in store {
        let KittyImageCarry {
            id,
            image_serial,
            shown_frame,
            state,
            max_loops,
            loops,
            shown_at_ns,
            frames,
            variants,
            virtuals,
        } = entry;
        w.u32(*id);
        w.u32(image_serial.get());
        w.u8(*shown_frame);
        w.u8(match state {
            KittyPlayStateCarry::Stopped => 0,
            KittyPlayStateCarry::Loading => 1,
            KittyPlayStateCarry::Running => 2,
        });
        w.u32(*max_loops);
        w.u32(*loops);
        w.u64(*shown_at_ns);
        w.len(frames.len());
        for fc in frames {
            enc_kitty_raster(w, fc.format, &fc.bytes);
            enc_kitty_layout(w, &fc.layout);
            w.u32(fc.gap_ms);
        }
        w.len(variants.len());
        for vc in variants {
            match &vc.source {
                KittyVariantSourceCarry::Frame(idx) => {
                    w.u8(0);
                    w.u8(*idx);
                }
                KittyVariantSourceCarry::Orphan { bytes, format } => {
                    w.u8(1);
                    enc_kitty_raster(w, *format, bytes);
                }
            }
            enc_kitty_layout(w, &vc.layout);
        }
        w.len(virtuals.len());
        for virt in virtuals {
            w.u32(virt.placement_id);
            w.u32(virt.image_serial.get());
            w.u32(virt.serial.get());
            match virt.image {
                KittyVirtualImageCarry::Frame(idx) => {
                    w.u8(0);
                    w.u8(idx);
                }
                KittyVirtualImageCarry::Variant(idx) => {
                    w.u8(1);
                    w.u8(idx);
                }
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "sequential wire decode and canonical validation of Kitty store entries"
)]
fn dec_kitty_store(d: &mut R<'_>) -> Result<Vec<KittyImageCarry>, CanonFault> {
    use super::handler::{
        MAX_KITTY_FRAMES, MAX_KITTY_IMAGES, MAX_KITTY_STORE_BYTES, MAX_KITTY_VARIANTS,
        MAX_KITTY_VIRTUALS,
    };
    use core::num::NonZeroU32;
    let n_images = d.count("pane_carry.kitty_store", 27)?;
    if n_images > MAX_KITTY_IMAGES {
        return Err(fault("pane_carry.kitty_store", "past MAX_KITTY_IMAGES"));
    }
    let mut store = Vec::with_capacity(n_images);
    let mut prev_id = 0u32;
    let mut total_bytes = 0usize;
    let add_bytes = |total: &mut usize, n: usize| -> Result<(), CanonFault> {
        *total = total
            .checked_add(n)
            .ok_or_else(|| fault("pane_carry.kitty_store", "kitty store bytes overflow usize"))?;
        if *total > MAX_KITTY_STORE_BYTES {
            return Err(fault(
                "pane_carry.kitty_store",
                "kitty store bytes exceed MAX_KITTY_STORE_BYTES",
            ));
        }
        Ok(())
    };
    for _ in 0..n_images {
        let id = d.u32("pane_carry.kitty_store.id")?;
        if id <= prev_id {
            return Err(fault(
                "pane_carry.kitty_store.id",
                "image ids must be non-zero and strictly ascending",
            ));
        }
        prev_id = id;
        let image_serial = NonZeroU32::new(d.u32("pane_carry.kitty_store.image_serial")?)
            .ok_or_else(|| {
                fault(
                    "pane_carry.kitty_store.image_serial",
                    "image_serial must be non-zero",
                )
            })?;
        let shown_frame = d.u8("pane_carry.kitty_store.shown_frame")?;
        let state = match d.u8("pane_carry.kitty_store.state")? {
            0 => KittyPlayStateCarry::Stopped,
            1 => KittyPlayStateCarry::Loading,
            2 => KittyPlayStateCarry::Running,
            _ => {
                return Err(fault(
                    "pane_carry.kitty_store.state",
                    "play state tag is 0 (Stopped), 1 (Loading) or 2 (Running)",
                ));
            }
        };
        let max_loops = d.u32("pane_carry.kitty_store.max_loops")?;
        let loops = d.u32("pane_carry.kitty_store.loops")?;
        let shown_at_ns = d.u64("pane_carry.kitty_store.shown_at_ns")?;
        let n_frames = d.count("pane_carry.kitty_store.frames", 17)?;
        if n_frames == 0 || n_frames > MAX_KITTY_FRAMES {
            return Err(fault(
                "pane_carry.kitty_store.frames",
                "frame count must be within 1..=MAX_KITTY_FRAMES",
            ));
        }
        if usize::from(shown_frame) >= n_frames {
            return Err(fault(
                "pane_carry.kitty_store.shown_frame",
                "shown_frame must index into frames",
            ));
        }
        let mut frames = Vec::with_capacity(n_frames);
        for _ in 0..n_frames {
            let (format, bytes) = dec_kitty_raster(d)?;
            let layout = dec_kitty_layout(d)?;
            let gap_ms = d.u32("pane_carry.kitty_store.frames.gap_ms")?;
            add_bytes(&mut total_bytes, bytes.len())?;
            frames.push(KittyFrameCarry {
                bytes,
                format,
                layout,
                gap_ms,
            });
        }
        add_bytes(
            &mut total_bytes,
            frames[usize::from(shown_frame)].bytes.len(),
        )?;

        let n_variants = d.count("pane_carry.kitty_store.variants", 16)?;
        if n_variants > MAX_KITTY_VARIANTS {
            return Err(fault(
                "pane_carry.kitty_store.variants",
                "past MAX_KITTY_VARIANTS",
            ));
        }
        let mut variants: Vec<KittyVariantCarry> = Vec::with_capacity(n_variants);
        for _ in 0..n_variants {
            let source = match d.u8("pane_carry.kitty_store.variants.source")? {
                0 => {
                    let idx = d.u8("pane_carry.kitty_store.variants.source")?;
                    if usize::from(idx) >= n_frames {
                        return Err(fault(
                            "pane_carry.kitty_store.variants.source",
                            "variant source frame out of range",
                        ));
                    }
                    add_bytes(&mut total_bytes, frames[usize::from(idx)].bytes.len())?;
                    KittyVariantSourceCarry::Frame(idx)
                }
                1 => {
                    let (format, bytes) = dec_kitty_raster(d)?;
                    add_bytes(&mut total_bytes, bytes.len())?;
                    KittyVariantSourceCarry::Orphan { bytes, format }
                }
                _ => {
                    return Err(fault(
                        "pane_carry.kitty_store.variants.source",
                        "variant source tag is 0 (Frame) or 1 (Orphan)",
                    ));
                }
            };
            let layout = dec_kitty_layout(d)?;
            if let KittyVariantSourceCarry::Frame(idx) = source {
                if layout == frames[usize::from(idx)].layout {
                    return Err(fault(
                        "pane_carry.kitty_store.variants",
                        "frame variant layout must differ from its source frame",
                    ));
                }
                if variants.iter().any(|prev| {
                    prev.source == KittyVariantSourceCarry::Frame(idx) && prev.layout == layout
                }) {
                    return Err(fault(
                        "pane_carry.kitty_store.variants",
                        "duplicate frame variant layout",
                    ));
                }
            }
            variants.push(KittyVariantCarry { source, layout });
        }

        let n_virtuals = d.count("pane_carry.kitty_store.virtuals", 14)?;
        if n_virtuals > MAX_KITTY_VIRTUALS {
            return Err(fault(
                "pane_carry.kitty_store.virtuals",
                "past MAX_KITTY_VIRTUALS",
            ));
        }
        let mut virtuals: Vec<KittyVirtualCarry> = Vec::with_capacity(n_virtuals);
        for _ in 0..n_virtuals {
            let placement_id = d.u32("pane_carry.kitty_store.virtuals.placement_id")?;
            if placement_id != 0
                && virtuals
                    .iter()
                    .any(|prev| prev.placement_id == placement_id)
            {
                return Err(fault(
                    "pane_carry.kitty_store.virtuals.placement_id",
                    "duplicate non-zero virtual placement_id",
                ));
            }
            let v_image_serial =
                NonZeroU32::new(d.u32("pane_carry.kitty_store.virtuals.image_serial")?)
                    .ok_or_else(|| {
                        fault(
                            "pane_carry.kitty_store.virtuals.image_serial",
                            "virtual image_serial must be non-zero",
                        )
                    })?;
            let serial = NonZeroU32::new(d.u32("pane_carry.kitty_store.virtuals.serial")?)
                .ok_or_else(|| {
                    fault(
                        "pane_carry.kitty_store.virtuals.serial",
                        "virtual serial must be non-zero",
                    )
                })?;
            let image = match d.u8("pane_carry.kitty_store.virtuals.image")? {
                0 => {
                    let idx = d.u8("pane_carry.kitty_store.virtuals.image")?;
                    if usize::from(idx) >= n_frames {
                        return Err(fault(
                            "pane_carry.kitty_store.virtuals.image",
                            "virtual frame index out of range",
                        ));
                    }
                    KittyVirtualImageCarry::Frame(idx)
                }
                1 => {
                    let idx = d.u8("pane_carry.kitty_store.virtuals.image")?;
                    if usize::from(idx) >= n_variants {
                        return Err(fault(
                            "pane_carry.kitty_store.virtuals.image",
                            "virtual variant index out of range",
                        ));
                    }
                    KittyVirtualImageCarry::Variant(idx)
                }
                _ => {
                    return Err(fault(
                        "pane_carry.kitty_store.virtuals.image",
                        "virtual image tag is 0 (Frame) or 1 (Variant)",
                    ));
                }
            };
            virtuals.push(KittyVirtualCarry {
                placement_id,
                image_serial: v_image_serial,
                serial,
                image,
            });
        }

        store.push(KittyImageCarry {
            id,
            image_serial,
            shown_frame,
            state,
            max_loops,
            loops,
            shown_at_ns,
            frames,
            variants,
            virtuals,
        });
    }
    Ok(store)
}

fn dec_kitty_numbers(
    d: &mut R<'_>,
    store: &[KittyImageCarry],
) -> Result<Vec<(u32, u32)>, CanonFault> {
    use super::handler::MAX_KITTY_IMAGES;
    let n = d.count("pane_carry.kitty_numbers", 8)?;
    if n > MAX_KITTY_IMAGES {
        return Err(fault("pane_carry.kitty_numbers", "past MAX_KITTY_IMAGES"));
    }
    let mut out = Vec::with_capacity(n);
    let mut prev_num = 0u32;
    for _ in 0..n {
        let num = d.u32("pane_carry.kitty_numbers")?;
        let id = d.u32("pane_carry.kitty_numbers")?;
        if num <= prev_num {
            return Err(fault(
                "pane_carry.kitty_numbers",
                "image numbers must be non-zero and strictly ascending",
            ));
        }
        prev_num = num;
        if store.binary_search_by_key(&id, |e| e.id).is_err() {
            return Err(fault(
                "pane_carry.kitty_numbers",
                "image number must name a stored image id",
            ));
        }
        out.push((num, id));
    }
    Ok(out)
}

fn enc_kitty_pending(w: &mut W, cmd: &super::kitty_graphics::KittyCommand) {
    use super::kitty_graphics::{KittyAction, KittyFormat, KittyMedium};
    let super::kitty_graphics::KittyCommand {
        action,
        quiet,
        format,
        medium,
        id,
        number,
        placement,
        x,
        y,
        crop_width,
        crop_height,
        cursor_stays,
        virtual_placement,
        width,
        height,
        columns,
        rows,
        z_index,
        cell_x_offset,
        cell_y_offset,
        parent_id,
        parent_placement,
        parent_dx,
        parent_dy,
        more,
        compressed,
        delete_target,
        payload,
    } = cmd;
    w.u8(match action {
        KittyAction::Transmit => 0,
        KittyAction::TransmitAndDisplay => 1,
        KittyAction::Query => 2,
        KittyAction::Display => 3,
        KittyAction::Delete => 4,
        KittyAction::Frame => 5,
        KittyAction::Animate => 6,
        KittyAction::Compose => 7,
    });
    w.u8(*quiet);
    w.u8(match format {
        KittyFormat::Rgb => 0,
        KittyFormat::Rgba => 1,
        KittyFormat::Png => 2,
    });
    w.u8(match medium {
        KittyMedium::Direct => 0,
        KittyMedium::File => 1,
        KittyMedium::TempFile => 2,
        KittyMedium::SharedMemory => 3,
    });
    w.opt(*id, W::u32);
    w.opt(*number, W::u32);
    w.opt(*placement, W::u32);
    w.opt(*x, W::u32);
    w.opt(*y, W::u32);
    w.opt(*crop_width, W::u32);
    w.opt(*crop_height, W::u32);
    w.bool(*cursor_stays);
    w.bool(*virtual_placement);
    w.opt(*width, W::u32);
    w.opt(*height, W::u32);
    w.opt(*columns, W::u32);
    w.opt(*rows, W::u32);
    w.opt(*z_index, W::i32);
    w.opt(*cell_x_offset, W::u32);
    w.opt(*cell_y_offset, W::u32);
    w.opt(*parent_id, W::u32);
    w.opt(*parent_placement, W::u32);
    w.opt(*parent_dx, W::i32);
    w.opt(*parent_dy, W::i32);
    w.bool(*more);
    w.bool(*compressed);
    w.opt(*delete_target, |w, c| w.u32(u32::from(c)));
    w.bytes(payload);
}

#[allow(
    clippy::too_many_lines,
    reason = "sequential wire decode and canonical validation of KittyCommand"
)]
fn dec_kitty_pending(
    d: &mut R<'_>,
    store: &[KittyImageCarry],
    numbers: &[(u32, u32)],
) -> Result<super::kitty_graphics::KittyCommand, CanonFault> {
    use super::handler::MAX_KITTY_IMAGE_BYTES;
    use super::kitty_graphics::{DuringLoad, KittyAction, KittyCommand, KittyFormat, KittyMedium};
    let field = "pane_carry.kitty_pending";
    let action = match d.u8(field)? {
        0 => KittyAction::Transmit,
        1 => KittyAction::TransmitAndDisplay,
        2 => KittyAction::Query,
        3 => KittyAction::Display,
        4 => KittyAction::Delete,
        5 => KittyAction::Frame,
        6 => KittyAction::Animate,
        7 => KittyAction::Compose,
        _ => return Err(fault(field, "unknown KittyAction tag")),
    };
    let quiet = d.u8(field)?;
    let format = match d.u8(field)? {
        0 => KittyFormat::Rgb,
        1 => KittyFormat::Rgba,
        2 => KittyFormat::Png,
        _ => return Err(fault(field, "unknown KittyFormat tag")),
    };
    let medium = match d.u8(field)? {
        0 => KittyMedium::Direct,
        1 => KittyMedium::File,
        2 => KittyMedium::TempFile,
        3 => KittyMedium::SharedMemory,
        _ => return Err(fault(field, "unknown KittyMedium tag")),
    };
    let nz_u32 = |d: &mut R<'_>, sub: &'static str| -> Result<Option<u32>, CanonFault> {
        let val = d.opt(sub, |d| d.u32(sub))?;
        if val == Some(0) {
            return Err(fault(sub, "protocol id must be non-zero when present"));
        }
        Ok(val)
    };
    let id = nz_u32(d, "pane_carry.kitty_pending.id")?;
    let number = nz_u32(d, "pane_carry.kitty_pending.number")?;
    let placement = nz_u32(d, "pane_carry.kitty_pending.placement")?;
    let crop_x = d.opt(field, |d| d.u32(field))?;
    let crop_y = d.opt(field, |d| d.u32(field))?;
    let crop_width = d.opt(field, |d| d.u32(field))?;
    let crop_height = d.opt(field, |d| d.u32(field))?;
    let cursor_stays = d.bool(field)?;
    let virtual_placement = d.bool(field)?;
    let width = d.opt(field, |d| d.u32(field))?;
    let height = d.opt(field, |d| d.u32(field))?;
    let columns = d.opt(field, |d| d.u32(field))?;
    let rows = d.opt(field, |d| d.u32(field))?;
    let z_index = d.opt(field, |d| d.i32(field))?;
    let cell_x_offset = d.opt(field, |d| d.u32(field))?;
    let cell_y_offset = d.opt(field, |d| d.u32(field))?;
    let parent_id = nz_u32(d, "pane_carry.kitty_pending.parent_id")?;
    let parent_placement = nz_u32(d, "pane_carry.kitty_pending.parent_placement")?;
    let parent_dx = d.opt(field, |d| d.i32(field))?;
    let parent_dy = d.opt(field, |d| d.i32(field))?;
    let more = d.bool(field)?;
    let compressed = d.bool(field)?;
    let delete_target = d.opt(field, |d| {
        char::from_u32(d.u32(field)?)
            .ok_or_else(|| fault(field, "delete_target must be a Unicode scalar"))
    })?;
    let payload = d.bytes("pane_carry.kitty_pending.payload")?.to_vec();
    if payload.len() > MAX_KITTY_IMAGE_BYTES {
        return Err(fault(
            "pane_carry.kitty_pending.payload",
            "pending payload exceeds MAX_KITTY_IMAGE_BYTES",
        ));
    }
    let cmd = KittyCommand {
        action,
        quiet,
        format,
        medium,
        id,
        number,
        placement,
        x: crop_x,
        y: crop_y,
        crop_width,
        crop_height,
        cursor_stays,
        virtual_placement,
        width,
        height,
        columns,
        rows,
        z_index,
        cell_x_offset,
        cell_y_offset,
        parent_id,
        parent_placement,
        parent_dx,
        parent_dy,
        more,
        compressed,
        delete_target,
        payload,
    };
    if !cmd.more
        || cmd.during_load() != DuringLoad::Continues
        || (cmd.action == KittyAction::Query && cmd.id.is_none())
        || (cmd.id.is_some() && cmd.number.is_some())
        || !cmd.declares_loadable_size()
    {
        return Err(fault(
            "pane_carry.kitty_pending",
            "kitty_pending must be an in-flight direct loadable transfer",
        ));
    }
    if cmd.action == KittyAction::Frame {
        let resolved = cmd.id.or_else(|| {
            let n = cmd.number?;
            numbers
                .binary_search_by_key(&n, |&(num, _)| num)
                .ok()
                .map(|i| numbers[i].1)
        });
        let has_target =
            resolved.is_some_and(|rid| store.binary_search_by_key(&rid, |e| e.id).is_ok());
        if !has_target {
            // Note: an image could be deleted via `a=d`? No, `a=d` has `DuringLoad::Ends`, which clears `kitty_pending`!
            return Err(fault(
                "pane_carry.kitty_pending",
                "chunked frame transfer must name a stored image",
            ));
        }
    }
    Ok(cmd)
}

fn enc_loose_raster(w: &mut W, format: ImageFormat, bytes: &[u8]) {
    match format {
        ImageFormat::Png => {
            w.u8(0);
            w.bytes(bytes);
        }
        ImageFormat::RawRgba8 { width, height } => {
            w.u8(1);
            w.u16(width);
            w.u16(height);
            w.bytes(bytes);
        }
        ImageFormat::Unknown => {
            w.u8(2);
            w.bytes(bytes);
        }
    }
}

fn dec_loose_raster(d: &mut R<'_>) -> Result<(ImageFormat, Vec<u8>), CanonFault> {
    use super::handler_osc_1337::{MAX_IMAGE_BYTES, MAX_LIVE_IMAGE_BYTES};
    match d.u8("pane_carry.loose_images.format")? {
        0 => {
            let bytes = d.bytes("pane_carry.loose_images.bytes")?.to_vec();
            if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
                return Err(fault(
                    "pane_carry.loose_images.bytes",
                    "PNG payload must be 1..=MAX_IMAGE_BYTES",
                ));
            }
            Ok((ImageFormat::Png, bytes))
        }
        1 => {
            let width = d.u16("pane_carry.loose_images.format")?;
            let height = d.u16("pane_carry.loose_images.format")?;
            if width == 0 || height == 0 {
                return Err(fault(
                    "pane_carry.loose_images.format",
                    "RGBA dimensions must be non-zero",
                ));
            }
            let expected = usize::from(width)
                .saturating_mul(usize::from(height))
                .saturating_mul(4);
            let bytes = d.bytes("pane_carry.loose_images.bytes")?.to_vec();
            if expected > MAX_IMAGE_BYTES || bytes.len() != expected {
                return Err(fault(
                    "pane_carry.loose_images.bytes",
                    "RGBA payload must equal 4 * width * height within MAX_IMAGE_BYTES",
                ));
            }
            Ok((ImageFormat::RawRgba8 { width, height }, bytes))
        }
        2 => {
            let bytes = d.bytes("pane_carry.loose_images.bytes")?.to_vec();
            if bytes.is_empty() || bytes.len() > MAX_LIVE_IMAGE_BYTES {
                return Err(fault(
                    "pane_carry.loose_images.bytes",
                    "unknown-format inline payload must be 1..=MAX_LIVE_IMAGE_BYTES",
                ));
            }
            Ok((ImageFormat::Unknown, bytes))
        }
        _ => Err(fault(
            "pane_carry.loose_images.format",
            "format tag is 0 (Png), 1 (RawRgba8) or 2 (Unknown)",
        )),
    }
}

fn enc_loose_images(w: &mut W, loose: &[LooseImageCarry]) {
    w.len(loose.len());
    for lc in loose {
        enc_loose_raster(w, lc.format, &lc.bytes);
        enc_kitty_layout(w, &lc.layout);
    }
}

fn dec_loose_images(d: &mut R<'_>) -> Result<Vec<LooseImageCarry>, CanonFault> {
    use super::handler_osc_1337::MAX_LIVE_IMAGE_BYTES;
    const MAX_LOOSE_IMAGES: usize = 65_536;
    const MAX_LOOSE_TOTAL_BYTES: usize = 8 * MAX_LIVE_IMAGE_BYTES;
    let n = d.count("pane_carry.loose_images", 24)?;
    if n > MAX_LOOSE_IMAGES {
        return Err(fault("pane_carry.loose_images", "past MAX_LOOSE_IMAGES"));
    }
    let mut out = Vec::with_capacity(n);
    let mut total_bytes = 0usize;
    for _ in 0..n {
        let (format, bytes) = dec_loose_raster(d)?;
        total_bytes = total_bytes.checked_add(bytes.len()).ok_or_else(|| {
            fault(
                "pane_carry.loose_images",
                "loose image bytes overflow usize",
            )
        })?;
        if total_bytes > MAX_LOOSE_TOTAL_BYTES {
            return Err(fault(
                "pane_carry.loose_images",
                "loose image bytes exceed cap",
            ));
        }
        let layout = dec_kitty_layout(d)?;
        out.push(LooseImageCarry {
            bytes,
            format,
            layout,
        });
    }
    Ok(out)
}

fn enc_placed_source(w: &mut W, src: PlacedImageSourceCarry) {
    match src {
        PlacedImageSourceCarry::KittyFrame { image_id, frame } => {
            w.u8(0);
            w.u32(image_id);
            w.u8(frame);
        }
        PlacedImageSourceCarry::KittyVariant { image_id, variant } => {
            w.u8(1);
            w.u32(image_id);
            w.u8(variant);
        }
        PlacedImageSourceCarry::Loose(idx) => {
            w.u8(2);
            w.u32(idx);
        }
    }
}

fn dec_placed_source<'a>(
    d: &mut R<'_>,
    store: &'a [KittyImageCarry],
    loose: &'a [LooseImageCarry],
    next_loose: &mut u32,
    field: &'static str,
) -> Result<(PlacedImageSourceCarry, &'a KittyLayoutCarry), CanonFault> {
    match d.u8(field)? {
        0 => {
            let image_id = d.u32(field)?;
            let frame = d.u8(field)?;
            let pos = store
                .binary_search_by_key(&image_id, |e| e.id)
                .map_err(|_| fault(field, "placed KittyFrame names an unstored image_id"))?;
            let fc = store[pos]
                .frames
                .get(usize::from(frame))
                .ok_or_else(|| fault(field, "placed KittyFrame index out of range"))?;
            Ok((
                PlacedImageSourceCarry::KittyFrame { image_id, frame },
                &fc.layout,
            ))
        }
        1 => {
            let image_id = d.u32(field)?;
            let variant = d.u8(field)?;
            let pos = store
                .binary_search_by_key(&image_id, |e| e.id)
                .map_err(|_| fault(field, "placed KittyVariant names an unstored image_id"))?;
            let vc = store[pos]
                .variants
                .get(usize::from(variant))
                .ok_or_else(|| fault(field, "placed KittyVariant index out of range"))?;
            Ok((
                PlacedImageSourceCarry::KittyVariant { image_id, variant },
                &vc.layout,
            ))
        }
        2 => {
            let idx = d.u32(field)?;
            let slot = usize::try_from(idx)
                .ok()
                .and_then(|i| loose.get(i))
                .ok_or_else(|| fault(field, "placed Loose index out of range"))?;
            if idx > *next_loose {
                return Err(fault(
                    field,
                    "loose images must appear in canonical first-reference order",
                ));
            }
            if idx == *next_loose {
                *next_loose = next_loose.saturating_add(1);
            }
            Ok((PlacedImageSourceCarry::Loose(idx), &slot.layout))
        }
        _ => Err(fault(
            field,
            "placed source tag is 0 (KittyFrame), 1 (KittyVariant) or 2 (Loose)",
        )),
    }
}

fn enc_kitty_tag(w: &mut W, tag: aterm_grid::KittyPlacementTag) {
    w.u32(tag.image_id);
    w.u32(tag.placement_id);
    w.u32(tag.image_serial.get());
    w.u32(tag.serial.get());
}

fn dec_kitty_tag(
    d: &mut R<'_>,
    field: &'static str,
) -> Result<aterm_grid::KittyPlacementTag, CanonFault> {
    use core::num::NonZeroU32;
    let image_id = d.u32(field)?;
    let placement_id = d.u32(field)?;
    let image_serial = NonZeroU32::new(d.u32(field)?)
        .ok_or_else(|| fault(field, "kitty tag image_serial must be non-zero"))?;
    let serial = NonZeroU32::new(d.u32(field)?)
        .ok_or_else(|| fault(field, "kitty tag serial must be non-zero"))?;
    Ok(aterm_grid::KittyPlacementTag {
        image_id,
        placement_id,
        image_serial,
        serial,
    })
}

fn enc_kitty_parent(w: &mut W, parent: KittyParentCarry) {
    match parent {
        KittyParentCarry::Named {
            image_id,
            placement_id,
        } => {
            w.u8(0);
            w.u32(image_id);
            w.u32(placement_id.get());
        }
        KittyParentCarry::Serial { image_id, serial } => {
            w.u8(1);
            w.u32(image_id);
            w.u32(serial.get());
        }
    }
}

fn dec_kitty_parent(d: &mut R<'_>, field: &'static str) -> Result<KittyParentCarry, CanonFault> {
    use core::num::NonZeroU32;
    match d.u8(field)? {
        0 => {
            let image_id = d.u32(field)?;
            let placement_id = NonZeroU32::new(d.u32(field)?)
                .ok_or_else(|| fault(field, "named parent placement_id must be non-zero"))?;
            Ok(KittyParentCarry::Named {
                image_id,
                placement_id,
            })
        }
        1 => {
            let image_id = d.u32(field)?;
            let serial = NonZeroU32::new(d.u32(field)?)
                .ok_or_else(|| fault(field, "serial parent serial must be non-zero"))?;
            Ok(KittyParentCarry::Serial { image_id, serial })
        }
        _ => Err(fault(field, "parent tag is 0 (Named) or 1 (Serial)")),
    }
}

fn enc_kitty_floating(w: &mut W, floating: &[KittyFloatingCarry]) {
    w.len(floating.len());
    for fl in floating {
        enc_kitty_tag(w, fl.tag);
        enc_placed_source(w, fl.image);
        match fl.parent {
            KittyFloatParentCarry::Virtual(p) => {
                w.u8(0);
                enc_kitty_parent(w, p);
            }
            KittyFloatParentCarry::Floating(p) => {
                w.u8(1);
                enc_kitty_parent(w, p);
            }
        }
        w.i32(fl.dx);
        w.i32(fl.dy);
    }
}

fn dec_kitty_floating(
    d: &mut R<'_>,
    store: &[KittyImageCarry],
    loose: &[LooseImageCarry],
    next_loose: &mut u32,
) -> Result<Vec<KittyFloatingCarry>, CanonFault> {
    use super::kitty_floating::MAX_KITTY_FLOATING;
    let n = d.count("pane_carry.kitty_floating", 34)?;
    if n > MAX_KITTY_FLOATING {
        return Err(fault(
            "pane_carry.kitty_floating",
            "past MAX_KITTY_FLOATING",
        ));
    }
    let mut out: Vec<KittyFloatingCarry> = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = dec_kitty_tag(d, "pane_carry.kitty_floating.tag")?;
        if out.iter().any(|prev| prev.tag.serial == tag.serial) {
            return Err(fault(
                "pane_carry.kitty_floating.tag",
                "duplicate floating placement serial",
            ));
        }
        let (image, _) = dec_placed_source(
            d,
            store,
            loose,
            next_loose,
            "pane_carry.kitty_floating.image",
        )?;
        let parent = match d.u8("pane_carry.kitty_floating.parent")? {
            0 => KittyFloatParentCarry::Virtual(dec_kitty_parent(
                d,
                "pane_carry.kitty_floating.parent",
            )?),
            1 => KittyFloatParentCarry::Floating(dec_kitty_parent(
                d,
                "pane_carry.kitty_floating.parent",
            )?),
            _ => {
                return Err(fault(
                    "pane_carry.kitty_floating.parent",
                    "float parent tag is 0 (Virtual) or 1 (Floating)",
                ));
            }
        };
        let dx = d.i32("pane_carry.kitty_floating.dx")?;
        let dy = d.i32("pane_carry.kitty_floating.dy")?;
        out.push(KittyFloatingCarry {
            tag,
            image,
            parent,
            dx,
            dy,
        });
    }
    Ok(out)
}

fn enc_kitty_relations(w: &mut W, relations: &[KittyRelationCarry]) {
    w.len(relations.len());
    for rel in relations {
        enc_kitty_tag(w, rel.child);
        enc_kitty_parent(w, rel.parent);
        w.i32(rel.dx);
        w.i32(rel.dy);
    }
}

fn dec_kitty_relations(d: &mut R<'_>) -> Result<Vec<KittyRelationCarry>, CanonFault> {
    const MAX_RELATIONS: usize = 65_536;
    let n = d.count("pane_carry.kitty_relations", 29)?;
    if n > MAX_RELATIONS {
        return Err(fault("pane_carry.kitty_relations", "past MAX_RELATIONS"));
    }
    let mut out = Vec::with_capacity(n);
    let mut seen_serials = std::collections::HashSet::with_capacity(n);
    for _ in 0..n {
        let child = dec_kitty_tag(d, "pane_carry.kitty_relations.child")?;
        if !seen_serials.insert(child.serial) {
            return Err(fault(
                "pane_carry.kitty_relations.child",
                "duplicate relation child serial",
            ));
        }
        let parent = dec_kitty_parent(d, "pane_carry.kitty_relations.parent")?;
        let dx = d.i32("pane_carry.kitty_relations.dx")?;
        let dy = d.i32("pane_carry.kitty_relations.dy")?;
        out.push(KittyRelationCarry {
            child,
            parent,
            dx,
            dy,
        });
    }
    Ok(out)
}

fn enc_placed_spans(w: &mut W, spans: &[PlacedSpanCarry]) {
    w.len(spans.len());
    for sp in spans {
        w.u16(sp.start_col);
        w.u16(sp.end_col);
        w.u16(sp.image_row);
        w.u16(sp.first_cell_col);
        enc_placed_source(w, sp.image);
        w.opt(sp.kitty, enc_kitty_tag);
    }
}

fn dec_placed_spans(
    d: &mut R<'_>,
    store: &[KittyImageCarry],
    loose: &[LooseImageCarry],
    next_loose: &mut u32,
) -> Result<Vec<PlacedSpanCarry>, CanonFault> {
    let max_spans = usize::from(MAX_GRID_COLS) * aterm_grid::MAX_IMAGE_LAYERS;
    let n = d.count("pane_carry.placed_grid.spans", 14)?;
    if n == 0 || n > max_spans {
        return Err(fault(
            "pane_carry.placed_grid.spans",
            "row span list must be non-empty and within MAX_GRID_COLS * MAX_IMAGE_LAYERS",
        ));
    }
    let mut out = Vec::with_capacity(n);
    let mut prev_key: Option<((i32, u32, u32), u16)> = None;
    for _ in 0..n {
        let start_col = d.u16("pane_carry.placed_grid.span")?;
        let end_col = d.u16("pane_carry.placed_grid.span")?;
        if start_col >= end_col || end_col > MAX_GRID_COLS {
            return Err(fault(
                "pane_carry.placed_grid.span",
                "span columns must satisfy start_col < end_col <= MAX_GRID_COLS",
            ));
        }
        let image_row = d.u16("pane_carry.placed_grid.span")?;
        let first_cell_col = d.u16("pane_carry.placed_grid.span")?;
        let (image, layout) = dec_placed_source(
            d,
            store,
            loose,
            next_loose,
            "pane_carry.placed_grid.span.image",
        )?;
        if image_row >= layout.rows
            || u32::from(first_cell_col).saturating_add(u32::from(end_col - start_col))
                > u32::from(layout.cols)
        {
            return Err(fault(
                "pane_carry.placed_grid.span",
                "span footprint exceeds image layout rows or cols",
            ));
        }
        let kitty = d.opt("pane_carry.placed_grid.span.kitty", |d| {
            dec_kitty_tag(d, "pane_carry.placed_grid.span.kitty")
        })?;
        let key = (aterm_types::layer_order(layout.z_index, kitty), start_col);
        if prev_key.is_some_and(|prev| key < prev) {
            return Err(fault(
                "pane_carry.placed_grid.spans",
                "spans must be sorted by (layer_order, start_col)",
            ));
        }
        prev_key = Some(key);
        out.push(PlacedSpanCarry {
            start_col,
            end_col,
            image_row,
            first_cell_col,
            image,
            kitty,
        });
    }
    Ok(out)
}

fn enc_placed_grid(w: &mut W, grid: &PlacedGridCarry) {
    w.len(grid.visible.len());
    for &(row, ref spans) in &grid.visible {
        w.u16(row);
        enc_placed_spans(w, spans);
    }
    w.len(grid.history_rev.len());
    for &(rev, ref spans) in &grid.history_rev {
        w.u32(rev);
        enc_placed_spans(w, spans);
    }
}

fn dec_placed_grid(
    d: &mut R<'_>,
    store: &[KittyImageCarry],
    loose: &[LooseImageCarry],
    next_loose: &mut u32,
) -> Result<PlacedGridCarry, CanonFault> {
    let n_vis = d.count("pane_carry.placed_grid.visible", 10)?;
    if n_vis > usize::from(MAX_GRID_ROWS) {
        return Err(fault(
            "pane_carry.placed_grid.visible",
            "past MAX_GRID_ROWS",
        ));
    }
    let mut visible = Vec::with_capacity(n_vis);
    let mut prev_row: Option<u16> = None;
    for _ in 0..n_vis {
        let row = d.u16("pane_carry.placed_grid.visible.row")?;
        if row >= MAX_GRID_ROWS || prev_row.is_some_and(|p| row <= p) {
            return Err(fault(
                "pane_carry.placed_grid.visible.row",
                "visible rows must be < MAX_GRID_ROWS and strictly ascending",
            ));
        }
        prev_row = Some(row);
        let spans = dec_placed_spans(d, store, loose, next_loose)?;
        visible.push((row, spans));
    }

    let n_hist = d.count("pane_carry.placed_grid.history_rev", 12)?;
    if n_hist > PANE_HISTORY_IMAGE_LINES {
        return Err(fault(
            "pane_carry.placed_grid.history_rev",
            "past PANE_HISTORY_IMAGE_LINES",
        ));
    }
    let hist_cap = u32::try_from(PANE_HISTORY_IMAGE_LINES).unwrap_or(u32::MAX);
    let mut history_rev = Vec::with_capacity(n_hist);
    let mut prev_rev: Option<u32> = None;
    for _ in 0..n_hist {
        let rev = d.u32("pane_carry.placed_grid.history_rev.rev")?;
        if rev >= hist_cap || prev_rev.is_some_and(|p| rev <= p) {
            return Err(fault(
                "pane_carry.placed_grid.history_rev.rev",
                "history_rev indices must be < PANE_HISTORY_IMAGE_LINES and strictly ascending",
            ));
        }
        prev_rev = Some(rev);
        let spans = dec_placed_spans(d, store, loose, next_loose)?;
        history_rev.push((rev, spans));
    }

    Ok(PlacedGridCarry {
        visible,
        history_rev,
    })
}

impl PaneStepCarry {
    /// Canonical byte encoding of this [`PaneStepCarry`] (`PANE_CARRY_CANON_VERSION`).
    #[must_use]
    pub fn canonical(&self) -> Vec<u8> {
        let Self {
            sync_start_ns,
            sync_timeout_ns,
            response_limiter: (rl_bits, rl_cap, rl_refill, rl_last),
            policy_buckets,
            alt_entered_without_cursor_save,
            conhost_alt_screen_left_up,
            dead_alt_screen,
            dead_alt_prompt_at,
            screen_replaced,
            size_report_held,
            bracketed_paste_from_prompt,
            kitty_last_assigned_id,
            kitty_serial,
            live_image_charge: [charge_main, charge_alt],
            last_osc_bel_terminated,
            pending_restore,
            foreground_job,
            probe_job_active,
            awaiting_shell_nonce,
            typed_rekey_prior,
            active_handback,
            inactive_handback,
            dcs_query,
            kitty_store,
            kitty_numbers,
            kitty_pending,
            loose_images,
            kitty_floating,
            kitty_relations,
            active_images,
            inactive_images,
        } = self;
        let mut w = W(Vec::with_capacity(256));
        w.raw(PANE_CARRY_MAGIC);
        w.u32(PANE_CARRY_CANON_VERSION);
        w.opt_u64(*sync_start_ns);
        w.u64(*sync_timeout_ns);
        w.u64(*rl_bits);
        w.u64(*rl_cap);
        w.u64(*rl_refill);
        w.opt_u64(*rl_last);
        w.len(policy_buckets.len());
        for &(bits, last) in policy_buckets {
            w.u64(bits);
            w.opt_u64(last);
        }
        w.bool(*alt_entered_without_cursor_save);
        w.bool(*conhost_alt_screen_left_up);
        w.opt_u64(*dead_alt_screen);
        w.opt(*dead_alt_prompt_at, |w, (scr, row, col)| {
            w.u64(scr);
            w.u16(row);
            w.u16(col);
        });
        w.u64(*screen_replaced);
        w.bool(*size_report_held);
        w.bool(*bracketed_paste_from_prompt);
        w.u32(*kitty_last_assigned_id);
        w.u32(*kitty_serial);
        w.u64(*charge_main);
        w.u64(*charge_alt);
        w.bool(*last_osc_bel_terminated);
        w.u8(*pending_restore);
        w.bool(*foreground_job);
        w.bool(*probe_job_active);
        w.opt(*awaiting_shell_nonce, |w, n| w.raw(&n.0));
        w.opt(*typed_rekey_prior, |w, (pn, pa)| {
            w.opt(pn, |w, n| w.raw(&n.0));
            w.bool(pa);
        });
        enc_handback(&mut w, *active_handback);
        w.opt(*inactive_handback, enc_handback);
        w.opt(dcs_query.as_ref(), enc_dcs_query);
        enc_kitty_store(&mut w, kitty_store);
        w.len(kitty_numbers.len());
        for &(num, id) in kitty_numbers {
            w.u32(num);
            w.u32(id);
        }
        w.opt(kitty_pending.as_ref(), enc_kitty_pending);
        enc_loose_images(&mut w, loose_images);
        enc_kitty_floating(&mut w, kitty_floating);
        enc_kitty_relations(&mut w, kitty_relations);
        enc_placed_grid(&mut w, active_images);
        w.opt(inactive_images.as_ref(), enc_placed_grid);
        w.0
    }

    /// Decode a [`PaneStepCarry`] from canonical bytes ([`Self::canonical`]).
    ///
    /// # Errors
    /// Returns [`CanonFault`] naming the first non-canonical field.
    #[allow(
        clippy::too_many_lines,
        reason = "sequential wire decode and canonical validation of PaneStepCarry"
    )]
    pub fn from_canonical(bytes: &[u8]) -> Result<Self, CanonFault> {
        let mut d = R { b: bytes };
        d.magic(
            "pane_carry.magic",
            PANE_CARRY_MAGIC,
            PANE_CARRY_CANON_VERSION,
        )?;
        let f = "pane_carry";
        let sync_start_ns = d.opt_u64("pane_carry.sync_start_ns")?;
        let sync_timeout_ns = d.u64(f)?;
        let rl_bits = d.u64("pane_carry.response_limiter")?;
        let rl_cap = d.u64("pane_carry.response_limiter")?;
        let rl_refill = d.u64("pane_carry.response_limiter")?;
        let rl_last = d.opt_u64("pane_carry.response_limiter")?;
        validate_nonneg_f64_bits(rl_bits, "pane_carry.response_limiter")?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "capacity is bytes; matches ResponseRateLimiter::from_snapshot clamp"
        )]
        if f64::from_bits(rl_bits) > rl_cap as f64 {
            return Err(fault(
                "pane_carry.response_limiter",
                "tokens exceed capacity_bytes",
            ));
        }
        let n_buckets = d.count("pane_carry.policy_buckets", 9)?;
        if n_buckets > 1024 {
            return Err(fault("pane_carry.policy_buckets", "past bucket cap"));
        }
        let mut policy_buckets = Vec::with_capacity(n_buckets);
        for _ in 0..n_buckets {
            let bits = d.u64("pane_carry.policy_buckets")?;
            validate_nonneg_f64_bits(bits, "pane_carry.policy_buckets")?;
            let last = d.opt_u64("pane_carry.policy_buckets")?;
            policy_buckets.push((bits, last));
        }
        let alt_entered_without_cursor_save = d.bool(f)?;
        let conhost_alt_screen_left_up = d.bool(f)?;
        let dead_alt_screen = d.opt_u64(f)?;
        let dead_alt_prompt_at = d.opt("pane_carry.dead_alt_prompt_at", |d| {
            Ok((d.u64(f)?, d.u16(f)?, d.u16(f)?))
        })?;
        let screen_replaced = d.u64(f)?;
        let size_report_held = d.bool(f)?;
        let bracketed_paste_from_prompt = d.bool(f)?;
        let kitty_last_assigned_id = d.u32(f)?;
        let kitty_serial = d.u32(f)?;
        let charge_main = d.u64(f)?;
        let charge_alt = d.u64(f)?;
        let last_osc_bel_terminated = d.bool(f)?;
        let pending_restore = d.u8("pane_carry.pending_restore")?;
        if pending_restore > 2 {
            return Err(fault(
                "pane_carry.pending_restore",
                "pending_restore is 0, 1 or 2",
            ));
        }
        let (foreground_job, probe_job_active, awaiting_shell_nonce, typed_rekey_prior) =
            dec_host_shell_carry(&mut d)?;
        let active_handback = dec_handback(&mut d, "pane_carry.active_handback")?;
        let inactive_handback = d.opt("pane_carry.inactive_handback", |d| {
            dec_handback(d, "pane_carry.inactive_handback")
        })?;
        let dcs_query = d.opt("pane_carry.dcs_query", dec_dcs_query)?;
        let kitty_store = dec_kitty_store(&mut d)?;
        let kitty_numbers = dec_kitty_numbers(&mut d, &kitty_store)?;
        let kitty_pending = d.opt("pane_carry.kitty_pending", |d| {
            dec_kitty_pending(d, &kitty_store, &kitty_numbers)
        })?;
        let loose_images = dec_loose_images(&mut d)?;
        let mut next_loose = 0u32;
        let kitty_floating =
            dec_kitty_floating(&mut d, &kitty_store, &loose_images, &mut next_loose)?;
        let kitty_relations = dec_kitty_relations(&mut d)?;
        let active_images = dec_placed_grid(&mut d, &kitty_store, &loose_images, &mut next_loose)?;
        let inactive_images = d.opt("pane_carry.inactive_images", |d| {
            dec_placed_grid(d, &kitty_store, &loose_images, &mut next_loose)
        })?;
        if inactive_images.is_some() != inactive_handback.is_some() {
            return Err(fault(
                "pane_carry.inactive_images",
                "inactive_images presence must match inactive_handback",
            ));
        }
        if usize::try_from(next_loose).ok() != Some(loose_images.len()) {
            return Err(fault(
                "pane_carry.loose_images",
                "every loose image must be referenced at least once",
            ));
        }
        d.end("pane_carry")?;
        let carry = Self {
            sync_start_ns,
            sync_timeout_ns,
            response_limiter: (rl_bits, rl_cap, rl_refill, rl_last),
            policy_buckets,
            alt_entered_without_cursor_save,
            conhost_alt_screen_left_up,
            dead_alt_screen,
            dead_alt_prompt_at,
            screen_replaced,
            size_report_held,
            bracketed_paste_from_prompt,
            kitty_last_assigned_id,
            kitty_serial,
            live_image_charge: [charge_main, charge_alt],
            last_osc_bel_terminated,
            pending_restore,
            foreground_job,
            probe_job_active,
            awaiting_shell_nonce,
            typed_rekey_prior,
            active_handback,
            inactive_handback,
            dcs_query,
            kitty_store,
            kitty_numbers,
            kitty_pending,
            loose_images,
            kitty_floating,
            kitty_relations,
            active_images,
            inactive_images,
        };
        if carry.canonical() != bytes {
            return Err(fault(
                "pane_carry.canonical",
                "the value re-encodes to other bytes",
            ));
        }
        Ok(carry)
    }

    /// Whether `policy_buckets` matches `policy`'s declared rate-limit count and capacities.
    #[must_use]
    pub fn matches_policy(&self, policy: Option<&aterm_policy::Policy>) -> bool {
        match policy {
            None => self.policy_buckets.is_empty(),
            Some(p) => {
                let fresh = aterm_policy::limits::RateLimiterSet::from_policy(p).snapshots();
                self.policy_buckets.len() == fresh.len()
                    && self
                        .policy_buckets
                        .iter()
                        .zip(fresh.iter())
                        .all(|(&(bits, _), cap)| f64::from_bits(bits) <= cap.tokens)
            }
        }
    }
}

/// Whether `schemes` is the canonical output of [`super::Terminal::hyperlink_extra_schemes`]: lowercase RFC 3986 shapes, allowed, distinct, and within cap.
#[must_use]
pub fn are_canonical_hyperlink_schemes(schemes: &[String]) -> bool {
    let mut auth = super::hyperlink_auth::HyperlinkAuth::new();
    for s in schemes {
        if !auth.authorize_scheme(s) {
            return false;
        }
    }
    let stored = auth.extra_schemes();
    stored.len() == schemes.len()
        && stored
            .iter()
            .zip(schemes.iter())
            .all(|(canon, given)| &**canon == given.as_str())
}

fn enc_policy_profile(w: &mut W, p: aterm_policy::Profile) {
    use aterm_policy::Profile;
    match p {
        Profile::Permissive => w.u8(0),
        Profile::Standard => w.u8(1),
        Profile::Hardened => w.u8(2),
        other => w.unnamed(other),
    }
}

fn dec_policy_profile(
    d: &mut R<'_>,
    field: &'static str,
) -> Result<aterm_policy::Profile, CanonFault> {
    use aterm_policy::Profile;
    match d.u8(field)? {
        0 => Ok(Profile::Permissive),
        1 => Ok(Profile::Standard),
        2 => Ok(Profile::Hardened),
        _ => Err(fault(field, "unknown policy profile tag")),
    }
}

fn enc_policy_origin(w: &mut W, o: aterm_policy::OriginTag) {
    use aterm_policy::OriginTag;
    match o {
        OriginTag::Host => w.u8(0),
        OriginTag::ConfigFile => w.u8(1),
        OriginTag::User => w.u8(2),
        OriginTag::UserTyped => w.u8(3),
        OriginTag::Ai => w.u8(4),
        OriginTag::PtySafe => w.u8(5),
        OriginTag::Pty => w.u8(6),
        OriginTag::NetworkUntrusted => w.u8(7),
        other => w.unnamed(other),
    }
}

fn dec_policy_origin(
    d: &mut R<'_>,
    field: &'static str,
) -> Result<aterm_policy::OriginTag, CanonFault> {
    use aterm_policy::OriginTag;
    match d.u8(field)? {
        0 => Ok(OriginTag::Host),
        1 => Ok(OriginTag::ConfigFile),
        2 => Ok(OriginTag::User),
        3 => Ok(OriginTag::UserTyped),
        4 => Ok(OriginTag::Ai),
        5 => Ok(OriginTag::PtySafe),
        6 => Ok(OriginTag::Pty),
        7 => Ok(OriginTag::NetworkUntrusted),
        _ => Err(fault(field, "unknown policy origin tag")),
    }
}

fn enc_policy_response(w: &mut W, r: aterm_policy::Response) {
    use aterm_policy::Response;
    match r {
        Response::Drop => w.u8(0),
        Response::Warn => w.u8(1),
        Response::Execute => w.u8(2),
        Response::Ask => w.u8(3),
        Response::Rewrite => w.u8(4),
        other => w.unnamed(other),
    }
}

fn dec_policy_response(
    d: &mut R<'_>,
    field: &'static str,
) -> Result<aterm_policy::Response, CanonFault> {
    use aterm_policy::Response;
    match d.u8(field)? {
        0 => Ok(Response::Drop),
        1 => Ok(Response::Warn),
        2 => Ok(Response::Execute),
        3 => Ok(Response::Ask),
        4 => Ok(Response::Rewrite),
        _ => Err(fault(field, "unknown policy response tag")),
    }
}

/// Canonical byte encoding of an [`aterm_policy::Policy`] document (`POLICY_CANON_VERSION`).
#[must_use]
pub fn policy_canonical(policy: &aterm_policy::Policy) -> Vec<u8> {
    let aterm_policy::Policy {
        schema_version,
        profile,
        defaults:
            aterm_policy::Defaults {
                unmatched,
                shell_integration_require_nonce,
            },
        rules,
        rate_limits,
    } = policy;
    let mut w = W(Vec::with_capacity(512));
    w.raw(POLICY_MAGIC);
    w.u32(POLICY_CANON_VERSION);
    w.u32(*schema_version);
    enc_policy_profile(&mut w, *profile);
    enc_policy_response(&mut w, *unmatched);
    w.bool(*shell_integration_require_nonce);
    w.len(rules.len());
    for rule in rules {
        let aterm_policy::Rule {
            sequence,
            origin_min,
            response,
            rate_limit,
            prompt_id,
        } = rule;
        w.str(sequence);
        enc_policy_origin(&mut w, *origin_min);
        enc_policy_response(&mut w, *response);
        w.opt_str(rate_limit.as_deref());
        w.opt_str(prompt_id.as_deref());
    }
    w.len(rate_limits.len());
    for rl in rate_limits {
        let aterm_policy::RateLimit {
            id,
            capacity_bytes,
            refill_per_second,
            per_sequence_max,
        } = rl;
        w.str(id);
        w.u32(*capacity_bytes);
        w.u32(*refill_per_second);
        w.u32(*per_sequence_max);
    }
    w.0
}

/// Decode an [`aterm_policy::Policy`] document from canonical bytes ([`policy_canonical`]).
///
/// # Errors
/// Returns [`CanonFault`] naming the first non-canonical field.
pub fn policy_from_canonical(bytes: &[u8]) -> Result<aterm_policy::Policy, CanonFault> {
    let mut d = R { b: bytes };
    d.magic("policy.magic", POLICY_MAGIC, POLICY_CANON_VERSION)?;
    let schema_version = d.u32("policy.schema_version")?;
    let profile = dec_policy_profile(&mut d, "policy.profile")?;
    let unmatched = dec_policy_response(&mut d, "policy.defaults.unmatched")?;
    let shell_integration_require_nonce = d.bool("policy.defaults.nonce")?;
    let n_rules = d.count("policy.rules", 12)?;
    let mut rules = Vec::with_capacity(n_rules);
    for _ in 0..n_rules {
        rules.push(aterm_policy::Rule {
            sequence: d.string("policy.rules.sequence")?,
            origin_min: dec_policy_origin(&mut d, "policy.rules.origin_min")?,
            response: dec_policy_response(&mut d, "policy.rules.response")?,
            rate_limit: d.opt("policy.rules.rate_limit", |d| {
                d.string("policy.rules.rate_limit")
            })?,
            prompt_id: d.opt("policy.rules.prompt_id", |d| {
                d.string("policy.rules.prompt_id")
            })?,
        });
    }
    let n_limits = d.count("policy.rate_limits", 20)?;
    let mut rate_limits = Vec::with_capacity(n_limits);
    for _ in 0..n_limits {
        rate_limits.push(aterm_policy::RateLimit {
            id: d.string("policy.rate_limits.id")?,
            capacity_bytes: d.u32("policy.rate_limits.capacity_bytes")?,
            refill_per_second: d.u32("policy.rate_limits.refill_per_second")?,
            per_sequence_max: d.u32("policy.rate_limits.per_sequence_max")?,
        });
    }
    d.end("policy")?;
    let policy = aterm_policy::Policy {
        schema_version,
        profile,
        defaults: aterm_policy::Defaults {
            unmatched,
            shell_integration_require_nonce,
        },
        rules,
        rate_limits,
    };
    if policy_canonical(&policy) != bytes {
        return Err(fault(
            "policy.canonical",
            "the value re-encodes to other bytes",
        ));
    }
    Ok(policy)
}

#[cfg(test)]
#[path = "checkpoint_image_regressions.rs"]
mod image_replay_regressions;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;

    /// A screen that sets most of what a checkpoint carries: styles, history, margins, tab stops, charsets (a 96-set too), both keyboards, progress, cwd, colours and their stack, titles, shell marks, an SGR push, a link, saved modes, both screens and a saved cursor.
    fn rich(rows: u16, cols: u16) -> Terminal {
        let mut t = Terminal::new(rows, cols);
        t.modes.allow_palette_reconfigure = true;
        for bytes in [
            &b"\x1b[1;4;38;5;202;48;2;10;20;30mstyled\x1b[0m\r\n"[..],
            b"\x1b[2;5r\x1b[?69h\x1b[2;30s\x1b[?69l\x1b[r",
            b"\x1b[3g\x1b[1;5H\x1bH\x1b[4;1H",
            b"\x1b(0\x1b-A\x1b~",
            b"\x1b[>5u\x1b[>4;2m\x1b]9;4;1;42\x07\x1b]7;file://host/tmp/work\x07",
            b"\x1b]4;1;rgb:12/34/56\x07\x1b]10;rgb:aa/bb/cc\x07\x1b]12;rgb:01/02/03\x07",
            b"\x1b]30001\x07\x1b]4;2;rgb:65/43/21\x07",
            b"\x1b]0;a title\x07\x1b[22;0t\x1b]2;second\x07",
            b"\x1b]133;A\x07$ \x1b]133;B\x07true\r\n\x1b]133;C\x07\x1b]133;D;0\x07",
            b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 9\r\n\x1b]133;C\x07",
            b"\x1b[3;4m\x1b[#{\x1b[0m\x1b]8;id=k;https://example.com\x07link",
            b"\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?1000s\x1b[?2004s",
            b"\x1b7\x1b[5;9H\x1b[?1049hALT\r\n\x1b[2;2H\x1b7",
        ] {
            t.process(bytes);
        }
        for i in 0..30 {
            t.process(format!("line{i}\r\n").as_bytes());
        }
        t
    }

    fn states() -> Vec<TerminalCheckpoint> {
        let mut out = Vec::new();
        let plain = Terminal::new(4, 10);
        out.push(plain.checkpoint());
        let mut t = rich(8, 40);
        out.push(t.checkpoint());
        out.push(t.checkpoint_visible().expect("at ground"));
        t.process(b"\x1b[?1049l\x1b[1;3");
        out.push(t.checkpoint_continuing().expect("a CSI carries"));
        t.process(b"m\x1b]2;unterminated");
        out.push(t.checkpoint_continuing().expect("an OSC carries"));
        out
    }

    #[test]
    fn a_checkpoint_decodes_to_itself_and_reencodes_to_its_bytes() {
        for (i, c) in states().into_iter().enumerate() {
            let bytes = c.canonical();
            let back = TerminalCheckpoint::from_canonical(&bytes)
                .unwrap_or_else(|e| panic!("state {i}: {e}"));
            assert_eq!(back, c, "state {i}");
            assert_eq!(back.canonical(), bytes, "state {i}");
        }
    }

    #[test]
    fn the_rich_state_reaches_the_fields_it_is_meant_to() {
        let c = rich(8, 40).checkpoint();
        assert!(c.alt_grid.is_some() && c.modes.alternate_screen);
        assert!(c.saved_cursor_main.is_some() && c.saved_cursor_alt.is_some());
        assert!(
            c.charset.designated_96(1).is_some(),
            "ESC - A designates G1"
        );
        assert!(!c.color.palette.is_empty() && !c.color.stack_overrides.is_empty());
        assert!(c.title.as_ref().is_some_and(|t| !t.stack.is_empty()));
        assert!(!c.output_state.sgr_stack.is_empty());
        assert!(!c.output_state.saved_modes.is_empty());
        assert!(c.taskbar_progress.is_some() && c.current_working_directory.is_some());
        let mut partial = rich(8, 40);
        partial.process(b"\x1b[1;3");
        assert!(
            partial
                .checkpoint_continuing()
                .is_some_and(|c| c.parser.is_some()),
            "a carried CSI"
        );
    }

    #[test]
    fn a_poisoned_field_changes_the_bytes() {
        let c = rich(8, 40).checkpoint();
        let base = c.canonical();
        let mut poisons: Vec<(&str, TerminalCheckpoint)> = Vec::new();
        let mut p = |name, f: fn(&mut TerminalCheckpoint)| {
            let mut x = c.clone();
            f(&mut x);
            poisons.push((name, x));
        };
        p("modes", |c| {
            c.modes.cursor_visible = !c.modes.cursor_visible
        });
        p("modes.last", |c| {
            c.modes.ambiguous_width_double = !c.modes.ambiguous_width_double;
        });
        p("cursor", |c| c.cursor.cursor_col ^= 1);
        p("saved cursor resized", |c| {
            let saved = c.saved_cursor_main.as_mut().expect("a DECSC slot");
            saved.resized = SavedCursorResized::new(!saved.resized.get());
        });
        p("tab stops", |c| {
            c.cursor.tab_stops[0] = !c.cursor.tab_stops[0]
        });
        p("grid", |c| {
            let last = c.grid.len() - 1;
            c.grid[last] ^= 1;
        });
        p("charset 96", |c| {
            c.charset.designate(1, CharacterSet::Ascii)
        });
        p("kitty", |c| c.kitty_keyboard.alt_sp ^= 1);
        p("xterm keyboard", |c| {
            c.xterm_keyboard.disable_modify_other_keys()
        });
        p("cwd", |c| c.current_working_directory = None);
        p("nonce", |c| {
            c.shell_integration_nonce = Some(ShellIntegrationNonce([7; 32]))
        });
        p("color", |c| {
            c.color.foreground = Some(Rgb { r: 1, g: 2, b: 4 })
        });
        p("shell", |c| c.shell.completed_seq += 1);
        p("title", |c| {
            c.title.as_mut().expect("title").window.push('x')
        });
        p("output", |c| {
            c.output_state.zwj_pending = !c.output_state.zwj_pending
        });
        p("holder", |c| {
            c.holder.seed_untrusted = !c.holder.seed_untrusted
        });
        p("rev", |c| {
            c.shell_integration_rev = Some("0123456789abcdef".into())
        });
        for (name, x) in poisons {
            assert_ne!(x, c, "{name}: precondition, the poison changes the value");
            assert_ne!(x.canonical(), base, "{name}");
        }
    }

    #[test]
    fn bytes_that_are_not_canonical_are_refused_by_field() {
        let c = rich(8, 40).checkpoint();
        let bytes = c.canonical();
        let refused = |b: &[u8]| TerminalCheckpoint::from_canonical(b).expect_err("refused");
        assert_eq!(refused(&bytes[..bytes.len() - 1]).field, "holder");
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(refused(&trailing).why, "bytes follow the value");
        let mut magic = bytes.clone();
        magic[0] ^= 1;
        assert_eq!(refused(&magic).field, "magic");
        let mut version = bytes.clone();
        version[CHECKPOINT_MAGIC.len()] ^= 1;
        assert_eq!(refused(&version).why, "a version this build does not read");
        let mut geometry = bytes.clone();
        let at = CHECKPOINT_MAGIC.len() + 4;
        geometry[at..at + 2].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(refused(&geometry).field, "rows");
        let mut history = c.clone();
        history.history_lines += 1;
        assert_eq!(refused(&history.canonical()).field, "history_lines");
        let mut alt = c.clone();
        alt.alt_cursor = None;
        assert_eq!(refused(&alt.canonical()).field, "alt_cursor");
        let mut grid = c.clone();
        grid.grid.truncate(grid.grid.len() / 2);
        assert_eq!(refused(&grid.canonical()).field, "grid");
    }

    #[test]
    fn a_config_decodes_to_itself_and_a_poisoned_field_changes_its_bytes() {
        let mut config = TerminalConfig::default();
        assert_eq!(
            TerminalConfig::from_canonical(&config.canonical()),
            Ok(config.clone())
        );
        let base = config.canonical();
        let mut palette = ColorPalette::new();
        palette.set(3, Rgb { r: 9, g: 8, b: 7 });
        config.custom_palette = Some(palette);
        config.color_scheme = Some(Appearance::Light);
        config.scrollback_limit = Some(77);
        config.faint_opacity = 0.25;
        config.cursor_style = CursorStyle::SteadyBar;
        let bytes = config.canonical();
        assert_ne!(bytes, base);
        assert_eq!(TerminalConfig::from_canonical(&bytes), Ok(config.clone()));
        let mut poisoned = config.clone();
        poisoned.bidi.reorder_nsm = !poisoned.bidi.reorder_nsm;
        assert_ne!(poisoned.canonical(), bytes, "the last field counts");
        let mut cut = bytes.clone();
        cut.pop();
        assert!(TerminalConfig::from_canonical(&cut).is_err());
    }

    fn reply(t: &mut Terminal, query: &[u8]) -> Vec<u8> {
        let _ = t.take_response();
        t.process(query);
        t.take_response().unwrap_or_default()
    }

    /// A replay restored from a checkpoint takes the session's colours, cursor and mode seeds from its config and keeps every mode the program set.
    #[test]
    fn a_restored_engine_under_the_sessions_config_answers_as_the_live_one() {
        let mut config = TerminalConfig::default();
        config.default_background = Rgb {
            r: 0xfa,
            g: 0xf0,
            b: 0xe6,
        };
        config.default_foreground = Rgb {
            r: 0x10,
            g: 0x20,
            b: 0x30,
        };
        config.cursor_style = CursorStyle::SteadyBar;
        config.color_scheme = Some(Appearance::Light);
        config.bracketed_paste = false;
        let live = || {
            let mut t = Terminal::new(6, 30);
            let _ = t.apply_config(&config);
            t.process(b"\x1b[?2004h\x1b[1 q\x1b]4;5;rgb:01/02/03\x07");
            let _ = t.take_response();
            t
        };
        let cp = live().checkpoint();
        for keep in [false, true] {
            let mut replay = Terminal::replay_from_checkpoint(&cp);
            if keep {
                replay.apply_config_keeping_modes(&config);
                let _ = replay.take_response();
            }
            let mut probe = live();
            assert_eq!(
                replay.checkpoint().canonical(),
                cp.canonical(),
                "keep: {keep}"
            );
            assert_eq!(replay.modes, probe.modes, "keep: {keep}");
            let queries = [
                &b"\x1b]11;?\x07"[..],
                b"\x1b]10;?\x07",
                b"\x1b]4;5;?\x07",
                b"\x1b[?996n",
            ];
            let answers = queries.map(|q| reply(&mut replay, q) == reply(&mut probe, q));
            assert_eq!(
                answers,
                [keep, keep, true, keep],
                "keep: {keep} (a program's OSC 4 crosses in the checkpoint either way)"
            );
            for t in [&mut replay, &mut probe] {
                t.process(b"\x1b[0 q\x1b[!p");
            }
            assert_eq!(
                replay.modes == probe.modes,
                keep,
                "DECSCUSR 0 and DECSTR, keep: {keep}"
            );
        }
    }

    #[test]
    fn pane_step_carry_roundtrips_and_rejects_noncanonical_shell_rekey_priors() {
        let mut t = Terminal::new(4, 20);
        t.authorize_shell_integration_on_first_mark([0x11; 32]);
        t.authorize_shell_integration_rekey([0x22; 32]);
        t.set_foreground_job(true);
        let origin = aterm_time::Instant::now();
        let carry = PaneStepCarry::capture(&t, origin, 2048);
        assert!(carry.foreground_job);
        assert_eq!(
            carry.awaiting_shell_nonce,
            Some(ShellIntegrationNonce([0x22; 32]))
        );
        assert_eq!(
            carry.typed_rekey_prior,
            Some((Some(ShellIntegrationNonce([0x11; 32])), true))
        );
        let bytes = carry.canonical();
        assert_eq!(PaneStepCarry::from_canonical(&bytes), Ok(carry.clone()));

        let mut flipped_fg = carry.clone();
        flipped_fg.foreground_job = false;
        assert_ne!(flipped_fg.canonical(), bytes);

        let mut orphan_prior = carry.clone();
        orphan_prior.awaiting_shell_nonce = None;
        assert_eq!(
            PaneStepCarry::from_canonical(&orphan_prior.canonical())
                .expect_err("typed_rekey_prior without awaiting_shell_nonce")
                .field,
            "pane_carry.typed_rekey_prior"
        );

        let mut impossible_prior = carry.clone();
        impossible_prior.typed_rekey_prior = Some((None, true));
        assert_eq!(
            PaneStepCarry::from_canonical(&impossible_prior.canonical())
                .expect_err("awaiting prior without nonce")
                .field,
            "pane_carry.typed_rekey_prior"
        );

        let mut both_jobs = carry;
        both_jobs.probe_job_active = true;
        assert_eq!(
            PaneStepCarry::from_canonical(&both_jobs.canonical())
                .expect_err("probe_job_active while foreground_job is true")
                .field,
            "pane_carry.probe_job_active"
        );
    }

    #[test]
    fn pane_step_carry_dcs_query_roundtrips_and_rejects_noncanonical_carries() {
        let origin = aterm_time::Instant::now();
        for (head, tail) in [
            (&b"\x1bP$q"[..], &b"m\x1b\\"[..]),
            (&b"\x1bP$qm\x1b"[..], &b"\\"[..]),
            (&b"\x1bP+q54"[..], &b"4E\x1b\\"[..]),
            (&b"\x1bPtmux;\x1b\x1bP$qm\x1b"[..], &b"\x1b\\\x1b\\"[..]),
        ] {
            let mut live = Terminal::new(4, 20);
            live.process(head);
            let cp = live
                .checkpoint_pane_step(2048)
                .expect("pane step carries query");
            assert!(!cp.parser_ground && cp.parser.is_none());
            let carry = PaneStepCarry::capture(&live, origin, 2048);
            assert!(carry.dcs_query.is_some(), "{head:?}");
            let bytes = carry.canonical();
            let decoded = PaneStepCarry::from_canonical(&bytes).expect("canonical dcs_query");
            assert_eq!(decoded, carry);

            let mut replay = Terminal::replay_from_checkpoint(&cp);
            replay.apply_pane_step_carry(&decoded, origin);
            live.process(tail);
            replay.process(tail);
            assert_eq!(replay.take_response(), live.take_response(), "{head:?}");
            assert_eq!(replay.checkpoint(), live.checkpoint(), "{head:?}");
        }

        let mut base_term = Terminal::new(4, 20);
        base_term.process(b"\x1bP$qm");
        let valid = PaneStepCarry::capture(&base_term, origin, 2048);

        let mut bad_seq = valid.clone();
        bad_seq.dcs_query.as_mut().unwrap().sequence_bytes = 5;
        assert_eq!(
            PaneStepCarry::from_canonical(&bad_seq.canonical())
                .expect_err("data.len() != sequence_bytes.min(cap)")
                .field,
            "pane_carry.dcs_query.sequence_bytes"
        );

        let mut over_budget = valid.clone();
        over_budget.dcs_query.as_mut().unwrap().data = vec![b'm'; 256];
        over_budget.dcs_query.as_mut().unwrap().sequence_bytes =
            u32::try_from(super::super::MAX_DCS_GLOBAL_BUDGET + 1).unwrap();
        assert_eq!(
            PaneStepCarry::from_canonical(&over_budget.canonical())
                .expect_err("sequence_bytes > MAX_DCS_GLOBAL_BUDGET")
                .field,
            "pane_carry.dcs_query.sequence_bytes"
        );

        let mut bad_parser = valid;
        bad_parser.dcs_query.as_mut().unwrap().parser = HookedDcsCarry {
            st_pending: true,
            string_utf8_tail: vec![0xE6],
            tmux: TmuxPassthrough::default(),
        };
        assert_eq!(
            PaneStepCarry::from_canonical(&bad_parser.canonical())
                .expect_err("st_pending with string_utf8_tail")
                .field,
            "pane_carry.dcs_query.parser"
        );
    }

    #[test]
    fn pane_step_carry_kitty_store_numbers_and_pending_roundtrip_and_reject_noncanonical() {
        use crate::terminal::{KittyLayoutCarry, KittyVariantCarry, KittyVariantSourceCarry};
        use aterm_types::{ImageFormat, ImageScaling};

        let origin = aterm_time::Instant::now();
        let cfg = crate::config::TerminalConfig::default();
        let mut live = Terminal::new(6, 30);
        live.set_cell_pixel_size(10, 20);
        // Image 1 (assigned via number 42): 2 RGBA frames (3x1 = 12 bytes each), shown_frame = 1
        // (c=2), playing, two U=1 virtual placements (one cropped variant, one stretched), and a
        // placeholder cell.
        live.process(b"\x1b_Ga=t,f=32,s=3,v=1,I=42;////AAAA////AAAA\x1b\\");
        live.process(b"\x1b_Ga=f,f=32,s=3,v=1,i=1,z=80;AAAA////AAAA////\x1b\\");
        live.process(b"\x1b_Ga=a,i=1,c=2,s=3\x1b\\");
        live.process(b"\x1b_Ga=p,U=1,i=1,p=2,x=1,y=0,w=2,h=1,c=2,r=1\x1b\\");
        live.process(b"\x1b_Ga=p,U=1,i=1,p=3,c=3,r=1\x1b\\");
        live.process("\x1b[2;1H\x1b[38;5;1m\u{10EEEE}\u{0305}\u{0305}\x1b[39m".as_bytes());
        // Image 9: 1x1 RGB (expanded to 1x1 RGBA = 4 bytes) plus an in-flight chunked transfer (`m=1`).
        live.process(b"\x1b_Ga=t,f=24,s=1,v=1,i=9;/wAA\x1b\\");
        live.process(b"\x1b_Ga=t,f=24,s=1,v=1,i=12,m=1;AP\x1b\\");
        let _ = live.take_response();

        let cp = live.checkpoint_pane_step(2048).expect("at ground");
        let carry = PaneStepCarry::capture(&live, origin, 2048);
        assert_eq!(carry.kitty_store.len(), 2);
        assert_eq!(carry.kitty_numbers, vec![(42, 1)]);
        assert!(carry.kitty_pending.is_some());

        let replay_carry = live.capture_replay_carry();
        assert!(replay_carry.retained_image_bytes() >= 12 + 12 + 4 + 2);
        let from_rc = PaneStepCarry::from_replay_carry(&replay_carry, &cp, &cfg, None, origin);
        assert_eq!(from_rc, carry);

        let bytes = carry.canonical();
        let decoded = PaneStepCarry::from_canonical(&bytes).expect("canonical kitty carry");
        assert_eq!(decoded, carry);
        assert_eq!(decoded.canonical(), bytes);

        // Also exercise the Orphan variant arm roundtrip.
        let mut with_orphan = carry.clone();
        with_orphan.kitty_store[1].variants.push(KittyVariantCarry {
            source: KittyVariantSourceCarry::Orphan {
                bytes: vec![7, 8, 9, 255],
                format: ImageFormat::RawRgba8 {
                    width: 1,
                    height: 1,
                },
            },
            layout: KittyLayoutCarry {
                cols: 2,
                rows: 1,
                z_index: 0,
                cell_offset_px: (0, 0),
                scaling: ImageScaling::Stretch,
                source_rect: None,
            },
        });
        let orphan_bytes = with_orphan.canonical();
        let orphan_decoded =
            PaneStepCarry::from_canonical(&orphan_bytes).expect("orphan variant decodes");
        assert_eq!(orphan_decoded, with_orphan);
        assert_eq!(orphan_decoded.canonical(), orphan_bytes);

        let mut pane_replay = Terminal::replay_from_checkpoint(&cp);
        pane_replay.set_cell_pixel_size(10, 20);
        pane_replay.apply_pane_step_carry(&decoded, origin);

        let mut carry_replay = Terminal::replay_from_checkpoint(&cp);
        carry_replay.set_cell_pixel_size(10, 20);
        carry_replay.apply_replay_carry(&replay_carry);

        let tail = b"\x1b_Gm=0;8A\x1b\\\
                     \x1b[3;1H\x1b_Ga=p,i=12,c=1,r=1\x1b\\\
                     \x1b[4;1H\x1b_Ga=p,I=42,c=2,r=1\x1b\\\
                     \x1b_Ga=a,i=1,c=1\x1b\\\
                     \x1b_Ga=d,d=I,i=1\x1b\\";
        live.process(tail);
        pane_replay.process(tail);
        carry_replay.process(tail);

        let live_resp = live.take_response();
        assert_eq!(pane_replay.take_response(), live_resp);
        assert_eq!(carry_replay.take_response(), live_resp);
        assert_eq!(pane_replay.checkpoint(), live.checkpoint());
        assert_eq!(carry_replay.checkpoint(), live.checkpoint());
        assert_eq!(
            pane_replay.transient.kitty_total_bytes,
            live.transient.kitty_total_bytes
        );
        assert_eq!(
            carry_replay.transient.kitty_total_bytes,
            live.transient.kitty_total_bytes
        );
        assert_eq!(
            pane_replay.transient.live_image_charge,
            live.transient.live_image_charge
        );
        assert_eq!(
            carry_replay.transient.live_image_charge,
            live.transient.live_image_charge
        );

        // Negative controls on canonical validation.
        let mut zero_id = carry.clone();
        zero_id.kitty_store[0].id = 0;
        assert_eq!(
            PaneStepCarry::from_canonical(&zero_id.canonical())
                .expect_err("zero kitty id")
                .field,
            "pane_carry.kitty_store.id"
        );

        let mut unsorted = carry.clone();
        unsorted.kitty_store.swap(0, 1);
        assert_eq!(
            PaneStepCarry::from_canonical(&unsorted.canonical())
                .expect_err("unsorted kitty ids")
                .field,
            "pane_carry.kitty_store.id"
        );

        let mut bad_shown = carry.clone();
        bad_shown.kitty_store[0].shown_frame = 2;
        assert_eq!(
            PaneStepCarry::from_canonical(&bad_shown.canonical())
                .expect_err("shown_frame >= frames.len()")
                .field,
            "pane_carry.kitty_store.shown_frame"
        );

        let mut bad_cols = carry.clone();
        bad_cols.kitty_store[0].frames[0].layout.cols = 0;
        assert_eq!(
            PaneStepCarry::from_canonical(&bad_cols.canonical())
                .expect_err("zero frame layout cols")
                .field,
            "pane_carry.kitty_layout.cols"
        );

        let mut bad_rgba = carry.clone();
        bad_rgba.kitty_store[0].frames[0].bytes = vec![0u8; 11];
        assert_eq!(
            PaneStepCarry::from_canonical(&bad_rgba.canonical())
                .expect_err("RGBA byte length mismatch")
                .field,
            "pane_carry.kitty_raster.bytes"
        );

        let mut dup_variant = carry.clone();
        let v0 = dup_variant.kitty_store[0].variants[0].clone();
        dup_variant.kitty_store[0].variants.push(v0);
        assert_eq!(
            PaneStepCarry::from_canonical(&dup_variant.canonical())
                .expect_err("duplicate variant layout")
                .field,
            "pane_carry.kitty_store.variants"
        );

        let mut orphan_num = carry.clone();
        orphan_num.kitty_numbers = vec![(42, 999)];
        assert_eq!(
            PaneStepCarry::from_canonical(&orphan_num.canonical())
                .expect_err("kitty_numbers id missing from store")
                .field,
            "pane_carry.kitty_numbers"
        );

        let mut settled_pending = carry;
        settled_pending.kitty_pending.as_mut().unwrap().more = false;
        assert_eq!(
            PaneStepCarry::from_canonical(&settled_pending.canonical())
                .expect_err("kitty_pending with more == false")
                .field,
            "pane_carry.kitty_pending"
        );
    }

    #[test]
    fn pane_step_carry_placed_images_floating_relations_and_history_roundtrip_and_reject_noncanonical()
     {
        let origin = aterm_time::Instant::now();
        let cfg = crate::config::TerminalConfig::default();
        let mut live = Terminal::new(6, 30);
        live.set_cell_pixel_size(10, 20);

        // 1. Place an iTerm2 OSC 1337 2-row loose image at the top and scroll its top row into
        // primary history (`history_rev`), keeping its second row on screen (`visible`) sharing
        // the same LooseImageCarry slot.
        live.process(
            b"\x1b]1337;File=inline=1;width=3;height=2;preserveAspectRatio=0:QUJDREVGRw==\x1b\\",
        );
        live.process(b"\x1b[6;1H\r\n");

        // 2. Unnamed (`i=0`) Kitty `a=T` loose placement on the primary screen.
        live.process(b"\x1b[1;10H\x1b_Ga=T,f=24,s=1,v=1,c=2,r=1;/wAA\x1b\\");

        // 3. Stored 2-frame Kitty image `i=1` with:
        //    - cell parent `p=10` + cell-relative child `p=11` (`kitty_relations`),
        //    - virtual parent `p=2` + placeholder + floating child `p=21` (`kitty_floating`).
        live.process(b"\x1b_Ga=t,f=32,s=2,v=1,i=1;////AAAA//8=\x1b\\");
        live.process(b"\x1b_Ga=f,f=32,s=2,v=1,i=1,z=40;AAAA//////8=\x1b\\");
        live.process(b"\x1b[3;2H\x1b_Ga=p,i=1,p=10,c=2,r=1\x1b\\");
        live.process(b"\x1b_Ga=p,i=1,p=11,P=1,Q=10,H=3,V=0,c=1,r=1\x1b\\");
        live.process(b"\x1b_Ga=p,U=1,i=1,p=2,c=2,r=1\x1b\\");
        live.process(
            "\x1b[4;2H\x1b[38;5;1;58;5;2m\u{10EEEE}\u{0305}\u{0305}\x1b[39;59m".as_bytes(),
        );
        live.process(b"\x1b_Ga=p,i=1,p=21,P=1,Q=2,H=1,V=0,c=1,r=1\x1b\\");

        // 4. Enter alternate screen and place another loose OSC 1337 image so both `active_images`
        // (alt) and `inactive_images` (primary visible + history_rev) carry placements.
        live.process(b"\x1b[?1049h\x1b[2;2H\x1b]1337;File=inline=1;width=2;height=1:WFla\x1b\\");
        let _ = live.take_response();

        let cp = live.checkpoint_pane_step(2048).expect("at ground");
        let carry = PaneStepCarry::capture(&live, origin, 2048);
        assert_eq!(
            carry.loose_images.len(),
            3,
            "alt OSC1337 + primary OSC1337 + primary i=0 a=T"
        );
        assert_eq!(carry.kitty_floating.len(), 1);
        assert_eq!(carry.kitty_relations.len(), 1);
        assert!(!carry.active_images.visible.is_empty());
        let inactive = carry.inactive_images.as_ref().expect("alt up -> inactive");
        assert!(!inactive.visible.is_empty());
        assert!(!inactive.history_rev.is_empty());

        let replay_carry = live.capture_replay_carry();
        assert!(replay_carry.retained_image_bytes() > 0);
        let from_rc = PaneStepCarry::from_replay_carry(&replay_carry, &cp, &cfg, None, origin);
        assert_eq!(from_rc, carry);

        let bytes = carry.canonical();
        let decoded = PaneStepCarry::from_canonical(&bytes).expect("canonical placed carry");
        assert_eq!(decoded, carry);
        assert_eq!(decoded.canonical(), bytes);

        let mut pane_replay = Terminal::replay_from_checkpoint(&cp);
        pane_replay.set_cell_pixel_size(10, 20);
        pane_replay.apply_pane_step_carry(&decoded, origin);

        let mut carry_replay = Terminal::replay_from_checkpoint(&cp);
        carry_replay.set_cell_pixel_size(10, 20);
        carry_replay.apply_replay_carry(&replay_carry);

        // Leave alt screen, move parent p=10 (cascades to relative child p=11), switch frame 2 on
        // i=1, and delete parent p=10 (`d=P` cascades to p=11).
        let base_clock = super::super::ClockReading::now();
        let c1 = super::super::ClockReading {
            monotonic: origin + core::time::Duration::from_millis(50),
            ..base_clock
        };
        let tail = b"\x1b[?1049l\
                     \x1b[3;6H\x1b_Ga=p,i=1,p=10,c=2,r=1\x1b\\\
                     \x1b_Ga=a,i=1,c=2\x1b\\";
        live.process_at(tail, c1);
        pane_replay.process_at(tail, c1);
        carry_replay.process_at(tail, c1);

        let live_resp = live.take_response();
        assert_eq!(pane_replay.take_response(), live_resp);
        assert_eq!(carry_replay.take_response(), live_resp);
        assert_eq!(pane_replay.checkpoint(), live.checkpoint());
        assert_eq!(carry_replay.checkpoint(), live.checkpoint());
        assert_eq!(
            PaneStepCarry::capture(&pane_replay, origin, 2048),
            PaneStepCarry::capture(&live, origin, 2048),
        );
        assert_eq!(
            PaneStepCarry::capture(&carry_replay, origin, 2048),
            PaneStepCarry::capture(&live, origin, 2048),
        );

        // Cascading delete of parent p=10 removes both p=10 and relative child p=11 identically.
        let c2 = super::super::ClockReading {
            monotonic: origin + core::time::Duration::from_millis(100),
            ..base_clock
        };
        let del = b"\x1b_Ga=d,d=P,i=1,p=10\x1b\\";
        live.process_at(del, c2);
        pane_replay.process_at(del, c2);
        carry_replay.process_at(del, c2);
        assert_eq!(
            PaneStepCarry::capture(&pane_replay, origin, 2048),
            PaneStepCarry::capture(&live, origin, 2048),
        );
        assert_eq!(
            PaneStepCarry::capture(&carry_replay, origin, 2048),
            PaneStepCarry::capture(&live, origin, 2048),
        );

        // Negative controls on v5 canonical validation.
        let mut unref_loose = carry.clone();
        let extra_loose = unref_loose.loose_images[0].clone();
        unref_loose.loose_images.push(extra_loose);
        assert_eq!(
            PaneStepCarry::from_canonical(&unref_loose.canonical())
                .expect_err("unreferenced loose image")
                .field,
            "pane_carry.loose_images"
        );

        let mut swapped_loose = carry.clone();
        swapped_loose.active_images.visible[0].1[0].image = PlacedImageSourceCarry::Loose(1);
        assert_eq!(
            PaneStepCarry::from_canonical(&swapped_loose.canonical())
                .expect_err("Loose(1) referenced before Loose(0)")
                .field,
            "pane_carry.placed_grid.span.image"
        );

        let mut bad_store_ref = carry.clone();
        bad_store_ref.active_images.visible[0].1[0].image = PlacedImageSourceCarry::KittyFrame {
            image_id: 999,
            frame: 0,
        };
        assert_eq!(
            PaneStepCarry::from_canonical(&bad_store_ref.canonical())
                .expect_err("unstored KittyFrame image_id")
                .field,
            "pane_carry.placed_grid.span.image"
        );

        let mut dup_float = carry.clone();
        let fl0 = dup_float.kitty_floating[0];
        dup_float.kitty_floating.push(fl0);
        assert_eq!(
            PaneStepCarry::from_canonical(&dup_float.canonical())
                .expect_err("duplicate floating serial")
                .field,
            "pane_carry.kitty_floating.tag"
        );

        let mut dup_rel = carry.clone();
        let rel0 = dup_rel.kitty_relations[0];
        dup_rel.kitty_relations.push(rel0);
        assert_eq!(
            PaneStepCarry::from_canonical(&dup_rel.canonical())
                .expect_err("duplicate relation serial")
                .field,
            "pane_carry.kitty_relations.child"
        );

        let mut bad_span = carry.clone();
        bad_span.active_images.visible[0].1[0].image_row = 99;
        assert_eq!(
            PaneStepCarry::from_canonical(&bad_span.canonical())
                .expect_err("span image_row >= layout.rows")
                .field,
            "pane_carry.placed_grid.span"
        );

        let mut bad_inactive = carry;
        bad_inactive.inactive_images = None;
        assert_eq!(
            PaneStepCarry::from_canonical(&bad_inactive.canonical())
                .expect_err("inactive_images None while inactive_handback Some")
                .field,
            "pane_carry.inactive_images"
        );
    }
}
