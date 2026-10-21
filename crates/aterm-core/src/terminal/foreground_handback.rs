// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Foreground handback: restore program-negotiable terminal modes when an
//! owning foreground process group exits without cleaning up.
//!
//! At the byte offset where `tcgetpgrp` changes, the host calls
//! [`Terminal::foreground_handback`] if a dead process group owned any
//! input-hijacking mode bit ([`evidence::INPUT`]: mouse, kitty keyboard,
//! modifyOtherKeys/formatOtherKeys, focus 1004, bracketed paste 2004, size
//! 2048, color-scheme 2031). Display-only bits (`ALT_SCREEN`, `VT52`,
//! `CURSOR_HIDDEN`, `SYNC`) do not trigger handback on their own so one-shot
//! commands (`tput smcup`, `tput civis`) persist, but are reverted alongside
//! an orphaned input bit.
//!
//! Modes are restored by feeding synthesized reset bytes through
//! [`Terminal::process_at`] and returning them so the host can record them in
//! `RawIn` and cast taps. A mid-sequence parser always receives `CAN` (`0x18`),
//! even when mode restoration is skipped ([`Terminal::foreground_handback_scoped`]).

use core::fmt::Write as _;

use aterm_types::charset::{CharacterSet, GlMapping};
use aterm_types::mouse::{MouseEncoding, MouseMode};

use super::{ClockReading, Terminal};

/// One bit per condition of [`Terminal::program_evidence`], the evidence the
/// handback gate reads. The host attributes each bit to the process group
/// that set it (aterm-gui's `FgOwners`).
pub mod evidence {
    /// The alternate screen (1049/1047/47).
    pub const ALT_SCREEN: u16 = 1 << 0;
    /// VT52 mode (`CSI ? 2 l`).
    pub const VT52: u16 = 1 << 1;
    /// Any mouse tracking mode.
    pub const MOUSE: u16 = 1 << 2;
    /// A hidden cursor (DECTCEM reset).
    pub const CURSOR_HIDDEN: u16 = 1 << 3;
    /// Synchronized output (2026).
    pub const SYNC: u16 = 1 << 4;
    /// In-band size reports (2048).
    pub const SIZE_REPORTS: u16 = 1 << 5;
    /// Colour-scheme reports (2031).
    pub const COLOR_SCHEME_REPORTS: u16 = 1 << 6;
    /// Focus reporting (1004) the host did not configure.
    pub const FOCUS: u16 = 1 << 7;
    /// Kitty keyboard flags, or a pushed kitty stack, on the current screen.
    pub const KITTY: u16 = 1 << 8;
    /// A non-zero modifyOtherKeys.
    pub const MODIFY_OTHER_KEYS: u16 = 1 << 9;
    /// A non-zero formatOtherKeys.
    pub const FORMAT_OTHER_KEYS: u16 = 1 << 10;
    /// Bracketed paste (2004) the host did not configure.
    pub const BRACKETED_PASTE: u16 = 1 << 11;
    /// How many bits are defined.
    pub const COUNT: usize = 12;

    /// The input-hijacking bits: each makes the terminal send bytes the next
    /// holder never asked for (mouse and focus reports, size and colour-scheme
    /// reports), re-encode its keys (kitty flags, modifyOtherKeys,
    /// formatOtherKeys) or frame its pastes (2004).
    ///
    /// The other four bits ([`ALT_SCREEN`], [`VT52`], [`CURSOR_HIDDEN`],
    /// [`SYNC`]) are display evidence that one-shot commands set on purpose
    /// (`tput smcup; cmd; tput rmcup`, `tput civis`). The host counts a gone
    /// group as orphaning the terminal only when it owns one of these input
    /// bits; such a group's display bits are then handed back with them.
    pub const INPUT: u16 = MOUSE
        | SIZE_REPORTS
        | COLOR_SCHEME_REPORTS
        | FOCUS
        | KITTY
        | MODIFY_OTHER_KEYS
        | FORMAT_OTHER_KEYS
        | BRACKETED_PASTE;

