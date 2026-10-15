// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! State accessors for `Terminal`: keyboard, cursor, modes, title, icon,
//! security authorizations and snapshots.

use super::{
    CurrentStyle, Cursor, CursorStyle, KittyKeyboardFlags, MAX_TITLE_BYTES, Terminal,
    XtermKeyboardState,
};
use crate::grid::ResizePolicy;
use aterm_types::PipelineTimestamps;
use std::sync::Arc;

/// Production accessors used by external crates and FFI.
impl Terminal {
    /// Get a reference to the vi mode state.
    #[must_use]
    #[inline]
    pub fn vi_mode(&self) -> &crate::vi_mode::ViMode {
        &self.vi
    }

    /// Get a mutable reference to the vi mode state.
    #[inline]
    pub fn vi_mode_mut(&mut self) -> &mut crate::vi_mode::ViMode {
        &mut self.vi
    }

    /// Whether vi (keyboard copy-mode) is active. The GUI gates its vi key handling
    /// and the vi-cursor render override on this.
    #[must_use]
    #[inline]
    pub fn vi_is_active(&self) -> bool {
        self.vi.is_active()
    }

    /// The vi cursor position (grid `line`, `col`). While vi mode is active the
    /// GUI paints the cursor here (mapped to a screen row via the display
    /// offset) instead of at the terminal cursor.
    #[must_use]
    #[inline]
    pub fn vi_cursor_point(&self) -> crate::vi_mode::ViPoint {
        self.vi.cursor_point()
    }

    /// Toggle vi mode on/off.
    ///
    /// When toggling on, the cursor is placed at the terminal's current
    /// cursor position. Split-borrows `grid` and `vi` to avoid borrow conflicts.
    pub fn vi_toggle(&mut self) {
        let cursor = self.grid.cursor();
        let point = crate::vi_mode::ViPoint::new(i32::from(cursor.row), cursor.col);
        self.vi.toggle(point);
    }

    /// Execute a vi mode motion with grid access.
    ///
    /// Split-borrows `grid` (read) and `vi` (write) to dispatch motions
    /// that require grid content (word, bracket, paragraph, search).
    pub fn vi_motion(
        &mut self,
        motion: crate::vi_mode::ViMotion,
        boundary: crate::vi_mode::ViBoundary,
    ) {
        self.vi.motion_with_grid(&self.grid, motion, boundary);
    }

    /// Keep the vi cursor on its text after a batch moved rows, then clamp it
    /// onto the grid.
    ///
    /// A vi line is counted from the live screen's top. `lines` is the rise of
    /// the grid's absolute row counter (the count that re-pins a scrolled-back
    /// viewport): output that scrolled the whole screen pushed every line that
    /// many lines up. History grows only by such scrolls and by a region at the
    /// top scrolling into it, both counted by the rise, so a line already in
    /// history always moves up by the whole rise.
    ///
    /// `bands` is the batch's row-band record when it explains every row the
    /// batch moved (`post_process`). A cursor on the screen then rides only the
    /// moves its row was in, replayed in order:
    ///
    /// * a region at the top of a screen with history (`top == 0`, the counter
    ///   rising): its rows and all of history go up and its top rows enter
    ///   history, while the fixed rows below it stay (Codex's composer, apt's
    ///   progress bar);
    /// * any other band (an interior region, IL/DL, a reverse index, a top
    ///   region with no history): its rows move, a cursor whose row left the
    ///   band rests on the edge it left through, as Alacritty's does, and
    ///   every line outside it stays.
    ///
    /// A row that entered history is among the batch's `lines` newest history
    /// lines, so the replay stops there. Only a batch that also took rows off
    /// the top of such a region without archiving them (DL at row 0) could
    /// carry it further: the band record composes the two moves into one.
    ///
    /// The clamp runs on every batch, so a batch that cleared history (ED 3)
    /// leaves no cursor below the oldest line. A cursor whose text left history
    /// rests on the oldest line kept, which at display offset 0 can be above
    /// the viewport: aterm's cursor follows its text out of a live viewport on
    /// purpose. Alacritty's `scroll_up_relative` instead stops it at the
    /// viewport's top, on text that is not its own.
    pub(super) fn vi_follow_output(&mut self, lines: u64, bands: Option<aterm_grid::RowBandMoves>) {
        if !self.vi_is_active() {
            return;
        }
        let rise = i32::try_from(lines).unwrap_or(i32::MAX);
        let before = self.vi.cursor_point().line;
        let after = match bands {
            Some(bands) if before >= 0 => bands
                .as_slice()
                .iter()
                .fold(before, |line, band| {
                    let (top, bottom) = (i32::from(band.top), i32::from(band.bottom));
                    let moved = line.saturating_add(i32::from(band.delta));
                    if top == 0 && band.delta < 0 && rise > 0 {
                        // Archival: history and the region go up, the region's top
                        // rows into history; the rows below it stay.
                        if line <= bottom { moved } else { line }
                    } else if (top..=bottom).contains(&line) {
                        // A row moved out of the band is gone: rest on its edge.
                        moved.clamp(top, bottom)
                    } else {
                        line
                    }
                })
                .max(rise.saturating_neg()),
            _ => before.saturating_sub(rise),
        };
        self.vi_shift(after.saturating_sub(before));
    }

