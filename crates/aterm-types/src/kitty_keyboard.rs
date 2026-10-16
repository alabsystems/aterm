// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kitty keyboard protocol state: the enhancement flags and the per-screen push/pop stacks.
//!
//! Reference: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>

/// Kitty keyboard protocol enhancement flags, which applications set with `CSI = flags u`
/// and push with `CSI > flags u`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KittyKeyboardFlags(u8);

impl KittyKeyboardFlags {
    /// Disambiguate escape codes (send Esc, Alt+key, Ctrl+key using CSI u).
    pub const DISAMBIGUATE: u8 = 0b0_0001;
    /// Report key repeat and release events.
    pub const REPORT_EVENTS: u8 = 0b0_0010;
    /// Report alternate key codes (shifted_key, base_layout_key).
    pub const REPORT_ALTERNATES: u8 = 0b0_0100;
    /// Report all keys as escape codes (including text-generating keys).
    pub const REPORT_ALL_KEYS: u8 = 0b0_1000;
    /// Embed associated text in escape code (requires REPORT_ALL_KEYS).
    pub const REPORT_TEXT: u8 = 0b1_0000;

    /// Create flags with no enhancements.
    #[inline]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Create flags from raw bits.
    #[inline]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0b1_1111) // Mask to valid bits
    }

    /// Get raw bits.
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Check if a specific flag is set.
    #[inline]
    #[must_use]
    pub const fn contains(self, flag: u8) -> bool {
        (self.0 & flag) != 0
    }

    /// Check if disambiguation is enabled.
    #[inline]
    #[must_use]
    pub const fn disambiguate(self) -> bool {
        self.contains(Self::DISAMBIGUATE)
    }

    /// Check if event reporting (repeat/release) is enabled.
    #[inline]
    #[must_use]
    pub const fn report_events(self) -> bool {
        self.contains(Self::REPORT_EVENTS)
    }

    /// Check if alternate key reporting is enabled.
    #[inline]
    #[must_use]
    pub const fn report_alternates(self) -> bool {
        self.contains(Self::REPORT_ALTERNATES)
    }

    /// Check if all keys should be reported as escape codes.
    #[inline]
    #[must_use]
    pub const fn report_all_keys(self) -> bool {
        self.contains(Self::REPORT_ALL_KEYS)
    }

    /// Check if text should be embedded in escape codes.
    #[inline]
    #[must_use]
    pub const fn report_text(self) -> bool {
        self.contains(Self::REPORT_TEXT)
    }

    /// Apply a mode operation to update flags.
    ///
    /// Mode values:
    /// - 1 (default): Set specified bits, clear unspecified
    /// - 2: Set specified bits, leave others unchanged (OR)
    /// - 3: Clear specified bits, leave others unchanged (AND NOT)
    pub fn apply(&mut self, bits: u8, mode: u8) {
        let bits = bits & 0b1_1111; // Mask to valid flags
        match mode {
            // Mode 0 is set: the parser delivers an empty mode (`CSI = f ; u`) as 0, and an
            // empty parameter means the default, 1. A literal `CSI = f ; 0 u` reads the same
            // (#7714) where kitty no-ops it; the deviation is harmless.
            0 | 1 => self.0 = bits, // Set exactly these bits
            2 => self.0 |= bits,    // OR with current
            3 => self.0 &= !bits,   // Clear specified bits
            // An undefined mode (>= 4) changes nothing, as in kitty's set handler.
            _ => {}
        }
    }
}

/// Kitty keyboard protocol stack entry: flags with a valid bit (0x80) set. The stack pointer,
/// not this bit, tracks validity.
#[derive(Debug, Clone, Copy, Default)]
struct KittyKeyboardStackEntry(u8);

impl KittyKeyboardStackEntry {
    const VALID_BIT: u8 = 0x80;

    /// Create a valid entry with the given flags.
    #[inline]
    fn new(flags: KittyKeyboardFlags) -> Self {
        Self(flags.bits() | Self::VALID_BIT)
    }

    /// Get the flags from this entry.
    #[inline]
    fn flags(self) -> KittyKeyboardFlags {
        KittyKeyboardFlags::from_bits(self.0 & !Self::VALID_BIT)
    }
}

/// Kitty keyboard protocol state.
///
/// Maintains the current flags and a stack for push/pop operations.
/// Main and alternate screens have separate stacks.
#[derive(Debug, Clone)]
pub struct KittyKeyboardState {
    /// Current active flags.
    flags: KittyKeyboardFlags,
    /// Saved flags for the main screen while the alternate screen is active.
    main_saved_flags: Option<KittyKeyboardFlags>,
    /// Saved flags for the alternate screen while the main screen is active.
    alt_saved_flags: Option<KittyKeyboardFlags>,
    /// Stack for the main screen: 8 slots, at most 7 saved entries (+ the live mode).
    main_stack: [KittyKeyboardStackEntry; 8],
    /// Stack for the alternate screen.
    alt_stack: [KittyKeyboardStackEntry; 8],
    /// Current stack pointer for main screen (points to next free slot).
    main_sp: usize,
    /// Current stack pointer for alternate screen.
    alt_sp: usize,
}