    /// The evidence bit a DECSET (`set`) or DECRST of private mode `param`
    /// asserts (turns on, whether or not already on), or `0`. Read by the DEC
    /// mode dispatcher into
    /// [`Terminal::take_evidence_asserted`](super::Terminal::take_evidence_asserted).
    #[must_use]
    pub const fn asserted_by_dec_mode(param: u16, set: bool) -> u16 {
        match (param, set) {
            (47 | 1047 | 1049, true) => ALT_SCREEN,
            (2, false) => VT52,
            (9 | 1000 | 1002 | 1003, true) => MOUSE,
            (25, false) => CURSOR_HIDDEN,
            (2026, true) => SYNC,
            (2048, true) => SIZE_REPORTS,
            (2031, true) => COLOR_SCHEME_REPORTS,
            (1004, true) => FOCUS,
            (2004, true) => BRACKETED_PASTE,
            _ => 0,
        }
    }
}

/// What one [`Terminal::foreground_handback`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundHandback {
    /// The exact bytes fed through [`Terminal::process_at`], in order. The host
    /// records them as `RawIn` and splices them into its byte taps at the cut,
    /// so a replay of the recording reaches the same state.
    pub bytes: Vec<u8>,
    /// Short names of what was moved (`"alt"`, `"kitty"`, `"mouse"`,
    /// `"parser"`, …), in emission order: the `reverted=` field of the
    /// `modes-restored` timeline event.
    pub reverted: Vec<&'static str>,
}

impl Terminal {
    /// Whether the terminal is in a state only a running program leaves it in:
    /// the evidence gate of [`Self::foreground_handback`].
    ///
    /// True when any holds: the alternate screen, VT52 mode, mouse tracking, a
    /// hidden cursor, synchronized output (2026), in-band size reports (2048),
    /// colour-scheme reports (2031), focus reporting (1004) or bracketed paste
    /// (2004) the host did not configure, kitty keyboard flags or a pushed
    /// kitty stack on the current screen, or a non-zero modifyOtherKeys /
    /// formatOtherKeys. A line editor's own 2004 at its prompt counts too; the
    /// host attributes it to the shell, which is alive.
    ///
    /// Not evidence: DECAWM, DECCKM, DECKPAM, G0/GL charsets, IRM, LNM, DECSCNM,
    /// 1007, 67, 1036 and a mouse encoding without tracking; one-shot commands
    /// (`tput rmam`, `smkx`, `smacs`, `smir`) set these and expect them to
    /// persist.
    #[must_use]
    pub fn program_owns_terminal(&self) -> bool {
        self.program_evidence() != 0
    }

    /// [`Self::program_owns_terminal`]'s conditions as a bit set (see
    /// [`evidence`]); zero iff the gate is closed. The host compares successive
    /// readings to learn which process group set which bit.
    #[must_use]
    pub fn program_evidence(&self) -> u16 {
        let m = &self.modes;
        let k = self.kitty_keyboard.snapshot();
        let depth = if m.alternate_screen {
            k.alt_sp
        } else {
            k.main_sp
        };
        // Flags pushed on the main screen before `?1049h` are parked in
        // `main_saved_flags` while alt is up and return on leaving it. They are
        // still the program's, so they count as evidence; otherwise a shell
        // leaving the alt screen by itself (`echoti rmcup`) would get CSI-u
        // chords at its prompt.
        let main_parked = m.alternate_screen
            && (k.main_saved_flags.is_some_and(|f| f.bits() != 0) || k.main_sp > 0);
        // Behind a console host the xterm modifier family is not offered, as kitty is not.
        let xk = super::keyboard_mode::offered_xterm_keyboard(m, self.xterm_keyboard);
        let bit = |on: bool, b: u16| if on { b } else { 0 };
        bit(m.alternate_screen, evidence::ALT_SCREEN)
            | bit(m.vt52_mode, evidence::VT52)
            | bit(m.mouse_mode != MouseMode::None, evidence::MOUSE)
            | bit(!m.cursor_visible, evidence::CURSOR_HIDDEN)
            | bit(m.synchronized_output, evidence::SYNC)
            | bit(m.in_band_size_reports, evidence::SIZE_REPORTS)
            | bit(m.report_color_scheme, evidence::COLOR_SCHEME_REPORTS)
            | bit(
                m.focus_reporting && !self.configured_modes.focus_reporting,
                evidence::FOCUS,
            )
            | bit(
                m.bracketed_paste && !self.configured_modes.bracketed_paste,
                evidence::BRACKETED_PASTE,
            )
            | bit(
                m.kitty_keyboard_enabled && (k.flags.bits() != 0 || depth > 0 || main_parked),
                evidence::KITTY,
            )
            | bit(
                xk.modify_other_keys().unwrap_or(0) != 0,
                evidence::MODIFY_OTHER_KEYS,
            )
            | bit(xk.format_other_keys() != 0, evidence::FORMAT_OTHER_KEYS)
    }

