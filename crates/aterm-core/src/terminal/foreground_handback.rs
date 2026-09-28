// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Foreground handback: return the terminal to its host after the program
//! that owned it lost the foreground without restoring what it negotiated.
//!
//! # The incident (2026-09-25)
//!
//! A Claude Code process was killed while it held the terminal. It had entered
//! the alternate screen, pushed kitty keyboard flags, set modifyOtherKeys 2,
//! enabled SGR any-motion mouse tracking, focus reporting, bracketed paste,
//! synchronized output, and hidden the cursor. A killed program restores
//! nothing, so the shell that reclaimed the terminal inherited all of it: every
//! mouse movement typed `ESC[<32;11;6M` at the prompt, Ctrl-key chords arrived
//! as `ESC[55;5u` (zle rang the bell at each), the prompt was invisible behind a
//! hidden cursor on a frozen alt screen, and the tab looked crashed. The owner
//! reported "it keeps sounding the bell and producing weird characters".
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
//! its modes (the 2026-09-25 review: stripping them left the job running
//! without them after `fg`). [`Terminal::foreground_handback_scoped`] is the
//! entry point that lets the host send the torn-sequence `CAN` alone.
//!
//! And the gone group must own an INPUT-HIJACKING bit ([`evidence::INPUT`]:
//! mouse, kitty, modifyOtherKeys/formatOtherKeys, 1004, 2048, 2031). The
//! display bits alone — alt screen, VT52, hidden cursor, 2026 — are what a
//! one-shot command sets on purpose and then exits cleanly: `tput smcup; cmd;
//! tput rmcup` and `tput civis` are kept (the second 2026-09-25 review found
//! both reverted the moment `tput` exited, and the wrapped `cmd` drawing on
//! the main screen). RESIDUAL: a one-shot that arms an input bit on purpose
//! (`/usr/bin/printf '\e[?1000h'`) is handed back when it exits — nothing
//! tells a clean one-shot exit from a SIGKILL, and a mouse left reporting
//! under the prompt is the incident; and a program killed with only display
//! modes armed (a SIGKILLed `less`) leaves them, as before the handback.
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
//! OSC 7/8 state, tab stops, grid/scrollback/images, XTSAVE slots, bidi, modes
//! 3/45/2027, the INACTIVE screen's kitty slot, and every host policy bit.

use core::fmt::Write as _;

use aterm_types::charset::{CharacterSet, GlMapping};
use aterm_types::mouse::{MouseEncoding, MouseMode};

use super::{ClockReading, Terminal};

/// One bit per condition of [`Terminal::program_evidence`] — the evidence the
/// foreground handback's gate reads. The host attributes each bit to the
/// process group that set it (aterm-gui's `FgOwners`).
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
    /// How many bits are defined.
    pub const COUNT: usize = 11;

    /// The INPUT-HIJACKING bits: each makes the terminal send bytes the next
    /// holder never asked for (mouse and focus reports, in-band size and
    /// colour-scheme reports) or re-encode the keys it types (kitty flags,
    /// modifyOtherKeys, formatOtherKeys). These are the incident's harm —
    /// `ESC[<32;…M` typed at the prompt on every mouse move, and zle ringing
    /// the bell at each CSI-u chord.
    ///
    /// The other four bits — [`ALT_SCREEN`], [`VT52`], [`CURSOR_HIDDEN`] and
    /// [`SYNC`] — are DISPLAY evidence, and a one-shot command sets them on
    /// purpose: `tput smcup; cmd; tput rmcup` is a common wrapper, and `tput
    /// civis` a common script opener. The host (aterm-gui's `FgOwners`) counts
    /// a gone group as orphaning the terminal only when it owns one of THESE
    /// bits (2026-09-25 review: counting display bits alone reverted `tput
    /// civis` and `tput smcup` the moment `tput` exited, and put the wrapped
    /// command on the main screen). A group that owns an input bit and dies
    /// still takes its display bits with it: the incident is handed back
    /// whole.
    pub const INPUT: u16 = MOUSE
        | SIZE_REPORTS
        | COLOR_SCHEME_REPORTS
        | FOCUS
        | KITTY
        | MODIFY_OTHER_KEYS
        | FORMAT_OTHER_KEYS;

    /// The evidence bit a DECSET (`set`) or DECRST of private mode `param`
    /// ASSERTS — the sequence that turns that bit's condition on, whether or
    /// not it was already on — or `0`. Read by the DEC mode dispatcher into
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
    /// Short names of what was moved (`"alt"`, `"kitty"`, `"mouse"`, `"parser"`,
    /// …), in emission order — the `reverted=` field of the `modes-restored`
    /// timeline event.
    pub reverted: Vec<&'static str>,
}

