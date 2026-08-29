// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! State accessor methods for Terminal.
//!
//! Contains keyboard, cursor, mode, title, icon, security, and snapshot accessors.
//! Extracted from mod.rs to reduce file size.

#[cfg(test)]
use super::CharacterSetState;
#[cfg(test)]
use super::SavedCursorState;
use super::{
    CurrentStyle, Cursor, CursorStyle, KittyKeyboardFlags, KittyKeyboardState, MAX_TITLE_BYTES,
    Terminal, TerminalSize, TerminalSnapshot, XtermKeyboardState,
};
use crate::scrollback::ScrollbackStorage;
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
    /// and the vi-cursor render override on this (VI-1).
    #[must_use]
    #[inline]
    pub fn vi_is_active(&self) -> bool {
        self.vi.is_active()
    }

    /// The vi cursor position (grid `line`, `col`). While vi mode is active the GUI
    /// paints the cursor here (mapped to a screen row via the display offset) instead
    /// of at the terminal cursor (VI-1).
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

    /// Get the Kitty keyboard protocol state.
    #[must_use]
    pub fn kitty_keyboard(&self) -> &KittyKeyboardState {
        &self.kitty_keyboard
    }

    /// Get the current Kitty keyboard enhancement flags.
    #[must_use]
    pub fn kitty_keyboard_flags(&self) -> KittyKeyboardFlags {
        self.kitty_keyboard.flags()
    }

    /// Enable or disable the Kitty keyboard protocol capability (default ON).
    ///
    /// When disabled the terminal behaves as if the protocol is UNSUPPORTED:
    /// the `CSI ? u` query gets no reply, `CSI > u`/`CSI = u`/`CSI < u`
    /// push/set/pop are consumed-and-ignored (no state change), and
    /// [`keyboard_mode`](Self::keyboard_mode) never carries kitty flags —
    /// even ones negotiated before the host disabled it. For embedders whose
    /// platform consumes kitty sequences itself (e.g. Windows ConPTY; xterm.js
    /// `vtExtensions.kittyKeyboard = false`). Host policy: preserved across
    /// `reset()`/RIS like the `allow_*` bits.
    pub fn set_kitty_keyboard_enabled(&mut self, enabled: bool) {
        self.modes.kitty_keyboard_enabled = enabled;
    }

    /// Whether the Kitty keyboard protocol capability is enabled
    /// (see [`set_kitty_keyboard_enabled`](Self::set_kitty_keyboard_enabled)).
    #[must_use]
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

    /// Get a mutable reference to the Kitty keyboard state.
    pub fn kitty_keyboard_mut(&mut self) -> &mut KittyKeyboardState {
        &mut self.kitty_keyboard
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
        if let Some(ref mut callback) = self.title.callback {
            callback(&self.title.window);
        }
        if let Some(ref mut callback) = self.title.event_callback {
            callback(aterm_types::TitleType::WindowOnly, &self.title.window);
        }
    }

    /// Get the window title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title.window
    }

    /// The monotonic count of bells that have FIRED (past the throttle). A
    /// watermark observer (e.g. the `subscribe events` stream) compares it across
    /// wakes to emit one `EVENT … bell` per new bell without a take-and-clear race.
    #[must_use]
    pub fn bell_total(&self) -> u64 {
        self.bell_total
    }

    /// Get the window title as a shared [`Arc<str>`] handle.
    ///
    /// A cheap reference-count bump of the title's backing allocation — lets a
    /// caller (e.g. the GUI) hold the title without copying the string or
    /// borrowing the terminal. Mirrors the `Arc::clone(&self.title.window)` the
    /// [`snapshot`](Self::snapshot) builder already uses.
    #[must_use]
    pub fn title_arc(&self) -> std::sync::Arc<str> {
        Arc::clone(&self.title.window)
    }

    /// Monotonic tab-LABEL-change epoch (window title OR reported cwd).
    ///
    /// Incremented (and only incremented) each time a value a host UI labels a
    /// tab with actually changes: the window title — via an OSC 0/2 sequence
    /// or [`set_title`](Self::set_title) — or the shell-reported working
    /// directory (OSC 7 / OSC 633 `P;Cwd=`), which the GUI shows in place of
    /// an unset title. Never bumped on a no-op re-set of the same string (a
    /// shell re-reports its cwd every prompt — the steady state must not
    /// thrash the signal). Reads use a lock-free `Relaxed` atomic load, so a
    /// host UI can cheaply poll for a tab's title/cwd drift (e.g. to refresh
    /// a tab strip) without taking a terminal lock on every output burst.
    /// Wraps via `fetch_add`; callers should compare against a cached value
    /// for change detection rather than assuming any absolute meaning.
    #[must_use]
    pub fn title_epoch(&self) -> u64 {
        self.title.epoch.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Get the icon name.
    #[must_use]
    pub fn icon_name(&self) -> &str {
        &self.title.icon
    }

    /// Enable or disable secure keyboard entry mode.
    ///
    /// When enabled, the UI layer should activate platform-specific secure input
    /// mechanisms to prevent keylogging:
    ///
    /// - **macOS**: Call `EnableSecureEventInput()` / `DisableSecureEventInput()`
    /// - **iOS**: Not applicable (sandboxed by default)
    /// - **Windows**: Limited protection available (document to users)
    /// - **Linux/X11**: Not possible (X11 is inherently insecure)
    /// - **Linux/Wayland**: Secure by default (no action needed)
    ///
    /// This flag is advisory - the terminal library does not implement the
    /// platform-specific security APIs directly. The UI layer must check this
    /// flag and enable the appropriate protection.
    pub fn set_secure_keyboard_entry(&mut self, enabled: bool) {
        self.secure_keyboard_entry = enabled;
    }

    /// Check if secure keyboard entry mode is enabled.
    ///
    /// Returns `true` if the UI layer should have secure input enabled.
    /// See [`set_secure_keyboard_entry`](Self::set_secure_keyboard_entry) for details.
    #[must_use]
    pub fn is_secure_keyboard_entry(&self) -> bool {
        self.secure_keyboard_entry
    }

    /// Report the host OS color scheme (light/dark) to the engine.
    ///
    /// The UI layer calls this on startup and whenever the desktop appearance
    /// changes. Apps read the current value via DSR `CSI ? 996 n`; when the value
    /// CHANGES and DEC private mode 2031 is set, the engine queues an unsolicited
    /// `CSI ? 997 ; Ps n` (1 = dark, 2 = light) for the host to drain via
    /// [`take_response`](Self::take_response) and forward to the PTY, so apps that
    /// subscribed can live-update their theme. A no-op if the scheme is unchanged.
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

    /// Enable or disable OSC 52 clipboard queries (Pd = "?").
    ///
    /// When disabled (default), aterm-core ignores OSC 52 query requests and
    /// does not invoke the clipboard callback or emit a response. This reduces
    /// clipboard exfiltration risk from untrusted output streams.
    ///
    /// Thin wrapper over [`authorize_clipboard_access`][Self::authorize_clipboard_access] /
    /// [`revoke_clipboard_access`][Self::revoke_clipboard_access] for
    /// [`super::clipboard_auth::ClipboardAccess::Query`]. The authoritative
    /// capability state lives in [`super::clipboard_auth::ClipboardAuth`];
    /// the `modes.allow_osc52_query` bool is kept in sync as a mirror for
    /// FFI/config back-compat (#7874, #7878 CF-005).
    pub fn set_osc52_query_allowed(&mut self, allowed: bool) {
        self.modes.allow_osc52_query = allowed;
        if allowed {
            self.clipboard_auth.authorize_query();
        } else {
            self.clipboard_auth.revoke_query();
        }
    }

    /// Check whether OSC 52 clipboard queries (Pd = "?") are allowed.
    ///
    /// Reads the authoritative capability state via
    /// [`super::clipboard_auth::ClipboardAuth::is_query_authorized`].
    #[must_use]
    pub fn is_osc52_query_allowed(&self) -> bool {
        self.clipboard_auth.is_query_authorized()
    }

    /// Authorize a clipboard access class (#7874, #7878 CF-004/CF-005).
    ///
    /// Grants the host's clipboard delegate structural authorization to
    /// be invoked by parser-origin clipboard sequences. `Write` covers OSC 52
    /// set/clear plus OSC 1337 `CopyToClipboard` / `EndCopy` / `Copy=`
    /// callback paths; `Query` covers OSC 52 queries. Without this, the
    /// parser path cannot reach the clipboard callback — the only way to
    /// mint a [`super::clipboard_auth::ClipboardWriteCapability`] or
    /// [`super::clipboard_auth::ClipboardQueryCapability`] is through
    /// this method's effect on [`super::clipboard_auth::ClipboardAuth`].
    ///
    /// Parallel to [`authorize_modal_protocol`][Self::authorize_modal_protocol].
    pub fn authorize_clipboard_access(&mut self, access: super::clipboard_auth::ClipboardAccess) {
        use super::clipboard_auth::ClipboardAccess;
        match access {
            ClipboardAccess::Write => {
                self.clipboard_auth.authorize_write();
                // Mirror the set bit on modes so checkpoints and FFI
                // observers stay in sync with the capability state
                // (#7782).
                self.modes.allow_osc52_set = true;
            }
            ClipboardAccess::Query => {
                self.clipboard_auth.authorize_query();
                // Mirror the query bit on modes for FFI/config back-compat.
                self.modes.allow_osc52_query = true;
            }
        }
    }

    /// Revoke a clipboard access class.
    ///
    /// Subsequent parser-origin clipboard sequences of the revoked class are
    /// silently dropped (no callback invocation, no response). Does not
    /// affect the clipboard callback itself — host-initiated clipboard
    /// operations are orthogonal.
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

    /// Whether a clipboard access class is currently authorized.
    #[must_use]
    pub fn is_clipboard_access_authorized(
        &self,
        access: super::clipboard_auth::ClipboardAccess,
    ) -> bool {
        use super::clipboard_auth::ClipboardAccess;
        match access {
            ClipboardAccess::Write => self.clipboard_auth.is_write_authorized(),
            ClipboardAccess::Query => self.clipboard_auth.is_query_authorized(),
        }
    }

    /// Authorize OSC 133 / OSC 633 shell integration with a 32-byte
    /// capability nonce (#7937 F01-2, #7960).
    ///
    /// After this call, when
    /// [`TerminalModes::require_shell_integration_nonce`] is set, OSC
    /// 133 A/B/C/D and OSC 633 A/B/C/D/E/F/G/H/P sequences that carry
    /// an `id=<64-hex>` parameter whose hex bytes equal `nonce` will
    /// dispatch normally. Sequences with no `id=`, a malformed `id=`,
    /// or the wrong nonce are silently dropped (no callback, no state
    /// transition, no response). The comparison is constant-time, so
    /// an attacker cannot recover the nonce through timing.
    ///
    /// The host is expected to:
    /// 1. Generate `nonce` with a CSPRNG at session init.
    /// 2. Call this method to install it here.
    /// 3. Inject the same value into the spawned shell's environment
    ///    (e.g. `ATERM_SHELL_NONCE`) so the shell-integration preamble
    ///    can emit `id=<hex>` on every OSC 133/633 sequence.
    /// 4. Set `modes.require_shell_integration_nonce = true` once the
    ///    preamble is in place. Until that bit is set, the nonce check
    ///    is bypassed and OSC 133/633 remains spoofable (matches the
    ///    default-off landing posture, parallel to
    ///    [`allow_palette_reconfigure`][aterm_types::TerminalModes]).
    ///
    /// Parallel to [`authorize_modal_protocol`][Self::authorize_modal_protocol].
    pub fn authorize_shell_integration(&mut self, nonce: [u8; 32]) {
        self.shell_integration_auth.authorize(nonce);
    }

    /// Revoke the shell-integration nonce (#7937 F01-2, #7960).
    ///
    /// Subsequent OSC 133 / OSC 633 sequences, under
    /// `modes.require_shell_integration_nonce`, can no longer pass the
    /// capability check until [`authorize_shell_integration`] is called
    /// again with a fresh nonce.
    pub fn revoke_shell_integration(&mut self) {
        self.shell_integration_auth.revoke();
    }

    /// Number of OSC 133 / OSC 633 sequences silently dropped by the
    /// capability-nonce gate since terminal construction (#7937 F01-2,
    /// #7960).
    ///
    /// Incremented by every `verify_nonce` call that fails because:
    /// - no nonce has been authorized yet, or
    /// - no `id=<hex>` parameter is present, or
    /// - `id=` decoded to the wrong nonce or malformed hex.
    ///
    /// Only counts drops that the enforcement gate actually executed —
    /// when `modes.require_shell_integration_nonce` is `false`, OSC
    /// 133/633 dispatches freely and this counter does not change.
    #[must_use]
    pub fn shell_integration_dropped_count(&self) -> u64 {
        self.shell_integration_auth.dropped_count()
    }

    /// Enable or disable OSC 133 / OSC 633 capability-nonce enforcement
    /// (#7937 F01-2, #7960).
    ///
    /// Thin wrapper over
    /// `modes.require_shell_integration_nonce`. When `true`, every OSC
    /// 133/633 A/B/C/D (and 633 E/F/G/H/P) must carry a matching
    /// `id=<64-hex>` — see [`authorize_shell_integration`]. When
    /// `false` (the default), dispatch proceeds unchanged for backward
    /// compatibility with unnonced shell integrations.
    ///
    /// Preserved across RIS (`\x1Bc`) and [`Terminal::reset`]: a rogue
    /// program cannot re-enable unauthenticated shell integration by
    /// issuing a full reset. See `reset.rs`.
    pub fn set_require_shell_integration_nonce(&mut self, required: bool) {
        self.modes.require_shell_integration_nonce = required;
    }

    /// Whether OSC 133 / OSC 633 capability-nonce enforcement is
    /// currently enabled (#7937 F01-2, #7960).
    #[must_use]
    pub fn is_require_shell_integration_nonce(&self) -> bool {
        self.modes.require_shell_integration_nonce
    }

    /// Enable or disable OSC 9 / 99 / 777 desktop notifications
    /// (#7878 CF-009, #7918).
    ///
    /// When disabled (the post-#7918 default), the OSC 9 / 99 / 777
    /// notification handlers in `handler_osc_notify.rs` return early
    /// before invoking any callback. Host applications that have wired a
    /// notification callback MUST call this with `allowed = true` to
    /// re-enable dispatch — wiring the callback alone is insufficient.
    ///
    /// # Authorization gate (#7878 CF-009)
    ///
    /// `modes.allow_notifications` is the single source of truth for
    /// notification dispatch: the OSC 9 / 99 / 777 handlers read it
    /// directly. This method is the low-level setter; the preferred host
    /// API is [`Self::authorize_notifications`] /
    /// [`Self::revoke_notifications`], which set the same bool.
    ///
    /// Mirror of the `allow_osc52_query` pattern:
    /// [`Self::authorize_notifications`] /
    /// [`Self::revoke_notifications`] are the preferred host API and
    /// are exposed through FFI as
    /// `aterm_terminal_set_allow_notifications_v2`. The bool is
    /// preserved across `Terminal::reset()` and RIS (`\x1Bc`) so a
    /// rogue program cannot re-enable notifications by issuing a full
    /// reset. See `reset.rs` and `handler_esc.rs::reset_terminal_state`.
    pub fn set_allow_notifications(&mut self, allowed: bool) {
        self.modes.allow_notifications = allowed;
    }

    /// Whether OSC 9 / 99 / 777 desktop notifications are currently
    /// authorized by the host (#7878 CF-009, #7918).
    #[must_use]
    pub fn is_allow_notifications(&self) -> bool {
        self.modes.allow_notifications
    }

    /// Authorize OSC 9 / OSC 99 / OSC 777 desktop notifications
    /// (#7878 CF-009).
    ///
    /// Grants the host's notification delegate authorization to be
    /// invoked when a PTY-origin OSC 9 / OSC 99 / OSC 777 sequence
    /// completes. Without this, the OSC 9 / 99 / 777 handlers in
    /// `handler_osc_notify.rs` return early before reaching the
    /// notification callback. Sets `modes.allow_notifications = true`
    /// (the bool is also used by FFI / config / checkpoint code paths).
    ///
    /// The pending-notification cap is orthogonal to this authorization
    /// and remains enforced regardless of authorization state. (The
    /// former notification rate-limiter (#7138) was deleted as inert —
    /// see `grouped_state.rs` and git history.)
    pub fn authorize_notifications(&mut self) {
        self.modes.allow_notifications = true;
    }

    /// Revoke OSC 9 / OSC 99 / OSC 777 desktop notification
    /// authorization (#7878 CF-009).
    ///
    /// Subsequent PTY-origin notification sequences return early at the
    /// authorization gate in `handle_osc_9` / `handle_osc_99` /
    /// `handle_osc_777` before any callback invocation — no
    /// notification is emitted. Does not affect the notification
    /// callbacks themselves; host-initiated paths outside OSC 9/99/777
    /// (if any) remain orthogonal.
    ///
    /// Sets `modes.allow_notifications = false`.
    pub fn revoke_notifications(&mut self) {
        self.modes.allow_notifications = false;
    }

    /// Whether OSC 9 / OSC 99 / OSC 777 desktop-notification callback
    /// invocation is currently authorized (#7878 CF-009).
    ///
    /// Reads `modes.allow_notifications`, returning the same value as
    /// [`Self::is_allow_notifications`]. Set via
    /// [`Self::authorize_notifications`] /
    /// [`Self::revoke_notifications`] / [`Self::set_allow_notifications`].
    #[must_use]
    pub fn is_notifications_authorized(&self) -> bool {
        self.modes.allow_notifications
    }

    /// Enable or disable OSC 133 / OSC 633 shell-integration recording
    /// into `SessionMemory` (#7878 CF-010).
    ///
    /// Host policy bit retained for config / checkpoint back-compat.
    /// The session-memory recording sinks themselves are permanently
    /// compiled out (the `aterm-memory` integration is not in the
    /// workspace), so this bit currently gates nothing at runtime.
    pub fn set_allow_session_memory(&mut self, allowed: bool) {
        self.modes.allow_session_memory = allowed;
    }

    /// Whether OSC 133 / OSC 633 → `SessionMemory` recording is
    /// currently permitted by the host (#7878 CF-010).
    #[must_use]
    pub fn is_allow_session_memory(&self) -> bool {
        self.modes.allow_session_memory
    }

    /// Authorize OSC 133 / OSC 633 → session-memory recording
    /// (#7878 CF-010).
    ///
    /// Sets `modes.allow_session_memory = true`. The recording sinks
    /// themselves are permanently compiled out, so this is a policy
    /// bit with no runtime consumer. Parallel to
    /// [`authorize_notifications`][Self::authorize_notifications].
    pub fn authorize_session_memory(&mut self) {
        self.modes.allow_session_memory = true;
    }

    /// Revoke OSC 133 / OSC 633 → session-memory recording
    /// authorization (#7878 CF-010).
    ///
    /// Sets `modes.allow_session_memory = false` (policy bit only; the
    /// recording sinks are permanently compiled out).
    pub fn revoke_session_memory(&mut self) {
        self.modes.allow_session_memory = false;
    }

    /// Whether OSC 133 / OSC 633 → `SessionMemory` recording is
    /// currently authorized (#7878 CF-010).
    ///
    /// Reads the authoritative capability state. Should return the
    /// same value as [`Self::is_allow_session_memory`] thanks to the
    /// mirror sync.
    #[must_use]
    pub fn is_session_memory_authorized(&self) -> bool {
        self.modes.allow_session_memory
    }

    /// Mint an EXTRA OSC 8 URI scheme onto the safe allowlist (orca
    /// deep-links §7, #4384) — the ONE hyperlink decision a host makes.
    ///
    /// OSC 8 acceptance itself is not a host switch: a URI that clears
    /// the byte cap, the control-character and BiDi-override scans and
    /// the scheme allowlist is carried, on every host, always. See
    /// [`super::hyperlink_auth`] for why, and for where the decision a
    /// PERSON makes lives (the host's click-time URL check).
    ///
    /// Returns `false` — refusing, state unchanged — when `scheme` is
    /// over-long (> 32 bytes), not RFC 3986 scheme-shaped
    /// (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`), in the never-allow set
    /// (`javascript`/`data`/`file`/`vbscript`/`about`/`blob`, case- and
    /// suffix-insensitively), or when 4 extra schemes are already live.
    /// Returns `true` when stored (idempotent for a live scheme). Stored
    /// lowercased; OSC-8 matching is case-insensitive like the safe list.
    ///
    /// Every ORTHOGONAL OSC-8 guard still applies to extra-scheme URIs — the
    /// byte cap, control-char scan and BiDi-override rejection — so minting a
    /// scheme never widens anything but the scheme comparison.
    /// Extra schemes are TERMINAL-INSTANCE state: they survive soft/RIS reset
    /// like the other host authorizations and are excluded from checkpoints.
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

    /// Remove a host-minted extra scheme (case-insensitive), restoring the
    /// default allowlist posture for it: subsequent OSC 8 URIs with that
    /// scheme are refused at parse time again. Absent schemes are a no-op.
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

    /// Whether `scheme` is currently a live host-minted extra scheme
    /// (case-insensitive). Introspection for hosts and the Tier-1
    /// conformance projection; the hardcoded safe allowlist is NOT consulted.
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

    /// Authorize raw DCS callback delivery (#8009 CF-013).
    ///
    /// Grants the DCS unhook handler structural authorization to
    /// forward the PTY-origin DCS payload to the registered
    /// `DcsCallback`. The zero-sized
    /// [`super::dcs_auth::DcsEmitCapability`] can only be minted while
    /// the underlying [`super::dcs_auth::DcsAuth`] is authorized.
    ///
    /// **Default is authorized** — the pre-refactor behavior was
    /// unconditional callback invocation. Hosts shipping a hardened
    /// profile can call [`revoke_dcs`][Self::revoke_dcs] after
    /// construction to drop raw DCS payloads at the capability gate.
    ///
    /// Does **not** affect per-DCS-type handlers (DECRQSS, Sixel,
    /// DECDLD, tmux / conductor token activation, XTGETTCAP). Those
    /// paths run before the callback and are gated by their own
    /// capabilities (e.g. `modal_protocol_auth`).
    pub fn authorize_dcs(&mut self) {
        self.dcs_auth.authorize();
    }

    /// Revoke raw DCS callback delivery (#8009 CF-013).
    ///
    /// Subsequent PTY-origin DCS completions fail at the capability
    /// gate — `invoke_dcs_callback` is unreachable without a minted
    /// token, and the registered `DcsCallback` is never invoked. This
    /// is the coarse "deliver raw DCS to the host at all" switch; per-
    /// DCS-type handlers (DECRQSS / Sixel / tmux / conductor / etc.)
    /// continue to run under their own capability gates.
    pub fn revoke_dcs(&mut self) {
        self.dcs_auth.revoke();
    }

    /// Whether raw DCS callback delivery is currently authorized
    /// (#8009 CF-013).
    ///
    /// Defaults to `true` on newly constructed `Terminal` instances.
    #[must_use]
    pub fn is_dcs_authorized(&self) -> bool {
        self.dcs_auth.is_authorized()
    }

    /// Install or replace the OSC / escape-sequence policy engine (#7996).
    ///
    /// Called by the FFI shim `aterm_terminal_apply_policy` after parsing a TOML
    /// policy document, and by the GUI at startup. The stored engine is LIVE and
    /// ENFORCING: the handler-side wiring consults
    /// `policy_engine.evaluate(sequence, origin)` at the OSC 52 / XTWINOPS /
    /// response / rate-limit gates (see `policy_bridge.rs`), so installing an
    /// engine changes behavior — it ADDS enforcement on top of the legacy
    /// `authorize_*` bits, never widening it.
    pub fn apply_policy_engine(&mut self, engine: aterm_policy::engine::PolicyEngine) {
        // `install` recompiles the constant-probe gate table in the same step,
        // so no gate can answer from the policy this call replaced.
        self.policy.install(engine);
    }

    /// Borrow the currently installed policy engine, if any (#7996).
    #[must_use]
    pub fn policy_engine(&self) -> Option<&aterm_policy::engine::PolicyEngine> {
        self.policy.engine()
    }

    /// Clear the installed policy engine (#7996).
    ///
    /// Leaves the terminal in its legacy `TerminalModes::allow_*`-only
    /// behavior. Primarily useful for tests and checkpoint restore
    /// (#7997 will serialize the engine so this path stays explicit).
    pub fn clear_policy_engine(&mut self) {
        // `clear` recompiles the gate table back to the legacy posture in the
        // same step — see `apply_policy_engine`.
        self.policy.clear();
    }

    /// Enable or disable CSI t XTWINOPS window manipulation (#7139).
    ///
    /// When disabled (the fail-closed default), every CSI t subcommand in
    /// the 1-10 state-change / geometry range is silently ignored at the
    /// capability gate — no window resize, move, raise, lower, iconify,
    /// etc. Host applications that want xterm-compatible permissive
    /// behaviour MUST call this with `allowed = true` to opt in.
    ///
    /// This is the FFI-facing counterpart to
    /// `modes_mut().allow_window_ops = allowed`; both paths set the same
    /// authoritative bit on [`TerminalModes`]. The bool is preserved
    /// across `Terminal::reset()` and RIS so a rogue program cannot
    /// re-enable window ops through a full reset.
    pub fn set_allow_window_ops(&mut self, allowed: bool) {
        self.modes.allow_window_ops = allowed;
    }

    /// Whether CSI t XTWINOPS window manipulation is currently allowed
    /// (#7139).
    #[must_use]
    pub fn is_allow_window_ops(&self) -> bool {
        self.modes.allow_window_ops
    }

    /// Enable or disable OSC 4 / OSC 21 indexed palette reconfigure
    /// (#7937 F01-3).
    ///
    /// When disabled (the fail-closed default), PTY-origin OSC 4 and
    /// OSC 21 sequences that attempt to change the 256-entry indexed
    /// palette are silently ignored — no palette mutation, no callback
    /// invocation. Host applications that want the classic xterm
    /// permissive posture MUST call this with `allowed = true` to opt in.
    ///
    /// This is the FFI-facing counterpart to
    /// `modes_mut().allow_palette_reconfigure = allowed`; both paths set
    /// the same authoritative bit on [`TerminalModes`]. The bool is
    /// preserved across `Terminal::reset()` and RIS so a rogue program
    /// cannot re-enable palette reconfigure by issuing a full reset.
    pub fn set_allow_palette_reconfigure(&mut self, allowed: bool) {
        self.modes.allow_palette_reconfigure = allowed;
    }

    /// Whether OSC 4 / OSC 21 indexed palette reconfigure is currently
    /// allowed (#7937 F01-3).
    #[must_use]
    pub fn is_allow_palette_reconfigure(&self) -> bool {
        self.modes.allow_palette_reconfigure
    }

    /// Get cursor position.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        self.grid.cursor()
    }

    /// Monotonic autowrap serial of the ACTIVE grid — the emulator wrap fact
    /// (kitty-motion §4.1). Advances by one each time an autowrap line
    /// advance actually resolves (`Grid::advance_autowrap_line`, the one
    /// funnel); hard newlines never advance it. A host diffs it against a
    /// per-window `(session, last-serial)` baseline read under the SAME lock
    /// as [`Self::cursor`], so "the serial moved since my last read of this
    /// session" is the fact that separates a wrap — margin, scrolled
    /// bottom-row, or margined — from `Home` pressed at the last column.
    ///
    /// Transient observability, deliberately unmigrated (the law lives on
    /// `GridStorage::wrap_serial`): checkpoint/restore omits it and the
    /// main/alt swaps in `handler_dec` leave each grid its own serial, so a
    /// restore or buffer switch costs at most one spurious changed/unchanged
    /// read — which the host's same-session baseline diff tolerates.
    #[must_use]
    pub fn wrap_serial(&self) -> u64 {
        self.grid.wrap_serial()
    }

    /// Check if cursor is visible.
    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.modes.cursor_visible
    }

    /// The VIEWPORT row the cursor projects onto, on screen or not.
    ///
    /// The DEC cursor is anchored in the ACTIVE grid; the viewport row it
    /// occupies is its active-grid row pushed DOWN by the scrollback offset,
    /// because older history scrolls in at the top and slides live content —
    /// the cursor's row included — toward the bottom.
    #[must_use]
    pub fn projected_cursor_row(&self) -> usize {
        (self.cursor().row as usize).saturating_add(self.grid().display_offset())
    }

    /// The viewport row the cursor OCCUPIES in a `rows`-tall frame, or `None`
    /// when nothing is drawn there: DECTCEM hid it, or a scroll-back pushed its
    /// projected row off the bottom.
    ///
    /// THE ONE PROJECTION. `cell_frame_into` decides `cursor_visible` with this,
    /// so a host asking "where is this terminal's caret in the frame I am
    /// looking at" gets the extraction's own answer instead of a second copy of
    /// the arithmetic that can drift from it.
    #[must_use]
    pub fn cursor_row_on_screen(&self, rows: usize) -> Option<usize> {
        let row = self.projected_cursor_row();
        (self.cursor_visible() && row < rows).then_some(row)
    }

    /// DECSCNM (DEC private mode 5) reverse-video: the whole-screen fg/bg swap is
    /// applied at colour-resolve time, so it changes the RENDERED frame without
    /// touching any cell's content (it bumps `damage_epoch`, not `content_seq`). A
    /// read-only accessor so an introspection consumer (the subscribe render
    /// signature) can notice a DECSCNM flip that the content-seq gate would miss.
    #[must_use]
    pub fn reverse_video(&self) -> bool {
        self.modes.reverse_video
    }

    /// Monotonic REPAINT-BLINK epoch: how many DECTCEM hides (`CSI ?25l`)
    /// have been processed WHILE DEC-2026 synchronized output was active —
    /// the hide-inside-sync bracket modern full-redraw TUIs (Claude Code)
    /// wrap around every keystroke's repaint. Hosts diff this against a
    /// last-seen value to classify "the attached app repaints per keystroke"
    /// without any keyboard-protocol inference; vim/less (hide, no sync) and
    /// ConPTY (hide per echo, no sync) never advance it. A read-only display
    /// projection like [`Self::content_seq`]; never reset, never gates bytes.
    #[must_use]
    pub fn repaint_blink_epoch(&self) -> u64 {
        self.repaint_blink_epoch
    }

    /// Get current cursor style.
    #[must_use]
    pub fn cursor_style(&self) -> CursorStyle {
        self.modes.cursor_style
    }

    /// Set the host-preferred DEFAULT cursor style — the shape used before any
    /// DECSCUSR and restored on RIS/DECSTR reset. Unlike a render override, this does
    /// NOT clobber an app's live DECSCUSR (e.g. vim insert-mode bar): the live style
    /// only follows the new default while the app has not taken control (the
    /// `was_default` guard), so the preference takes effect immediately at startup yet
    /// yields to the app afterward. Fires the cursor-style callback if the live style
    /// changed, matching DECSCUSR.
    pub fn set_default_cursor_style(&mut self, style: CursorStyle) {
        let was_default = self.modes.cursor_style == self.default_cursor_style;
        self.default_cursor_style = style;
        if was_default && self.modes.cursor_style != style {
            self.modes.cursor_style = style;
            if let Some(callback) = self.cursor_style_callback.as_mut() {
                callback(style);
            }
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

    /// Get the terminal size as a [`TerminalSize`].
    #[must_use]
    pub fn size(&self) -> TerminalSize {
        TerminalSize::new(self.grid.rows(), self.grid.cols())
    }

    /// Take a snapshot of the current terminal state.
    ///
    /// This captures essential state for diagnostics or comparison,
    /// returning a lightweight struct that can be stored or inspected.
    #[must_use]
    pub fn snapshot(&self) -> TerminalSnapshot {
        let scrollback_lines = self
            .grid
            .scrollback()
            .map_or(0, ScrollbackStorage::line_count);

        TerminalSnapshot {
            cursor_row: self.grid.cursor().row,
            cursor_col: self.grid.cursor().col,
            cols: self.grid.cols(),
            rows: self.grid.rows(),
            title: Arc::clone(&self.title.window),
            current_working_directory: self.current_working_directory.clone(),
            // The alt buffer persists in `alt_grid` while the main screen is
            // active, so slot occupancy no longer implies "alt screen active".
            alternate_screen_active: self.modes.alternate_screen,
            origin_mode: self.modes.origin_mode,
            insert_mode: self.modes.insert_mode,
            cursor_visible: self.modes.cursor_visible,
            cursor_style: self.modes.cursor_style,
            total_scrollback_lines: scrollback_lines,
        }
    }

    /// Get pipeline timestamps from the last `process()` call (#5560).
    ///
    /// Returns per-stage durations (parse, grid, total) measured during VT
    /// processing. The render snapshot reads this to carry Rust-side timing
    /// through FFI to Swift for end-to-end latency decomposition.
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

#[cfg(test)]
impl Terminal {
    /// Get the character set state.
    #[must_use]
    pub fn charset(&self) -> &CharacterSetState {
        &self.charset
    }

    /// Get a mutable reference to the character set state.
    pub fn charset_mut(&mut self) -> &mut CharacterSetState {
        &mut self.charset
    }
}

#[cfg(test)]
use super::TITLE_STACK_MAX_DEPTH;
use super::TerminalModes;

impl Terminal {
    /// Get a mutable reference to the terminal modes.
    pub fn modes_mut(&mut self) -> &mut TerminalModes {
        &mut self.modes
    }

    /// Get a mutable reference to the current style.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "white-box accessor consumed by the un-wired checkpoint/session test suites"
    )]
    pub(crate) fn style_mut(&mut self) -> &mut CurrentStyle {
        &mut self.style
    }

    /// Get saved cursor state for main screen.
    #[cfg(test)]
    #[must_use]
    #[allow(
        dead_code,
        reason = "white-box accessor consumed by the un-wired checkpoint/session test suites"
    )]
    pub(crate) fn saved_cursor_main(&self) -> Option<&SavedCursorState> {
        self.cursor_save.main.as_ref()
    }

    /// Get saved cursor state for alt screen.
    #[cfg(test)]
    #[must_use]
    #[allow(
        dead_code,
        reason = "white-box accessor consumed by the un-wired checkpoint/session test suites"
    )]
    pub(crate) fn saved_cursor_alt(&self) -> Option<&SavedCursorState> {
        self.cursor_save.alt.as_ref()
    }

    /// Set saved cursor state for main screen.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "white-box accessor consumed by the un-wired checkpoint/session test suites"
    )]
    pub(crate) fn set_saved_cursor_main(&mut self, cursor: Option<SavedCursorState>) {
        self.cursor_save.main = cursor;
    }

    /// Set saved cursor state for alt screen.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "white-box accessor consumed by the un-wired checkpoint/session test suites"
    )]
    pub(crate) fn set_saved_cursor_alt(&mut self, cursor: Option<SavedCursorState>) {
        self.cursor_save.alt = cursor;
    }

    /// Get the title stack.
    #[cfg(test)]
    #[must_use]
    pub fn title_stack(&self) -> &[(Arc<str>, Arc<str>)] {
        &self.title.stack
    }

    /// Set the title stack.
    ///
    /// Truncates to [`TITLE_STACK_MAX_DEPTH`] for consistency with `push_title()`.
    #[cfg(test)]
    pub fn set_title_stack(&mut self, stack: Vec<(Arc<str>, Arc<str>)>) {
        let len = stack.len().min(TITLE_STACK_MAX_DEPTH);
        self.title.stack = stack.into_iter().take(len).collect();
    }

    /// Set the icon name.
    ///
    /// Names longer than [`MAX_TITLE_BYTES`] are truncated at a char boundary.
    #[cfg(test)]
    pub fn set_icon_name(&mut self, name: &str) {
        let name = &name[..name.floor_char_boundary(MAX_TITLE_BYTES)];
        self.title.icon = name.into();
    }

    /// Set a hyperlink URL.
    ///
    /// Convenience method that takes a string slice.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "white-box accessor consumed by the un-wired checkpoint/session test suites"
    )]
    pub(crate) fn set_hyperlink(&mut self, url: Option<&str>) {
        self.transient.current_hyperlink = url.map(Arc::from);
        self.transient.update_has_transient_extras();
    }

    /// Set the underline color.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "white-box accessor consumed by the un-wired checkpoint/session test suites"
    )]
    pub(crate) fn set_underline_color(&mut self, color: Option<u32>) {
        self.transient.current_underline_color = color;
        self.transient.update_has_transient_extras();
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