    /// The evidence bits whose setter was parsed since the last call, clearing
    /// them: a DECSET of mouse tracking, 1004, 2004, 2048, 2031, the alt screen
    /// or 2026, a DECRST of 25 or 2 (VT52), a kitty push or non-zero set, a
    /// non-zero modifyOtherKeys or formatOtherKeys. Asserted even when the
    /// condition was already on, which [`Self::program_evidence`] cannot show.
    ///
    /// The host gives each evidence bit to the group whose bytes set it; with
    /// asserted bits a job that re-arms an already-on mode becomes its owner,
    /// so its death hands the mode back even if an earlier owner was
    /// misattributed.
    pub fn take_evidence_asserted(&mut self) -> u16 {
        core::mem::take(&mut self.evidence_asserted)
    }

    /// Hand the terminal back after a foreground-process-group change:
    /// [`Self::foreground_handback_scoped`] with the modes in scope. The host
    /// calls it only where the modes are orphaned (see the module docs).
    pub fn foreground_handback(&mut self) -> Option<ForegroundHandback> {
        self.foreground_handback_scoped(true)
    }

    /// Hand the terminal back after a foreground-process-group change.
    ///
    /// Call it exactly at the byte offset where the foreground changed (see the
    /// module docs). Returns `None` when nothing was sent: the parser was at
    /// ground and either `restore_modes` was false or
    /// [`Self::program_owns_terminal`] was. Otherwise the returned bytes have
    /// already been processed; the caller only records them.
    ///
    /// `restore_modes = false` sends step 0 alone: the old holder is gone but
    /// the modes in force belong to a program that still runs.
    ///
    /// The byte plan, each step emitted only when the live value differs from
    /// its target ([`TerminalModes::new`](aterm_types::TerminalModes::new), the
    /// host-configured DECAWM/1004/2004/1007, and `XtermKeyboardState::new`):
    ///
    /// 0. `CAN` if the parser is mid-sequence, even with the gate closed,
    ///    then `ESC \` if a tmux passthrough wrapper is open.
    /// 1. `ESC <` to leave VT52 (before any CSI, which VT52 would not parse).
    /// 2. `CSI ? 2026 l`.
    /// 3. On the alt screen: `CSI < 8 u` (clears the alt kitty stack; depth is
    ///    at most 7, and a pop deeper than the stack sets flags 0), then
    ///    `CSI ? 1049 l`, which reloads main's saved kitty flags.
    /// 4. `CSI < 8 u` for the main screen's flags/stack.
    /// 5. `CSI ? 1000 l` (clears any tracking), then the DECRST of the active
    ///    mouse encoding (each clears only its own).
    /// 6. 1004 to the configured value; `?2048l`, `?2031l`; 1007 to the host's
    ///    value ([`Self::set_host_alternate_scroll`]).
    /// 7. `CSI > 4 m`, `CSI > 4 f` (modifyOtherKeys / formatOtherKeys).
    /// 8. `?1l`, `ESC >`, `?67l`, `?1035h`, `?1036l`, `?1039h`.
    /// 9. 2004 to the configured value.
    /// 10. `4l`, `20l`, `?5l`, DECAWM to the configured value.
    /// 11. `ESC ( B`, `SI`.
    /// 12. `CSI ? 25 h`.
    ///
    /// Phase B (gate open only): a scroll region, DECOM or DECLRMM that
    /// survived phase A (`1049l` copies the alt screen's DECSTBM onto main) is
    /// cleared, and since DECSTBM/DECOM home the cursor, the cursor position
    /// read before is written back with a CUP.
    pub fn foreground_handback_scoped(
        &mut self,
        restore_modes: bool,
    ) -> Option<ForegroundHandback> {
        let owned = restore_modes && self.program_owns_terminal();
        self.handback(owned, false)
    }