/// Target stack for kitty keyboard push/pop operations.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenBuffer {
    /// Main screen buffer.
    Main,
    /// Alternate screen buffer.
    Alternate,
}

/// Serializable snapshot of kitty keyboard runtime state.
///
/// This captures the protocol state in terms of semantic flag values rather
/// than the internal stack-entry encoding used by `KittyKeyboardState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KittyKeyboardStateSnapshot {
    /// Current active flags.
    pub flags: KittyKeyboardFlags,
    /// Saved flags for the main screen while the alternate screen is active.
    pub main_saved_flags: Option<KittyKeyboardFlags>,
    /// Saved flags for the alternate screen while the main screen is active.
    pub alt_saved_flags: Option<KittyKeyboardFlags>,
    /// Main-screen push/pop stack contents.
    pub main_stack: [KittyKeyboardFlags; 8],
    /// Alternate-screen push/pop stack contents.
    pub alt_stack: [KittyKeyboardFlags; 8],
    /// Main-screen stack pointer (next free slot).
    pub main_sp: u8,
    /// Alternate-screen stack pointer (next free slot).
    pub alt_sp: u8,
}

impl From<bool> for ScreenBuffer {
    fn from(is_alternate: bool) -> Self {
        if is_alternate {
            Self::Alternate
        } else {
            Self::Main
        }
    }
}

impl Default for KittyKeyboardState {
    fn default() -> Self {
        Self::new()
    }
}

impl KittyKeyboardState {
    /// Create new keyboard state with no enhancements.
    pub fn new() -> Self {
        Self {
            flags: KittyKeyboardFlags::none(),
            main_saved_flags: None,
            alt_saved_flags: None,
            main_stack: [KittyKeyboardStackEntry::default(); 8],
            alt_stack: [KittyKeyboardStackEntry::default(); 8],
            main_sp: 0,
            alt_sp: 0,
        }
    }

    /// Get current keyboard flags.
    #[must_use]
    #[inline]
    pub fn flags(&self) -> KittyKeyboardFlags {
        self.flags
    }

    /// Query current flags (for CSI ? u response).
    #[must_use]
    #[inline]
    pub fn query_flags(&self) -> u8 {
        self.flags.bits()
    }

    /// Set flags directly (CSI = flags u or CSI = flags ; mode u).
    pub fn set_flags(&mut self, bits: u8, mode: u8) {
        self.flags.apply(bits, mode);
    }

    /// Save flags for the current screen and restore the target screen's flags.
    pub fn switch_screen(&mut self, entering_alt: bool) {
        if entering_alt {
            self.main_saved_flags = Some(self.flags);
            self.flags = self
                .alt_saved_flags
                .take()
                .unwrap_or(KittyKeyboardFlags::none());
        } else {
            self.alt_saved_flags = Some(self.flags);
            self.flags = self
                .main_saved_flags
                .take()
                .unwrap_or(KittyKeyboardFlags::none());
        }
    }

    /// Push the current flags (CSI > flags u) and activate `new_flags`; a full stack evicts
    /// its oldest entry.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn push_flags_for_buffer(&mut self, new_flags: u8, buffer: ScreenBuffer) {
        let (stack, sp) = if matches!(buffer, ScreenBuffer::Alternate) {
            (&mut self.alt_stack, &mut self.alt_sp)
        } else {
            (&mut self.main_stack, &mut self.main_sp)
        };

        // A full stack evicts its oldest entry. The depth is 7 saved entries + the live mode =
        // 8, kitty's 8-slot key_encoding_flags (current mode on top); a deeper stack diverges
        // from kitty when a pop empties it after an eviction. `(i + 1) & 7` is `i + 1` for i in
        // 0..6, masked (with a saturating add) so the Trust gate sees every index in bounds.
        if *sp >= 7 {
            for i in 0..6 {
                stack[i] = stack[i.saturating_add(1) & 7];
            }
            *sp = 6;
        }

        // `*sp <= 6` here, so the mask is identity and the bump exact; the mask sits in the
        // index expression because a `let` binding did not carry the 0..=7 range to the gate.
        stack[*sp & 7] = KittyKeyboardStackEntry::new(self.flags);
        *sp = (*sp & 7).saturating_add(1);