    /// Move the vi cursor `delta` lines and clamp it onto the grid: its line
    /// into `-history..=rows - 1`, its column into the width. While an
    /// offloaded reflow holds the history (a window's width change), no
    /// history is in memory to clamp against: the line keeps its floor until
    /// [`Self::finish_resize_offload`] or [`Self::abort_resize_offload`]
    /// clamps it — clamped then, a width change in copy mode put a cursor in
    /// history on live line 0.
    pub(super) fn vi_shift(&mut self, delta: i32) {
        if !self.vi_is_active() {
            return;
        }
        let topmost = if self.grid.reflow_offload_in_flight() {
            i32::MIN
        } else {
            aterm_types::BufferAccess::total_lines(&self.grid).saturating_neg()
        };
        let bottommost = i32::from(self.grid.rows()).saturating_sub(1);
        let last_col = self.grid.cols().saturating_sub(1);
        self.vi.scroll(topmost, bottommost, last_col, delta);
    }

    /// Execute a vi mode inline character search (f/F/t/T).
    ///
    /// Split-borrows `grid` (read) and `vi` (write).
    pub fn vi_inline_search_execute(
        &mut self,
        needle: char,
        kind: crate::vi_mode::InlineSearchKind,
    ) -> bool {
        self.vi.inline_search_execute(&self.grid, needle, kind)
    }

    /// Repeat the last vi mode inline search (`;`).
    pub fn vi_inline_search_repeat(&mut self) -> bool {
        self.vi.inline_search_repeat(&self.grid)
    }

    /// Repeat the last vi mode inline search in reverse (`,`).
    pub fn vi_inline_search_repeat_reverse(&mut self) -> bool {
        self.vi.inline_search_repeat_reverse(&self.grid)
    }

    /// Get the current Kitty keyboard enhancement flags.
    #[must_use]
    pub fn kitty_keyboard_flags(&self) -> KittyKeyboardFlags {
        self.kitty_keyboard.flags()
    }

    /// Enable or disable the Kitty keyboard protocol (default on).
    ///
    /// When disabled the terminal behaves as if the protocol is unsupported: the
    /// `CSI ? u` query gets no reply, `CSI > u` / `= u` / `< u` are consumed and
    /// ignored, and [`keyboard_mode`](Self::keyboard_mode) never carries kitty
    /// flags, including ones negotiated earlier. For embedders whose platform
    /// consumes kitty sequences itself (Windows ConPTY; xterm.js
    /// `vtExtensions.kittyKeyboard = false`). Host policy: preserved across
    /// `reset()`/RIS like the `allow_*` bits.
    pub fn set_kitty_keyboard_enabled(&mut self, enabled: bool) {
        self.modes.kitty_keyboard_enabled = enabled;
        self.refresh_mode_mirror();
    }

    /// Declare whether this session sits behind a Windows console host
    /// (ConPTY), whose `CSI ? 9001 h` asks for win32-input-mode key records
    /// (default off). Only then does DEC 9001 take effect; anywhere else it is
    /// program output and is ignored, so a file that writes it cannot turn the
    /// person's Escape and Enter chords into records no console host reads.
    /// Host policy: preserved across `reset()`/RIS like the kitty capability.
    pub fn set_win32_input_enabled(&mut self, enabled: bool) {
        self.modes.win32_input_enabled = enabled;
        self.refresh_mode_mirror();
    }