    /// Manual terminal reset (`reset` control verb, Edit ▸ Reset Terminal).
    ///
    /// Runs the [`Self::foreground_handback_scoped`] byte plan unconditionally,
    /// closes any open OSC 8 hyperlink (`"hyperlink"`), and resets program-overridden
    /// palette/dynamic colors via `OSC 104/110/111/112/117/119` (`"colors"`),
    /// while preserving the screen, scrollback, SGR pen, titles, and tab stops.
    pub fn manual_handback(&mut self) -> ForegroundHandback {
        self.handback(true, true).unwrap_or(ForegroundHandback {
            bytes: Vec::new(),
            reverted: Vec::new(),
        })
    }

    /// The shared body of [`Self::foreground_handback_scoped`] (`owned` = the
    /// gate's verdict, `manual` false) and [`Self::manual_handback`] (both
    /// true). `None` when there was nothing to send.
    fn handback(&mut self, owned: bool, manual: bool) -> Option<ForegroundHandback> {
        let clock = ClockReading::now();
        let mut bytes: Vec<u8> = Vec::new();
        let mut reverted: Vec<&'static str> = Vec::new();
        if !self.parser_is_ground() {
            // The CAN ends the torn sequence, inside an open tmux passthrough
            // wrapper too (where it is payload, and a pending ESC is the
            // payload's), so the `ESC \` after it ends only the wrapper: the
            // wrapper's end would end a torn string as at its ST, and the next
            // holder's own ESCs would otherwise still be read through it.
            bytes.push(0x18);
            if self.parser.tmux_passthrough().is_open() {
                bytes.extend_from_slice(b"\x1b\\");
            }
            reverted.push("parser");
        }
        if owned {
            self.handback_phase_a(&mut bytes, &mut reverted);
        }
        // An open OSC 8 link turns every later character into a link. The
        // automatic handback leaves OSC 8 alone; a manual reset closes it,
        // after phase A so VT52 has been left and the OSC parses.
        if manual && self.transient.current_hyperlink.is_some() {
            bytes.extend_from_slice(b"\x1b]8;;\x1b\\");
            reverted.push("hyperlink");
        }
        if manual {
            self.manual_colour_reset(&mut bytes, &mut reverted);
        }
        if bytes.is_empty() {
            return None;
        }
        self.process_at(&bytes, clock);
        if owned {
            let rows = self.grid.rows();
            let region = self.grid.scroll_region();
            let full = region.top == 0 && region.bottom + 1 == rows;
            let origin = self.modes.origin_mode;
            let lr = self.modes.left_right_margin_mode;
            if origin || lr || !full {
                let (row, col) = (self.grid.cursor_row(), self.grid.cursor_col());
                let mut b = String::new();
                if origin {
                    b.push_str("\x1b[?6l");
                    reverted.push("origin");
                }
                if lr {
                    b.push_str("\x1b[?69l");
                    reverted.push("margins");
                }
                if !full {
                    b.push_str("\x1b[r");
                    reverted.push("region");
                }
                let _ = write!(b, "\x1b[{};{}H", u32::from(row) + 1, u32::from(col) + 1);
                self.process_at(b.as_bytes(), clock);
                bytes.extend_from_slice(b.as_bytes());
            }
        }
        Some(ForegroundHandback { bytes, reverted })
    }

    /// The manual reset's colour step: exactly the OSC resets that clear what a
    /// program set over the configured colours ([`Self::program_color_overrides`]),
    /// each ST-terminated, and `"colors"` when there was any. Every reset is
    /// ungated and lands on the configured baseline, so afterwards the diff is
    /// empty. The automatic handback never runs this.
    fn manual_colour_reset(&self, bytes: &mut Vec<u8>, reverted: &mut Vec<&'static str>) {
        let o = self.color.capture_overrides();
        let start = bytes.len();
        for (set, seq) in [
            (!o.palette.is_empty(), &b"\x1b]104\x1b\\"[..]),
            (o.foreground.is_some(), b"\x1b]110\x1b\\"),
            (o.background.is_some(), b"\x1b]111\x1b\\"),
            (o.cursor.is_some(), b"\x1b]112\x1b\\"),
            (o.selection_background.is_some(), b"\x1b]117\x1b\\"),
            (o.selection_foreground.is_some(), b"\x1b]119\x1b\\"),
        ] {
            if set {
                bytes.extend_from_slice(seq);
            }
        }
        if bytes.len() > start {
            reverted.push("colors");
        }
    }