        self.flags = KittyKeyboardFlags::from_bits(new_flags);
    }

    /// Pop `count` entries (CSI < n u), restoring the flags the deepest popped entry saved;
    /// popping more entries than the stack holds resets the flags to 0.
    pub fn pop_flags_for_buffer(&mut self, count: u16, buffer: ScreenBuffer) {
        let (stack, sp) = if matches!(buffer, ScreenBuffer::Alternate) {
            (&mut self.alt_stack, &mut self.alt_sp)
        } else {
            (&mut self.main_stack, &mut self.main_sp)
        };

        // An explicit `CSI < 0 u` pops nothing (kitty's loop is `num && i-- > 0`); the spec's
        // default of 1 covers only an omitted parameter, which the dispatcher resolves. The
        // early return also keeps count=0 from reloading the one-past-top slot.
        let count = count as usize;
        if count == 0 {
            return;
        }

        // Popping more than the stack holds (an empty stack included) resets to 0 per the
        // spec, also when flags were set by `CSI = u` without a push (#7421, #7482).
        if count > *sp {
            self.flags = KittyKeyboardFlags::none();
            *sp = 0;
            return;
        }

        // Normal pop: restore stack[new_sp], including new_sp == 0, which holds the flags the
        // first push saved (#5679). `count <= *sp <= 7`, so `wrapping_sub` never wraps and the
        // mask is identity; both only prove the subtraction and load safe to the Trust gate.
        let new_sp = (*sp).wrapping_sub(count);
        self.flags = stack[new_sp & 7].flags();
        *sp = new_sp;
    }

    /// Reset the current screen's kitty keyboard state only (DECSTR). The kitty spec is silent
    /// on soft reset; this follows the xterm-style convention foot and ghostty use. The
    /// inactive screen's saved flags and stack survive; RIS ([`Self::reset`]) wipes both, as
    /// kitty's screen_reset does.
    pub fn reset_current_screen(&mut self, buffer: ScreenBuffer) {
        self.flags = KittyKeyboardFlags::none();
        if matches!(buffer, ScreenBuffer::Alternate) {
            self.alt_stack = [KittyKeyboardStackEntry::default(); 8];
            self.alt_sp = 0;
            // Already None while the screen is active; cleared for safety.
            self.alt_saved_flags = None;
        } else {
            self.main_stack = [KittyKeyboardStackEntry::default(); 8];
            self.main_sp = 0;
            self.main_saved_flags = None;
        }
    }

    /// Reset keyboard state (for RIS).
    pub fn reset(&mut self) {
        self.flags = KittyKeyboardFlags::none();
        self.main_saved_flags = None;
        self.alt_saved_flags = None;
        self.main_stack = [KittyKeyboardStackEntry::default(); 8];
        self.alt_stack = [KittyKeyboardStackEntry::default(); 8];
        self.main_sp = 0;
        self.alt_sp = 0;
    }

    /// Capture a stable snapshot of the current kitty keyboard state.
    #[must_use]
    pub fn snapshot(&self) -> KittyKeyboardStateSnapshot {
        let mut main_stack = [KittyKeyboardFlags::none(); 8];
        for (dst, src) in main_stack.iter_mut().zip(self.main_stack.iter()) {
            *dst = src.flags();
        }

        let mut alt_stack = [KittyKeyboardFlags::none(); 8];
        for (dst, src) in alt_stack.iter_mut().zip(self.alt_stack.iter()) {
            *dst = src.flags();
        }

        KittyKeyboardStateSnapshot {
            flags: self.flags,
            main_saved_flags: self.main_saved_flags,
            alt_saved_flags: self.alt_saved_flags,
            main_stack,
            alt_stack,
            // At most 7 saved entries (+ the live mode = kitty's 8). A restored sp of 8 would
            // make the next push evict one entry and overwrite another.
            main_sp: self.main_sp.min(7) as u8,
            alt_sp: self.alt_sp.min(7) as u8,
        }
    }

    /// Restore kitty keyboard state from a semantic snapshot.
    pub fn restore_snapshot(&mut self, snapshot: KittyKeyboardStateSnapshot) {
        self.flags = snapshot.flags;
        self.main_saved_flags = snapshot.main_saved_flags;
        self.alt_saved_flags = snapshot.alt_saved_flags;
        // Clamp to the 7-saved-entry depth (see `snapshot`).
        self.main_sp = usize::from(snapshot.main_sp.min(7));
        self.alt_sp = usize::from(snapshot.alt_sp.min(7));

        self.main_stack = [KittyKeyboardStackEntry::default(); 8];
        for (dst, src) in self
            .main_stack
            .iter_mut()
            .take(self.main_sp)
            .zip(snapshot.main_stack.iter())
        {
            *dst = KittyKeyboardStackEntry::new(*src);
        }

        self.alt_stack = [KittyKeyboardStackEntry::default(); 8];
        for (dst, src) in self
            .alt_stack
            .iter_mut()
            .take(self.alt_sp)
            .zip(snapshot.alt_stack.iter())
        {
            *dst = KittyKeyboardStackEntry::new(*src);
        }
    }
}