    /// Set the keyboard capabilities a console host decides, from the fact
    /// that also picks the session's resize seam policy, so the two cannot
    /// disagree. Behind a Windows console host ([`ResizePolicy::ConPty`]),
    /// conhost's DEC 9001 is honored
    /// ([`set_win32_input_enabled`](Self::set_win32_input_enabled)) and the
    /// kitty protocol is off
    /// ([`set_kitty_keyboard_enabled`](Self::set_kitty_keyboard_enabled)):
    /// conhost cannot turn CSI-u reports into the key records a console
    /// program reads. Any other session gets the reverse. It sets both bits
    /// whichever way they stood, so a checkpoint that carried another host's
    /// bits does not outlive it. A live session and every replay of one
    /// (`cast drift`) call this with the policy they resize under.
    pub fn set_console_host_posture(&mut self, policy: ResizePolicy) {
        let console_host = policy == ResizePolicy::ConPty;
        self.modes.win32_input_enabled = console_host;
        self.modes.kitty_keyboard_enabled = !console_host;
        self.refresh_mode_mirror();
    }

    /// Whether the Kitty keyboard protocol capability is enabled
    /// (see [`set_kitty_keyboard_enabled`](Self::set_kitty_keyboard_enabled)).
    #[must_use]
    #[cfg(test)]
    pub fn is_kitty_keyboard_enabled(&self) -> bool {
        self.modes.kitty_keyboard_enabled
    }

    /// Get the xterm keyboard modifier/format state (XTMODKEYS/XTFMTKEYS).
    #[must_use]
    pub fn xterm_keyboard(&self) -> &XtermKeyboardState {
        &self.xterm_keyboard
    }

    /// Get the current style.
    #[must_use]
    pub fn style(&self) -> &CurrentStyle {
        &self.style
    }

    /// Set the window title.
    ///
    /// Titles longer than `MAX_TITLE_BYTES` are truncated at a char boundary.
    pub fn set_title(&mut self, title: &str) {
        let title = &title[..title.floor_char_boundary(MAX_TITLE_BYTES)];
        // Bump the title-change epoch only on a real value change (mirrors the
        // OSC 0/2 handler) so `title_epoch()` is a faithful change signal.
        if &*self.title.window != title {
            self.title
                .epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.title.window = title.into();
    }

    /// Get the window title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title.window
    }

    /// The monotonic count of bells that fired (past the throttle). Watermark
    /// observers (the `subscribe events` stream) compare it across wakes to emit
    /// one `EVENT … bell` per bell without a take-and-clear race.
    #[must_use]
    pub fn bell_total(&self) -> u64 {
        self.bell_total
    }

    /// The window title as a shared [`Arc<str>`] handle: a reference-count bump,
    /// so callers can hold the title without copying or borrowing the terminal.
    #[must_use]
    pub fn title_arc(&self) -> std::sync::Arc<str> {
        Arc::clone(&self.title.window)
    }

