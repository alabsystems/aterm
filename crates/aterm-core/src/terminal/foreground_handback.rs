// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Foreground handback: return the terminal to its host after the program
//! that owned it lost the foreground without restoring what it negotiated.
//!
//! # Why
//!
//! A killed program restores nothing. A Claude Code process killed while it
//! held the alt screen, kitty flags, modifyOtherKeys 2, SGR any-motion mouse,
//! 1004, 2004, 2026 and a hidden cursor left the reclaiming shell typing
//! `ESC[<32;11;6M` on every mouse move and ringing the bell at every
//! `ESC[55;5u` chord, behind an invisible cursor.
//!
//! # The mechanism
//!
//! The host (aterm-gui's PTY gather) samples `tcgetpgrp` on the master and
//! cuts each read batch at the offset where the foreground process group
//! changed. At exactly that offset — after every byte the old holder wrote,
//! before any byte the new holder writes — the parse thread calls
//! [`Terminal::foreground_handback`]. Because the cut is ordered IN the byte
//! stream, the new holder's own mode setup (zsh's `ESC[?2004h`, fish's kitty
//! push) lands AFTER the handback and survives it; a UI-side sweep would race
//! it and undo the shell's own modes.
//!
//! # The gate
//!
//! A foreground change is a normal event (every command a shell runs starts and
//! ends with one), so the handback acts only on EVIDENCE that the old holder
//! left the terminal the way only a running program leaves it —
//! [`Terminal::program_owns_terminal`]. A clean `vim`/`less`/`htop` exit, a
//! `tput rmam`, `smkx`, `smacs` or `stty` leaves no evidence and is untouched;
//! `tput smcup` and `tput civis` leave only display evidence and are
//! untouched too (below).
//!
//! Evidence is necessary, not sufficient: the modes must also be ORPHANED —
//! armed by a program that no longer exists. That half is the host's, because
//! only the host knows which process group wrote which bytes and whether it
//! is still alive. aterm-gui attributes each bit of
//! [`Terminal::program_evidence`] to the foreground group whose bytes set it,
//! and restores only when one of those groups' leaders is gone; a stopped job
//! (Ctrl-Z), or a live `gdb -tui` that hands the terminal to its inferior, keeps
//! its modes (stripping them would leave the job running without them after
//! `fg`). [`Terminal::foreground_handback_scoped`] is the entry point that lets
//! the host send the torn-sequence `CAN` alone.
//!
//! And the gone group must own an INPUT-HIJACKING bit ([`evidence::INPUT`]:
//! mouse, kitty, modifyOtherKeys/formatOtherKeys, 1004, 2004, 2048, 2031). The
//! display bits alone — alt screen, VT52, hidden cursor, 2026 — are what a
//! one-shot command sets on purpose and then exits cleanly: `tput smcup; cmd;
//! tput rmcup` and `tput civis` are kept (reverting them the moment `tput`
//! exits would leave the wrapped `cmd` drawing on the main screen). RESIDUAL: a
//! one-shot that arms an input bit on purpose (`/usr/bin/printf '\033[?1000h'`
//! or `'\033[?2004h'`) is handed back when it exits — nothing tells a clean
//! one-shot exit from a SIGKILL, and a mouse left reporting under the prompt is
//! the failure above; and a program killed with
//! only display modes armed (a SIGKILLed `less`) leaves them, as before the
//! handback.
//!
//! 2004 is an input bit because it changes what a paste sends, and the host's
//! multi-line paste guard reads it as the reader's promise to wait for Enter.
//! PG1 (docs/AUDIT-side-effects-2026-10-07.md): a `cat` of a file that planted
//! the clipboard with OSC 52 and armed 2004 under a reader that does not parse
//! the brackets (`/bin/bash` 3.2, dash) turned the guard off for every later
//! paste, and the reader ran each pasted line. zsh and bash 5.1+ arm it again
//! at their next prompt, after the cut. RESIDUAL: a `?2004h` parsed while a
//! long-lived holder or the shell itself holds the terminal (anything inside
//! ssh, a background job's output, a `/bin/bash` 3.2 prompt printing a
//! directory name raw, a builtin `printf`) is not handed back while that holder
//! runs (ssh's only at its exit, the shell's never), and the paste guard trusts
//! it.
//!
//! RESIDUAL, the late reader: the cut is placed where the host SAW the group
//! change, so a starved reader that reads a holder's last bytes only after the
//! shell took the terminal back parses them as the SHELL's. Late ARMING bytes
//! (a one-shot's `?1000h`) make the shell their owner, and the one-shot loses
//! its handback. Late RESET bytes are the mirror case: a clean exit whose own
//! reset (a bare `vim -u NONE -c q` turning modifyOtherKeys off) lands after
//! the cut is handed back there, so the modes end where the reset puts them
//! anyway, but the timeline records a `modes-restored` the exit did not earn
//! (3 of 160 runs at load 103-112, 2026-09-30).
//!
//! # The restore
//!
//! When the gate is open, every program-negotiable mode is returned to its
//! power-on or host-configured value by feeding SYNTHESIZED BYTES through the
//! real [`Terminal::process_at`]. The bytes are returned so the host can record
//! them on the temporal spine (`RawIn`) and splice them into the cast/`bytes`
//! taps at the cut: replaying the recording reproduces the live state exactly.
//! A torn escape sequence (the program died mid-write) always gets a `CAN`,
//! gate open or not, so the next holder's first byte is not swallowed.
//!
//! Never touched: cursor style/blink, SGR, palette and dynamic colours, titles,
//! OSC 7/8 state (an open OSC 8 link is closed only by the manual reset,
//! [`Terminal::manual_handback`]), tab stops, grid/scrollback/images, XTSAVE slots, bidi, modes
//! 3/45/2027, the INACTIVE screen's kitty slot, and every host policy bit.

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
                self.xterm_keyboard.modify_other_keys().unwrap_or(0) != 0,
                evidence::MODIFY_OTHER_KEYS,
            )
            | bit(
                self.xterm_keyboard.format_other_keys() != 0,
                evidence::FORMAT_OTHER_KEYS,
            )
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
    /// 0. `CAN` if the parser is mid-sequence, even with the gate closed.
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

    /// The manual reset: the same handback, requested by a person or operator
    /// (the `reset` control verb, Edit ▸ Reset Terminal) rather than a
    /// foreground change.
    ///
    /// It covers what the automatic gate cannot: modes armed before the
    /// current holder (a live shell is never gone), a shifted charset left on,
    /// an OSC 8 hyperlink a killed program left open, display modes a killed
    /// `less` left, and foreground changes the sampler missed. Unlike DECSTR it
    /// keeps the SGR pen and saved cursor and reports exactly what moved.
    ///
    /// It runs the byte plan of [`Self::foreground_handback_scoped`] without
    /// the evidence gate (step 0's `CAN` if mid-sequence, steps 1-12 where the
    /// live value differs from the host target, phase B), plus `OSC 8 ; ; ST`
    /// when a hyperlink is open (`"hyperlink"`). The bytes go through
    /// [`Self::process_at`] so the host records them and replay matches.
    ///
    /// It never clears the screen or scrollback or touches the SGR pen,
    /// palette, titles, tab stops or images; phase B puts the cursor back. The
    /// alternate screen is left (`?1049l`). Run under a live TUI, it strips
    /// that TUI's modes too.
    ///
    /// Always returns a [`ForegroundHandback`]; its `bytes` are empty when the
    /// terminal was already at its host defaults (nothing is processed then).
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
            bytes.push(0x18);
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

    /// Phase A of [`Self::foreground_handback`]: the byte plan's steps 1–12,
    /// computed from the live state (nothing is processed here).
    fn handback_phase_a(&self, bytes: &mut Vec<u8>, reverted: &mut Vec<&'static str>) {
        let m = self.modes;
        let cfg = self.configured_modes;
        let k = self.kitty_keyboard.snapshot();
        let kitty = m.kitty_keyboard_enabled;
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
            m.alternate_scroll != cfg.alternate_scroll,
            set_reset(cfg.alternate_scroll, "\x1b[?1007h", "\x1b[?1007l"),
            "alternate-scroll",
        );
        put(
            self.xterm_keyboard.modify_other_keys() != Some(0),
            "\x1b[>4m",
            "mok",
        );
        put(
            self.xterm_keyboard.format_other_keys() != 0,
            "\x1b[>4f",
            "fok",
        );
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