    /// Phase A of [`Self::foreground_handback`]: the byte plan's steps 1–12,
    /// computed from the live state (nothing is processed here).
    fn handback_phase_a(&self, bytes: &mut Vec<u8>, reverted: &mut Vec<&'static str>) {
        let m = self.modes;
        let cfg = self.configured_modes;
        let k = self.kitty_keyboard.snapshot();
        let kitty = m.kitty_keyboard_enabled;
        let xk = super::keyboard_mode::offered_xterm_keyboard(&m, self.xterm_keyboard);
        let mut put = |on: bool, seq: &str, name: &'static str| {
            if on {
                bytes.extend_from_slice(seq.as_bytes());
                reverted.push(name);
            }
        };
        let set_reset = |set: bool, on: &'static str, off: &'static str| if set { on } else { off };

        put(m.vt52_mode, "\x1b<", "vt52");
        put(m.synchronized_output, "\x1b[?2026l", "sync");
        if m.alternate_screen {
            put(
                kitty && (k.flags.bits() != 0 || k.alt_sp > 0),
                "\x1b[<8u",
                "kitty-alt",
            );
            put(true, "\x1b[?1049l", "alt");
        }
        // After `1049l` the live flags are main's saved flags.
        let (main_flags, main_sp) = if m.alternate_screen {
            (
                k.main_saved_flags
                    .map_or(0, aterm_types::KittyKeyboardFlags::bits),
                k.main_sp,
            )
        } else {
            (k.flags.bits(), k.main_sp)
        };
        put(
            kitty && (main_flags != 0 || main_sp > 0),
            "\x1b[<8u",
            "kitty",
        );
        put(m.mouse_mode != MouseMode::None, "\x1b[?1000l", "mouse");
        let encoding = match m.mouse_encoding {
            MouseEncoding::Utf8 => Some("\x1b[?1005l"),
            MouseEncoding::Sgr => Some("\x1b[?1006l"),
            MouseEncoding::Urxvt => Some("\x1b[?1015l"),
            MouseEncoding::SgrPixel => Some("\x1b[?1016l"),
            // X10 is the power-on encoding; the enum is non-exhaustive.
            _ => None,
        };
        if let Some(seq) = encoding {
            put(true, seq, "mouse-encoding");
        }
        put(
            m.focus_reporting != cfg.focus_reporting,
            set_reset(cfg.focus_reporting, "\x1b[?1004h", "\x1b[?1004l"),
            "focus",
        );
        put(m.in_band_size_reports, "\x1b[?2048l", "size-reports");
        put(m.report_color_scheme, "\x1b[?2031l", "color-scheme-reports");
        // 1007 goes back to the host's value (aterm-gui: on), not power-on.
        put(
            m.alternate_scroll != self.host_alternate_scroll,
            set_reset(self.host_alternate_scroll, "\x1b[?1007h", "\x1b[?1007l"),
            "alternate-scroll",
        );
        put(xk.modify_other_keys() != Some(0), "\x1b[>4m", "mok");
        put(xk.format_other_keys() != 0, "\x1b[>4f", "fok");
        put(m.application_cursor_keys, "\x1b[?1l", "cursor-keys");
        put(m.application_keypad, "\x1b>", "keypad");
        put(m.backarrow_sends_bs, "\x1b[?67l", "backarrow");
        put(!m.special_modifiers, "\x1b[?1035h", "num-lock");
        put(m.meta_send_escape, "\x1b[?1036l", "meta-sends-escape");
        put(!m.alt_send_escape, "\x1b[?1039h", "alt-sends-escape");
        put(
            m.bracketed_paste != cfg.bracketed_paste,
            set_reset(cfg.bracketed_paste, "\x1b[?2004h", "\x1b[?2004l"),
            "paste",
        );
        put(m.insert_mode, "\x1b[4l", "insert");
        put(m.new_line_mode, "\x1b[20l", "newline");
        put(m.reverse_video, "\x1b[?5l", "reverse-video");
        put(
            m.auto_wrap != cfg.auto_wrap,
            set_reset(cfg.auto_wrap, "\x1b[?7h", "\x1b[?7l"),
            "wrap",
        );
        put(self.charset.g0 != CharacterSet::Ascii, "\x1b(B", "g0");
        put(self.charset.gl != GlMapping::G0, "\x0f", "gl");
        put(!m.cursor_visible, "\x1b[?25h", "cursor");
    }
}

#[cfg(test)]
#[path = "foreground_handback_tests.rs"]
mod tests;