    /// Monotonic tab-label change epoch (window title or reported cwd).
    ///
    /// Incremented each time a value a host labels a tab with changes: the
    /// window title (OSC 0/2 or [`set_title`](Self::set_title)) or the
    /// shell-reported cwd (OSC 7 / OSC 633 `P;Cwd=`), shown for an unset title.
    /// A no-op re-set does not bump it. Read with a lock-free `Relaxed` load so
    /// hosts can poll without taking the terminal lock; it wraps, so compare
    /// against a cached value.
    #[must_use]
    pub fn title_epoch(&self) -> u64 {
        self.title.epoch.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Get the icon name.
    #[must_use]
    #[cfg(test)]
    pub fn icon_name(&self) -> &str {
        &self.title.icon
    }

    /// Enable or disable secure keyboard entry mode.
    ///
    /// Advisory: the UI layer activates the platform's secure input:
    ///
    /// - **macOS**: `EnableSecureEventInput()` / `DisableSecureEventInput()`
    /// - **iOS**: not applicable (sandboxed)
    /// - **Windows**: limited protection available
    /// - **Linux/X11**: not possible
    /// - **Linux/Wayland**: secure by default
    #[cfg(test)]
    pub fn set_secure_keyboard_entry(&mut self, enabled: bool) {
        self.secure_keyboard_entry = enabled;
    }

    /// Report the host OS color scheme (light/dark) to the engine, at startup and
    /// on every appearance change. Apps read it via DSR `CSI ? 996 n`; when it
    /// changes and DEC mode 2031 is set, the engine queues `CSI ? 997 ; Ps n`
    /// (1 = dark, 2 = light) for the host to drain via
    /// [`take_response`](Self::take_response). A no-op if unchanged.
    pub fn set_color_scheme(&mut self, scheme: aterm_types::Appearance) {
        if self.modes.color_scheme == scheme {
            return;
        }
        self.modes.color_scheme = scheme;
        if self.modes.report_color_scheme {
            // Unsolicited host-initiated push (not a query reply, so no capability
            // gating / rate limiting): append directly to the response buffer.
            use std::fmt::Write as _;
            let mut response = super::stack_response::StackResponse::<16>::new();
            let _ = write!(response, "\x1b[?997;{}n", scheme.dsr_code());
            self.transient
                .response_buffer
                .extend_from_slice(response.as_bytes());
        }
    }

    /// The host OS color scheme last reported via [`set_color_scheme`](Self::set_color_scheme).
    #[must_use]
    pub fn color_scheme(&self) -> aterm_types::Appearance {
        self.modes.color_scheme
    }

    /// Whether an OSC 52 query the program sends now reaches the host's
    /// clipboard callback: one is installed, and the gate its handler mints
    /// with opens (query access authorized, or allowed by the installed
    /// policy). A host reads the pasteboard for a query before it takes the
    /// terminal lock, and only for one this answers yes to (aterm-gui's
    /// `host_io_prefetch`).
    #[must_use]
    pub fn clipboard_query_admitted(&self) -> bool {
        self.clipboard.callback.is_some()
            && self
                .clipboard_auth
                .try_mint_query_capability_with_engine(self.policy.gates())
                .is_some()
    }

    /// Whether a Kitty non-direct medium resolver is installed
    /// ([`Self::set_kitty_file_resolver`]), i.e. whether a `t=f`/`t=t`/`t=s`
    /// transmission reaches the host at all.
    #[must_use]
    pub fn kitty_file_resolver_installed(&self) -> bool {
        self.kitty_file_resolver.is_some()
    }

    /// Authorize a clipboard access class for parser-origin sequences.
    ///
    /// `Write` covers OSC 52 set/clear and the OSC 1337 `CopyToClipboard` /
    /// `EndCopy` / `Copy=` paths; `Query` covers OSC 52 queries. This is the
    /// only way to mint a [`super::clipboard_auth::ClipboardWriteCapability`] or
    /// [`super::clipboard_auth::ClipboardQueryCapability`] (via
    /// [`super::clipboard_auth::ClipboardAuth`]).
    pub fn authorize_clipboard_access(&mut self, access: super::clipboard_auth::ClipboardAccess) {
        use super::clipboard_auth::ClipboardAccess;
        match access {
            ClipboardAccess::Write => {
                self.clipboard_auth.authorize_write();
                // Mirror the set bit on modes so checkpoints and FFI observers
                // agree with the capability state.
                self.modes.allow_osc52_set = true;
            }
            ClipboardAccess::Query => {
                self.clipboard_auth.authorize_query();
                // Mirror the query bit on modes for FFI/config back-compat.
                self.modes.allow_osc52_query = true;
            }
        }
    }

    /// Revoke a clipboard access class: later parser-origin sequences of that
    /// class are dropped (no callback, no response). Host-initiated clipboard
    /// operations are unaffected.
    pub fn revoke_clipboard_access(&mut self, access: super::clipboard_auth::ClipboardAccess) {
        use super::clipboard_auth::ClipboardAccess;
        match access {
            ClipboardAccess::Write => {
                self.clipboard_auth.revoke_write();
                self.modes.allow_osc52_set = false;
            }
            ClipboardAccess::Query => {
                self.clipboard_auth.revoke_query();
                self.modes.allow_osc52_query = false;
            }
        }
    }

    /// Authorize OSC 133 / OSC 633 shell integration with a 32-byte capability
    /// nonce.
    ///
    /// When [`TerminalModes::require_shell_integration_nonce`] is set, OSC 133
    /// A/B/C/D and OSC 633 A-H/P sequences dispatch only if their
    /// `id=<64-hex>` equals `nonce`; others are dropped silently. The comparison
    /// is constant-time.
    ///
    /// The host:
    /// 1. Generates `nonce` with a CSPRNG at session init.
    /// 2. Installs it here.
    /// 3. Passes it to the shell's environment (e.g. `ATERM_SHELL_NONCE`) so the
    ///    integration preamble emits `id=<hex>`.
    /// 4. Sets `modes.require_shell_integration_nonce = true` once the preamble
    ///    is in place; until then the check is bypassed (default off, like
    ///    [`allow_palette_reconfigure`][aterm_types::TerminalModes]).
    pub fn authorize_shell_integration(&mut self, nonce: [u8; 32]) {
        self.shell_integration_auth.authorize(nonce);
    }

    /// [`authorize_shell_integration`][Self::authorize_shell_integration] for a
    /// nonce the shell has not taken yet: the re-key of an adopted shell whose
    /// nonce did not survive an update, delivered through a file the shell reads
    /// at its next prompt (aterm-gui `shell_rekey`, the scripts'
    /// `__aterm_rekey_check`). Marks signed with it verify at once, but it is not
    /// in use until the first arrives:
    /// [`shell_integration_posture`][Self::shell_integration_posture] reads
    /// `Degraded` until then, and the seamless-handoff carry leaves it behind.
    pub fn authorize_shell_integration_on_first_mark(&mut self, nonce: [u8; 32]) {
        self.shell_integration_auth.authorize_on_first_mark(nonce);
    }

    /// [`authorize_shell_integration_on_first_mark`][Self::authorize_shell_integration_on_first_mark]
    /// for a typed re-key (aterm-gui `shell_rekey::typed`): a nonce left in a
    /// one-use file for a line typed at the prompt, healing a shell that has no
    /// re-key hook. It can be taken back: the replaced authorization is kept
    /// until the host restores it with
    /// [`withdraw_shell_integration_rekey`][Self::withdraw_shell_integration_rekey]
    /// (the line never ran) or forgets it with
    /// [`settle_shell_integration_rekey`][Self::settle_shell_integration_rekey]
    /// (the shell read the file). Marks verify at once; it is `On` from the
    /// first of them.
    pub fn authorize_shell_integration_rekey(&mut self, nonce: [u8; 32]) {
        self.shell_integration_auth.authorize_rekey(nonce);
    }

    /// Take back the typed re-key `nonce`
    /// ([`authorize_shell_integration_rekey`][Self::authorize_shell_integration_rekey]),
    /// restoring the authorization it replaced. `false`, changing nothing, when
    /// `nonce` is not that pending key (a mark signed with it already arrived, or
    /// it was replaced or settled).
    pub fn withdraw_shell_integration_rekey(&mut self, nonce: &[u8; 32]) -> bool {
        self.shell_integration_auth.withdraw_rekey(nonce)
    }

    /// Keep the typed re-key `nonce`: the shell read its file, so the
    /// authorization it replaced is forgotten. `false`, changing nothing, when
    /// `nonce` is not that key pending.
    pub fn settle_shell_integration_rekey(&mut self, nonce: &[u8; 32]) -> bool {
        self.shell_integration_auth.settle_rekey(nonce)
    }

    /// The nonce the shell's marks verify with now (a re-key not yet taken is
    /// not in use), or `None`. The seamless carry takes it across an update, and
    /// the typed upgrade of a healthy shell writes it into the one-use file so
    /// the relaunch line keeps the key the shell signs with.
    #[must_use]
    pub fn shell_integration_nonce_in_use(&self) -> Option<[u8; 32]> {
        self.shell_integration_auth.nonce_in_use()
    }

    /// Number of OSC 133 / OSC 633 sequences dropped by the capability-nonce
    /// gate since construction: no nonce authorized, no `id=`, or a wrong or
    /// malformed nonce. Only counted while
    /// `modes.require_shell_integration_nonce` is `true`.
    #[must_use]
    pub fn shell_integration_dropped_count(&self) -> u64 {
        self.shell_integration_auth.dropped_count()
    }

    /// Enable or disable OSC 133 / OSC 633 capability-nonce enforcement
    /// (`modes.require_shell_integration_nonce`). When `true`, every sequence
    /// must carry a matching `id=<64-hex>` (see [`authorize_shell_integration`]);
    /// `false` (the default) accepts unnonced shell integrations.
    ///
    /// Preserved across RIS and [`Terminal::reset`], so a program cannot turn
    /// enforcement off with a reset (see `reset.rs`).
    pub fn set_require_shell_integration_nonce(&mut self, required: bool) {
        self.modes.require_shell_integration_nonce = required;
    }

    /// Whether OSC 133 / OSC 633 capability-nonce enforcement is
    /// currently enabled.
    #[must_use]
    pub fn is_require_shell_integration_nonce(&self) -> bool {
        self.modes.require_shell_integration_nonce
    }

    /// Enable or disable OSC 9 / 99 / 777 desktop notifications (default off).
    ///
    /// While disabled, the handlers in `handler_osc_notify.rs` return before
    /// invoking any callback; hosts that wire a notification callback must also
    /// call this with `true`. `modes.allow_notifications` is the single source
    /// of truth; `Self::authorize_notifications` sets the same bool.
    ///
    /// Preserved across `Terminal::reset()` and RIS, so a program cannot
    /// re-enable notifications with a reset (see `reset.rs` and
    /// `handler_esc.rs::reset_terminal_state`).
    pub fn set_allow_notifications(&mut self, allowed: bool) {
        self.modes.allow_notifications = allowed;
    }

    /// Authorize OSC 9 / 99 / 777 desktop notifications: sets
    /// `modes.allow_notifications = true`, without which the handlers in
    /// `handler_osc_notify.rs` never reach the callback. The pending-notification
    /// cap applies regardless.
    #[cfg(test)]
    pub fn authorize_notifications(&mut self) {
        self.modes.allow_notifications = true;
    }

    /// Add an extra OSC 8 URI scheme to the safe allowlist: the one hyperlink
    /// decision a host makes (see [`super::hyperlink_auth`]; what a person may
    /// open is checked by the host at click time).
    ///
    /// Returns `false`, state unchanged, when `scheme` is over 32 bytes, not RFC
    /// 3986 scheme-shaped (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`), in the
    /// never-allow set (`javascript`/`data`/`file`/`vbscript`/`about`/`blob`,
    /// case- and suffix-insensitively), or when 4 extra schemes are already
    /// live. Returns `true` when stored (idempotent). Stored lowercased; matching
    /// is case-insensitive.
    ///
    /// The other OSC 8 guards (byte cap, control-char scan, BiDi-override
    /// rejection) still apply. Extra schemes are terminal-instance state: they
    /// survive soft and hard resets and are excluded from checkpoints.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "hyperlink_scheme_cap",
            action = "Authorize",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "hyperlink_scheme_cap",
            action = "AuthorizeOther",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "hyperlink_scheme_cap",
            action = "RefuseAtCap",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "hyperlink_scheme_cap",
            action = "RefuseNeverAllow",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    pub fn authorize_hyperlink_scheme(&mut self, scheme: &str) -> bool {
        self.hyperlink_auth.authorize_scheme(scheme)
    }

    /// Remove a host-minted extra scheme (case-insensitive): OSC 8 URIs with it
    /// are refused again. Absent schemes are a no-op.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "hyperlink_scheme_cap",
            action = "Revoke",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    pub fn revoke_hyperlink_scheme(&mut self, scheme: &str) {
        self.hyperlink_auth.revoke_scheme(scheme);
    }

    /// Whether `scheme` is a live host-minted extra scheme (case-insensitive).
    /// The built-in safe allowlist is not consulted.
    #[must_use]
    pub fn is_hyperlink_scheme_authorized(&self, scheme: &str) -> bool {
        self.hyperlink_auth
            .extra_schemes()
            .iter()
            .any(|s| s.eq_ignore_ascii_case(scheme))
    }

    /// Count of live host-minted extra schemes (Tier-1 projection aid).
    #[must_use]
    pub fn hyperlink_extra_scheme_count(&self) -> usize {
        self.hyperlink_auth.extra_schemes().len()
    }

    /// Install or replace the OSC / escape-sequence policy engine (the GUI does
    /// so for every session before its reader runs). The engine is enforcing:
    /// the OSC 52, XTWINOPS, response and rate-limit gates consult it (see
    /// `policy_bridge.rs`), adding restrictions on top of the `authorize_*` bits,
    /// never widening them.
    pub fn apply_policy_engine(&mut self, engine: aterm_policy::engine::PolicyEngine) {
        // `install` recompiles the constant-probe gate table in the same step,
        // so no gate can answer from the policy this call replaced.
        self.policy.install(engine);
    }

    /// Clear the installed policy engine, returning to the
    /// `TerminalModes::allow_*`-only behavior a checkpoint restore starts from
    /// (checkpoints do not carry the engine).
    #[cfg(test)]
    pub fn clear_policy_engine(&mut self) {
        // `clear` recompiles the gate table to the no-engine posture.
        self.policy.clear();
    }

    /// Enable or disable OSC 4 / OSC 21 indexed palette reconfigure (default
    /// off): while disabled, PTY-origin attempts to change the 256-entry palette
    /// are ignored. Equivalent to setting
    /// `modes_mut().allow_palette_reconfigure`; preserved across
    /// `Terminal::reset()` and RIS so a program cannot re-enable it with a reset.
    pub fn set_allow_palette_reconfigure(&mut self, allowed: bool) {
        self.modes.allow_palette_reconfigure = allowed;
    }

    /// Get cursor position.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        self.grid.cursor()
    }

