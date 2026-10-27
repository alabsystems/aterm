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
    Appearance, BiDiMode, ColorPalette, CursorStyle, KittyKeyboardFlags,
    KittyKeyboardStateSnapshot, ParagraphDirection, Rgb, TaskbarProgress, TerminalModes, VtLevel,
    XtermKeyboardState,
};

use super::checkpoint::{
    DcsQueryCarry, DcsQueryKind, GridCursorRepr, PaneStepCarry, SavedCursorPendingWrap,
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
pub const PANE_CARRY_CANON_VERSION: u32 = 3;

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
        } = self;
        let mut w = W(Vec::with_capacity(240));
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
        w.0
    }

    /// Decode a [`PaneStepCarry`] from canonical bytes ([`Self::canonical`]).
    ///
    /// # Errors
    /// Returns [`CanonFault`] naming the first non-canonical field.
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
}