impl Terminal {
    /// Whether the terminal is in a state only a RUNNING program leaves it in —
    /// the evidence gate of [`Self::foreground_handback`].
    ///
    /// True when any of these holds: the alternate screen, VT52 mode, mouse
    /// tracking, a hidden cursor, synchronized output (2026), in-band size
    /// reports (2048), colour-scheme reports (2031), focus reporting (1004) the
    /// host did not configure, kitty keyboard flags or a pushed kitty stack on
    /// the current screen, or a non-zero modifyOtherKeys / formatOtherKeys.
    ///
    /// Deliberately NOT evidence: DECAWM, DECCKM, DECKPAM, G0/GL charsets, IRM,
    /// LNM, DECSCNM, 1007, 67, 1036 and a mouse ENCODING without tracking —
    /// each is something a one-shot command (`tput rmam`, `smkx`, `smacs`,
    /// `smir`) sets on purpose and expects to persist.
    #[must_use]
    pub fn program_owns_terminal(&self) -> bool {
        self.program_evidence() != 0
    }

    /// [`Self::program_owns_terminal`]'s conditions as a bit set (see
    /// [`evidence`]): which of them hold right now. Zero iff the gate is
    /// closed. The host compares successive readings to learn which process
    /// group set which bit.
    #[must_use]
    pub fn program_evidence(&self) -> u16 {
        let m = &self.modes;
        let k = self.kitty_keyboard.snapshot();
        let depth = if m.alternate_screen {
            k.alt_sp
        } else {
            k.main_sp
        };
        // Flags pushed on the MAIN screen before `?1049h` are parked in
        // `main_saved_flags` while the alt screen is up, and come back live the
        // moment it is left. They are still the program's: a program that
        // pushed, entered the alt screen and died owned no other input bit, so
        // reading only the current screen let it go unhanded — and a shell
        // that then left the alt screen with a builtin (`echoti rmcup`) got
        // CSI-u chords at its prompt, the incident's symptom (final review of
        // the handback, 2026-09-25).
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

    /// The evidence bits whose SETTER was parsed since the last call, and
    /// clears them: a DECSET of mouse tracking, 1004, 2048, 2031, the alt
    /// screen or 2026, a DECRST of 25 or of 2 (VT52), a kitty push or a kitty
    /// set to non-zero flags, a non-zero modifyOtherKeys or formatOtherKeys.
    /// A bit is asserted even when its condition was ALREADY on — that is the
    /// point: [`Self::program_evidence`] cannot tell a program that re-arms a
    /// mode from one that never touched it.
    ///
    /// Why (2026-09-27, the handback lane under load 59-65): the host gives
    /// each evidence bit to the group whose bytes set it. A one-shot
    /// `/usr/bin/printf '\e[?1000h'` whose bytes were read only after the shell
    /// had taken the terminal back gave the MOUSE bit to the shell, and every
    /// later job that armed mouse tracking found the bit already on, so it
    /// never became the bit's owner, and its death handed nothing back — the
    /// session never recovered. With the asserted bits the host gives a
    /// re-armed bit to the group that re-armed it.
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
    /// ALREADY been processed; the caller only records them.
    ///
    /// `restore_modes = false` sends step 0 alone: the program that lost the
    /// terminal is gone (its torn sequence must not swallow the next holder's
    /// first byte) but the modes in force belong to a program that still runs.
    ///
    /// The byte plan, each step emitted only when the live value differs from
    /// its target ([`TerminalModes::new`](aterm_types::TerminalModes::new), the
    /// host-configured DECAWM/1004/2004/1007, and `XtermKeyboardState::new`):
    ///
    /// 0. `CAN` if the parser is mid-sequence — even with the gate closed.
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
    /// survived phase A — `1049l` copies the alt screen's DECSTBM onto main — is
    /// cleared, and because DECSTBM/DECOM home the cursor, the cursor position
    /// read before it is written back with a CUP.
    pub fn foreground_handback_scoped(
        &mut self,
        restore_modes: bool,
    ) -> Option<ForegroundHandback> {
        let clock = ClockReading::now();
        let mut bytes: Vec<u8> = Vec::new();
        let mut reverted: Vec<&'static str> = Vec::new();
        if !self.parser_is_ground() {
            bytes.push(0x18);
            reverted.push("parser");
        }
        let owned = restore_modes && self.program_owns_terminal();
        if owned {
            self.handback_phase_a(&mut bytes, &mut reverted);
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
        // 1007 goes back to the HOST's value (aterm-gui: ON), not power-on.
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