    /// Monotonic autowrap serial of the active grid. Advances each time an
    /// autowrap line advance resolves (`Grid::advance_autowrap_line`); hard
    /// newlines never advance it. A host diffs it against a per-session baseline
    /// read under the same lock as [`Self::cursor`] to tell a wrap from `Home`
    /// pressed at the last column.
    ///
    /// Not checkpointed, and each grid keeps its own serial across main/alt
    /// swaps, so a restore or buffer switch costs at most one spurious read,
    /// which the host's same-session baseline diff tolerates.
    #[must_use]
    pub fn wrap_serial(&self) -> u64 {
        self.grid.wrap_serial()
    }

    /// Check if cursor is visible.
    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.modes.cursor_visible
    }

    /// The echo anchor: where the most recent PTY print run ended, as `(row,
    /// col)` in active-grid coordinates one past the last glyph (a pending wrap
    /// counts, so a glyph at the last column reports `col == cols`), plus a
    /// monotonic per-print sequence so a host can tell new output from a stale
    /// sample. `None` before the first print or after a reset. For the
    /// cursor-effect host; never read by the terminal itself.
    #[must_use]
    pub fn print_anchor(&self) -> Option<(u16, u16, u64)> {
        self.transient
            .print_anchor
            .map(|(row, col)| (row, col, self.transient.print_anchor_seq))
    }

    /// What the active grid holds now at the echo anchor run's last cell
    /// (`(row, col - 1)` of [`Self::print_anchor`], independent of scroll), in
    /// [`Self::row_cols_into`]'s per-column convention: the lead char, `'\0'` at
    /// a wide continuation, `' '` for a blank. `None` before the first print, for
    /// a run ending at column 0, or when the anchor is off the grid. Lets the
    /// cursor-effect host tell a key's echo from something drawn after it (zsh's
    /// i-search ends on a fake cursor).
    #[must_use]
    pub fn print_anchor_glyph(&self) -> Option<char> {
        let (row, col) = self.transient.print_anchor?;
        let col = col.checked_sub(1)?;
        (row < self.grid.rows() && col < self.grid.cols())
            .then(|| self.grid.screen_row_view(row).col_char(col))
    }

    /// The viewport row the cursor projects onto, on screen or not: its
    /// active-grid row pushed down by the scrollback offset.
    #[must_use]
    pub fn projected_cursor_row(&self) -> usize {
        (self.cursor().row as usize).saturating_add(self.grid().display_offset())
    }

    /// The viewport row the cursor occupies in a `rows`-tall frame, or `None`
    /// when nothing is drawn there (DECTCEM hid it, or scroll-back pushed it off
    /// the bottom). `cell_frame_into` uses this same projection for
    /// `cursor_visible`.
    #[must_use]
    pub fn cursor_row_on_screen(&self, rows: usize) -> Option<usize> {
        let row = self.projected_cursor_row();
        (self.cursor_visible() && row < rows).then_some(row)
    }

    /// DECSCNM (mode 5) reverse video. Applied at colour-resolve time, it changes
    /// the rendered frame without touching cells (bumps `damage_epoch`, not
    /// `content_seq`), so introspection consumers read it here.
    #[must_use]
    pub fn reverse_video(&self) -> bool {
        self.modes.reverse_video
    }

    /// Monotonic repaint-blink epoch: DECTCEM hides processed while DEC 2026
    /// synchronized output was active, the per-keystroke repaint bracket of
    /// full-redraw TUIs. Hosts diff it to classify apps that repaint per
    /// keystroke; vim/less and ConPTY never advance it. Never reset, never gates
    /// bytes.
    #[must_use]
    pub fn repaint_blink_epoch(&self) -> u64 {
        self.repaint_blink_epoch
    }

    /// Get current cursor style.
    #[must_use]
    pub fn cursor_style(&self) -> CursorStyle {
        self.modes.cursor_style
    }

    /// Set the host-preferred default cursor style, used before any DECSCUSR and
    /// restored on RIS/DECSTR. The live style follows the new default only while
    /// the app has not set its own (the `was_default` guard), so an app's
    /// DECSCUSR is not clobbered.
    pub fn set_default_cursor_style(&mut self, style: CursorStyle) {
        let was_default = self.modes.cursor_style == self.default_cursor_style;
        self.default_cursor_style = style;
        if was_default && self.modes.cursor_style != style {
            self.modes.cursor_style = style;
            // Mode 12 mirrors the style's blink bit (see `set_cursor_blink`).
            self.modes.cursor_blink = style.blinks();
        }
    }

    /// Get number of rows.
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.grid.rows()
    }

    /// Get number of columns.
    #[must_use]
    pub fn cols(&self) -> u16 {
        self.grid.cols()
    }

    /// Pipeline timestamps from the last `process()` call: per-stage durations
    /// (parse, grid, total), carried by the render snapshot for latency
    /// breakdowns.
    #[must_use]
    #[inline]
    pub fn pipeline_timestamps(&self) -> &PipelineTimestamps {
        &self.transient.pipeline_timestamps
    }

    /// Process-unique identity stamped into every engine-filled RenderInput.
    /// Cursor-effect input witnesses use the same nonce to prove that an arm
    /// and its later content observation belong to one Terminal instance.
    #[must_use]
    #[inline]
    pub fn render_identity(&self) -> u64 {
        self.extract_identity
    }
}

// ---------------------------------------------------------------------------
// Test-only accessors for session serialization and charset tests.
// ---------------------------------------------------------------------------

use super::TerminalModes;

impl Terminal {
    /// Get a mutable reference to the terminal modes.
    pub fn modes_mut(&mut self) -> &mut TerminalModes {
        &mut self.modes
    }

    /// Set the host's alternate-scroll default (DEC 1007): the live mode now,
    /// and the value [`Self::foreground_handback`] restores 1007 to. Programs
    /// may still change the live mode with `CSI ? 1007 h/l`.
    pub fn set_host_alternate_scroll(&mut self, on: bool) {
        self.modes.alternate_scroll = on;
        self.configured_modes.alternate_scroll = on;
        // 1007 rides the lock-free mode word (the wheel's route reads it).
        self.refresh_mode_mirror();
    }
}

/// Append a DEC mode-2048 in-band size report to a response buffer.
///
/// Format `CSI 48 ; rows ; cols ; pixH ; pixW t` (matching xterm/ghostty). Pixel
/// dimensions are `rows*cell_h` / `cols*cell_w`, computed in `u32` to avoid u16
/// overflow; `0` cell sizes yield `0` pixels (apps tolerate this). Shared by the
/// DECSET-2048 enable path and `Terminal::resize`.
pub(super) fn push_in_band_size_report(
    buf: &mut Vec<u8>,
    rows: u16,
    cols: u16,
    cell_w: u16,
    cell_h: u16,
) {
    use std::fmt::Write as _;
    let pix_h = u32::from(rows) * u32::from(cell_h);
    let pix_w = u32::from(cols) * u32::from(cell_w);
    let mut r = super::stack_response::StackResponse::<48>::new();
    let _ = write!(r, "\x1b[48;{rows};{cols};{pix_h};{pix_w}t");
    buf.extend_from_slice(r.as_bytes());
}
