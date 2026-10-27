// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! State accessors for `Terminal`: keyboard, cursor, modes, title and the security authorizations.

use super::{
    CurrentStyle, Cursor, CursorStyle, KittyKeyboardFlags, MAX_TITLE_BYTES, Terminal,
    XtermKeyboardState,
};
use crate::grid::ResizePolicy;
use aterm_types::PipelineTimestamps;
use std::sync::Arc;

/// Where the reported colour scheme (DSR 996 / mode 2031) comes from ([`Terminal::color_scheme_source`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSchemeSource {
    /// The live default background is the host-configured one: the host's baseline appearance (`Terminal::set_color_scheme` / `TerminalConfig::color_scheme`).
    Configured,
    /// A program moved the default background (OSC 11/21, a colour-stack pop, a handoff replay): that background's polarity.
    Program,
}

/// Production accessors used by external crates and FFI.
impl Terminal {
    /// Get the current Kitty keyboard enhancement flags.
    #[inline]
    #[must_use]
    pub const fn kitty_keyboard_flags(&self) -> KittyKeyboardFlags {
        self.kitty_keyboard.flags()
    }

    /// Enable or disable the Kitty keyboard protocol (default on), for embedders whose platform consumes kitty sequences itself (Windows ConPTY).
    #[inline]
    pub fn set_kitty_keyboard_enabled(&mut self, enabled: bool) {
        self.modes.kitty_keyboard_enabled = enabled;
        self.refresh_mode_mirror();
    }

    /// Declare whether this session sits behind a Windows console host (ConPTY), whose `CSI ? 9001 h` asks for win32-input-mode key records (default off).
    #[inline]
    pub fn set_win32_input_enabled(&mut self, enabled: bool) {
        self.modes.win32_input_enabled = enabled;
        self.refresh_mode_mirror();
    }

    /// Declare whether the console host behind win32-input-mode is OpenConsole rather than Windows' inbox conhost (default off): the two read KP_Begin and F13-F20 differently (`KeyboardMode::OPENCONSOLE_HOST`).
    #[inline]
    pub fn set_win32_input_openconsole(&mut self, openconsole: bool) {
        self.modes.win32_input_openconsole = openconsole;
        self.refresh_mode_mirror();
    }

    /// Set the keyboard capabilities a console host decides from the same fact that picks the resize seam policy, so the two cannot disagree.
    #[inline]
    pub fn set_console_host_posture(&mut self, policy: ResizePolicy) {
        let console_host = policy == ResizePolicy::ConPty;
        self.modes.win32_input_enabled = console_host;
        self.modes.kitty_keyboard_enabled = !console_host;
        self.refresh_mode_mirror();
    }

    /// Host knowledge, before the bytes it feeds next: whether a program other than the shell holds the terminal's foreground while it wrote them (a unix host compares the foreground process group it read for those bytes with the shell's pid; a host with no foreground group says `false`).
    pub fn set_foreground_job(&mut self, job: bool) {
        let prev = (self.shell.foreground_job, self.shell.probe_job_active);
        self.shell.foreground_job = job;
        let mut restored = false;
        if job {
            self.shell.probe_job_active = false;
        } else if self.shell.cwd_before_job.is_some() {
            let running = self.shell.program_runs_since_prompt();
            self.shell.probe_job_active = running;
            if !running && let Some(before) = self.shell.cwd_before_job.take() {
                restored = true;
                let (_, mut handler) = self.split_for_process();
                handler.apply_cwd_report(before);
            }
        } else {
            self.shell.probe_job_active = false;
        }
        let after = (self.shell.foreground_job, self.shell.probe_job_active);
        if after != prev || restored {
            self.host_journal.push(super::HostOp::ForegroundJob {
                active: after.0,
                probed: after.1,
                restore_cwd: restored,
            });
        }
    }

    /// Install the host's program probe: given a wall-clock instant (unix epoch ms), whether a program the shell started at or after it runs now where the foreground word ([`Self::set_foreground_job`]) cannot see it.
    #[inline]
    pub fn set_program_probe(&mut self, probe: Option<Box<dyn Fn(u64) -> bool + Send>>) {
        self.shell.program_probe = probe;
    }

    /// Host policy: the console host between this terminal and its app forwards the app's alternate-screen switches verbatim and never repaints the buffer it switches to (Windows Terminal's OpenConsole).
    #[inline]
    pub const fn set_host_resyncs_alt_screen(&mut self, on: bool) {
        self.transient.host_resyncs_alt_screen = on;
        if !on {
            self.transient.alt_screen_resync_requested = false;
            self.transient.dead_alt_screen = None;
        }
    }

    /// The embedder is about to make its console host leave the alternate screen a dead app left up (the `mainscreen` verb under the host policy, [`set_host_resyncs_alt_screen`](Self::set_host_resyncs_alt_screen)): the `?1049l` the host forwards then leaves it as a dead app's screen, releasing the modes the app left on as a prompt's request does, and a prompt on that screen asks the host nothing more.
    #[inline]
    pub const fn expect_host_alt_screen_leave(&mut self) -> bool {
        if !self.modes.alternate_screen {
            return false;
        }
        let screen = Some(self.transient.screen_replaced);
        self.transient.dead_alt_screen = screen;
        self.transient.alt_screen_resync_screen = screen;
        self.host_journal.push(super::HostOp::ExpectHostAltLeave);
        true
    }

    /// Take the pending alternate-screen resync request (see [`set_host_resyncs_alt_screen`](Self::set_host_resyncs_alt_screen)): `true` when the embedder should make its console host leave the alternate buffer, once per alternate-screen visit: not again until the alternate screen is left (by the host's forwarded `?1049l` or the program's own) or the embedder gives the request back ([`abandon_alt_screen_resync`](Self::abandon_alt_screen_resync)), however many screens a program enters with a prompt mark on each meanwhile. A program that leaves and re-enters asks once per visit, so the bound on host clients is the embedder's (aterm-pty runs one leave client per session at a time).
    #[inline]
    pub fn take_alt_screen_resync_request(&mut self) -> bool {
        std::mem::take(&mut self.transient.alt_screen_resync_requested)
    }

    /// Give back the request [`take_alt_screen_resync_request`](Self::take_alt_screen_resync_request) took, when the embedder's leave client did not start: the visit is no longer asked for, so the next prompt mark on the alternate screen asks again.
    #[inline]
    pub const fn abandon_alt_screen_resync(&mut self) {
        self.transient.alt_screen_resync_screen = None;
    }

    /// Whether the Kitty keyboard protocol capability is enabled (see [`set_kitty_keyboard_enabled`](Self::set_kitty_keyboard_enabled)).
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn is_kitty_keyboard_enabled(&self) -> bool {
        self.modes.kitty_keyboard_enabled
    }

    /// Get the xterm keyboard modifier/format state (XTMODKEYS/XTFMTKEYS).
    #[inline]
    #[must_use]
    pub const fn xterm_keyboard(&self) -> &XtermKeyboardState {
        &self.xterm_keyboard
    }

    /// Get the current style.
    #[inline]
    #[must_use]
    pub const fn style(&self) -> &CurrentStyle {
        &self.style
    }

    /// Set the window title, truncated at a char boundary to `MAX_TITLE_BYTES`.
    pub fn set_title(&mut self, title: &str) {
        let title = &title[..title.floor_char_boundary(MAX_TITLE_BYTES)];
        if &*self.title.window != title {
            self.title
                .epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.title.window = title.into();
    }

    /// Get the window title.
    #[inline]
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title.window
    }

    /// The monotonic count of bells that fired (past the throttle).
    #[inline]
    #[must_use]
    pub const fn bell_total(&self) -> u64 {
        self.bell_total
    }

    /// The monotonic count of desktop notifications the program asked for (OSC 9, a final OSC 99 chunk, OSC 777 `notify`), whether or not notifications are allowed: the session log counts them (`mark - notify n=`), never their text.
    #[inline]
    #[must_use]
    pub const fn notify_total(&self) -> u64 {
        self.notify_total
    }

    /// The window title as a shared [`Arc<str>`] handle: a reference-count bump, so callers can hold the title without copying or borrowing the terminal.
    #[inline]
    #[must_use]
    pub fn title_arc(&self) -> std::sync::Arc<str> {
        Arc::clone(&self.title.window)
    }

    /// Monotonic change epoch for the window title (OSC 0/2 or [`set_title`](Self::set_title))
    /// and stored shell-reported cwd path/host (OSC 7 / OSC 633 `P;Cwd=`).
    /// Cwd changes advance it even while an explicit title is set; identical cwd reports do not.
    #[inline]
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
    #[cfg(test)]
    pub const fn set_secure_keyboard_entry(&mut self, enabled: bool) {
        self.secure_keyboard_entry = enabled;
    }

    /// Set the host's baseline colour scheme: the appearance of the host-configured default background (a theme's declared appearance).
    pub fn set_color_scheme(&mut self, baseline: aterm_types::Appearance) {
        self.color.configured_color_scheme = baseline;
        let _ = self.reconcile_color_scheme();
    }

    /// The colour scheme DSR 996 answers now: the background on screen.
    #[inline]
    #[must_use]
    pub fn color_scheme(&self) -> aterm_types::Appearance {
        self.color.reported_scheme().0
    }

    /// Where [`color_scheme`](Self::color_scheme) comes from.
    #[inline]
    #[must_use]
    pub fn color_scheme_source(&self) -> ColorSchemeSource {
        self.color.reported_scheme().1
    }

    /// [`Self::reconcile_color_scheme`] at the end of a process batch, where the change came from the program's own output (OSC 10/11/110/111, OSC 21, a colour-stack pop, RIS, DECSTR).
    pub(super) fn reconcile_color_scheme_after_output(&mut self) {
        const PUSH_LEN: usize = b"\x1b[?997;1n".len();
        if self.modes.color_scheme == self.color.reported_scheme().0 {
            return;
        }
        if self.modes.report_color_scheme
            && !super::handler::program_response_admitted(
                &mut self.policy,
                &mut self.transient,
                PUSH_LEN,
            )
        {
            return;
        }
        let _ = self.reconcile_color_scheme();
    }

    /// Bring the announced latch (`TerminalModes::color_scheme`) to the derived scheme, queueing the `CSI ? 997 ; Ps n` push when it moved and DEC 2031 is set.
    pub(super) fn reconcile_color_scheme(&mut self) -> bool {
        const PUSH_MAX_LEN: usize = 16;
        let now = self.color.reported_scheme().0;
        if self.modes.color_scheme == now {
            return false;
        }
        self.modes.color_scheme = now;
        if self.modes.report_color_scheme {
            use std::fmt::Write as _;
            let mut response = super::stack_response::StackResponse::<PUSH_MAX_LEN>::new();
            let _ = write!(response, "\x1b[?997;{}n", now.dsr_code());
            let _ = self.transient.push_report(response.as_bytes());
        }
        true
    }

    /// Whether an OSC 52 query the program sends now reaches the host's clipboard callback: one is installed and the query gate opens (authorized, or allowed by the installed policy). aterm-gui's `host_io_prefetch` reads the pasteboard, before it takes the terminal lock, only when this says yes.
    #[inline]
    #[must_use]
    pub fn clipboard_query_admitted(&self) -> bool {
        self.clipboard.callback.is_some()
            && self
                .clipboard_auth
                .try_mint_query_capability_with_engine(self.policy.gates())
                .is_some()
    }

    /// Whether a Kitty non-direct medium resolver is installed ([`Self::set_kitty_file_resolver`]), i.e. whether a `t=f`/`t=t`/`t=s` transmission reaches the host at all.
    #[inline]
    #[must_use]
    pub const fn kitty_file_resolver_installed(&self) -> bool {
        self.kitty_file_resolver.installed()
    }

    /// Authorize a clipboard access class for parser-origin sequences: `Write` covers OSC 52 set/clear and OSC 1337 `CopyToClipboard` / `EndCopy` / `Copy=`; `Query` covers OSC 52 queries.
    #[inline]
    pub const fn authorize_clipboard_access(
        &mut self,
        access: super::clipboard_auth::ClipboardAccess,
    ) {
        use super::clipboard_auth::ClipboardAccess;
        match access {
            ClipboardAccess::Write => {
                self.clipboard_auth.authorize_write();
                self.modes.allow_osc52_set = true;
            }
            ClipboardAccess::Query => {
                self.clipboard_auth.authorize_query();
                self.modes.allow_osc52_query = true;
            }
        }
    }

    /// Revoke a clipboard access class: later parser-origin sequences of that class are dropped (no callback, no response).
    #[inline]
    pub const fn revoke_clipboard_access(
        &mut self,
        access: super::clipboard_auth::ClipboardAccess,
    ) {
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

    /// Authorize OSC 133 / OSC 633 shell integration with a 32-byte capability nonce.
    #[inline]
    pub const fn authorize_shell_integration(&mut self, nonce: [u8; 32]) {
        self.shell_integration_auth.authorize(nonce);
        self.host_journal.push(super::HostOp::AuthorizeShell(nonce));
    }

    /// [`authorize_shell_integration`][Self::authorize_shell_integration] for a nonce the shell has not taken yet: the re-key of an adopted shell whose nonce did not survive an update, delivered through a file the shell reads at its next prompt (aterm-gui `shell_rekey`).
    #[inline]
    pub const fn authorize_shell_integration_on_first_mark(&mut self, nonce: [u8; 32]) {
        self.shell_integration_auth.authorize_on_first_mark(nonce);
        self.host_journal
            .push(super::HostOp::AuthorizeShellOnFirstMark(nonce));
    }

    /// [`authorize_shell_integration_on_first_mark`][Self::authorize_shell_integration_on_first_mark] for a typed re-key (aterm-gui `shell_rekey::typed`): a nonce left in a one-use file for a line typed at the prompt.
    #[inline]
    pub const fn authorize_shell_integration_rekey(&mut self, nonce: [u8; 32]) {
        self.shell_integration_auth.authorize_rekey(nonce);
        self.host_journal
            .push(super::HostOp::AuthorizeShellRekey(nonce));
    }

    /// Take back the typed re-key `nonce`, restoring the authorization it replaced.
    #[inline]
    pub fn withdraw_shell_integration_rekey(&mut self, nonce: &[u8; 32]) -> bool {
        let withdrew = self.shell_integration_auth.withdraw_rekey(nonce);
        if withdrew {
            self.host_journal
                .push(super::HostOp::WithdrawShellRekey(*nonce));
        }
        withdrew
    }

    /// Keep the typed re-key `nonce`: the shell read its file, so the authorization it replaced is forgotten.
    #[inline]
    pub fn settle_shell_integration_rekey(&mut self, nonce: &[u8; 32]) -> bool {
        let settled = self.shell_integration_auth.settle_rekey(nonce);
        if settled {
            self.host_journal
                .push(super::HostOp::SettleShellRekey(*nonce));
        }
        settled
    }

    /// The nonce the shell's marks verify with now (a re-key not yet taken is not in use), or `None`.
    #[inline]
    #[must_use]
    pub const fn shell_integration_nonce_in_use(&self) -> Option<[u8; 32]> {
        self.shell_integration_auth.nonce_in_use()
    }

    /// Number of OSC 133 / OSC 633 sequences dropped by the capability-nonce gate since construction: no nonce authorized, no `id=`, or a wrong or malformed nonce.
    #[inline]
    #[must_use]
    pub const fn shell_integration_dropped_count(&self) -> u64 {
        self.shell_integration_auth.dropped_count()
    }

    /// Number of OSC 133 / OSC 633 sequences whose capability nonce verified since construction (saturating; a pure counter, only moved while `modes.require_shell_integration_nonce` is `true`).
    #[inline]
    #[must_use]
    pub const fn shell_marks_verified(&self) -> u64 {
        self.shell_integration_auth.verified_count()
    }

    /// Number of kitty-graphics APC strings (`ESC _ G … ST`) that ended since construction, complete or cut short by CAN/SUB, whatever each did: a pure counter, never reset.
    #[inline]
    #[must_use]
    pub const fn kitty_graphics_commands(&self) -> u64 {
        self.transient.kitty_commands
    }

    /// Number of Kitty unscrolls (`CSI n + T`) run on the main screen since construction, whatever each pulled: a pure counter, never reset.
    #[inline]
    #[must_use]
    pub const fn kitty_unscrolls(&self) -> u64 {
        self.transient.kitty_unscrolls
    }

    /// Enable or disable OSC 133 / OSC 633 capability-nonce enforcement (default off): when on, every sequence must carry a matching `id=<64-hex>` (see [`Self::authorize_shell_integration`]).
    #[inline]
    pub const fn set_require_shell_integration_nonce(&mut self, required: bool) {
        self.modes.require_shell_integration_nonce = required;
    }

    /// Whether OSC 133 / OSC 633 capability-nonce enforcement is currently enabled.
    #[inline]
    #[must_use]
    pub const fn is_require_shell_integration_nonce(&self) -> bool {
        self.modes.require_shell_integration_nonce
    }

    /// Enable or disable OSC 9 / 99 / 777 desktop notifications (default off): while off, the handlers in `handler_osc_notify.rs` never reach the callback, so a host that wires one must also call this with `true`.
    #[inline]
    pub const fn set_allow_notifications(&mut self, allowed: bool) {
        self.modes.allow_notifications = allowed;
    }

    /// Authorize OSC 9 / 99 / 777 desktop notifications (`modes.allow_notifications`).
    #[cfg(test)]
    pub const fn authorize_notifications(&mut self) {
        self.modes.allow_notifications = true;
    }

    /// Add an extra OSC 8 URI scheme to the safe allowlist (see [`super::hyperlink_auth`]; what a person may open is checked by the host at click time).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "hyperlink_scheme_cap",
            action = "Authorize",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "hyperlink_scheme_cap",
            action = "AuthorizeOther",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "hyperlink_scheme_cap",
            action = "RefuseAtCap",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "hyperlink_scheme_cap",
            action = "RefuseNeverAllow",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    pub fn authorize_hyperlink_scheme(&mut self, scheme: &str) -> bool {
        self.hyperlink_auth.authorize_scheme(scheme)
    }

    /// Remove a host-minted extra scheme (case-insensitive): OSC 8 URIs with it are refused again.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "hyperlink_scheme_cap",
            action = "Revoke",
            project = "conformance_hyperlink_scheme_cap::project"
        )
    )]
    pub fn revoke_hyperlink_scheme(&mut self, scheme: &str) {
        self.hyperlink_auth.revoke_scheme(scheme);
    }

    /// Whether `scheme` is a live host-minted extra scheme (case-insensitive).
    #[inline]
    #[must_use]
    pub fn is_hyperlink_scheme_authorized(&self, scheme: &str) -> bool {
        self.hyperlink_auth
            .extra_schemes()
            .iter()
            .any(|s| s.eq_ignore_ascii_case(scheme))
    }

    /// Count of live host-minted extra schemes (Tier-1 projection aid).
    #[inline]
    #[must_use]
    pub fn hyperlink_extra_scheme_count(&self) -> usize {
        self.hyperlink_auth.extra_schemes().len()
    }

    /// Copy of live host-minted extra OSC 8 URI schemes in authorization order (lowercased).
    #[must_use]
    pub fn hyperlink_extra_schemes(&self) -> Vec<String> {
        self.hyperlink_auth
            .extra_schemes()
            .iter()
            .map(|s| (**s).to_owned())
            .collect()
    }

    /// Install or replace the OSC / escape-sequence policy engine (the GUI does so for every session before its reader runs).
    #[inline]
    pub fn apply_policy_engine(&mut self, engine: aterm_policy::engine::PolicyEngine) {
        // `install` recompiles the constant-probe gate table in the same step,
        // so no gate can answer from the policy this call replaced.
        self.policy.install(engine);
    }

    /// Enable or disable OSC 4 / OSC 21 indexed palette reconfigure (engine default off; the aterm app turns it on unless configured false): while disabled, PTY-origin attempts to change the 256-entry palette are ignored.
    #[inline]
    pub const fn set_allow_palette_reconfigure(&mut self, allowed: bool) {
        if allowed && !self.modes.allow_palette_reconfigure {
            self.color.palette_set_refused = false;
        }
        self.modes.allow_palette_reconfigure = allowed;
    }

    /// Whether programs may set the 256 indexed colours (OSC 4 / numeric OSC 21) in this session (see [`Self::set_allow_palette_reconfigure`]).
    #[inline]
    #[must_use]
    pub const fn allow_palette_reconfigure(&self) -> bool {
        self.modes.allow_palette_reconfigure
    }

    /// What programs set over the configured colours (OSC 4/10/11/12/17/19/ 21): the diff `apply_config` replays over a new theme and a handoff carries.
    #[inline]
    #[must_use]
    pub fn program_color_overrides(&self) -> super::ColorOverridesRepr {
        self.color.capture_overrides()
    }

    /// Get cursor position.
    #[inline]
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.grid.cursor()
    }

    /// Monotonic autowrap serial of the active grid: advances on each autowrap line advance, never on a hard newline.
    #[inline]
    #[must_use]
    pub const fn wrap_serial(&self) -> u64 {
        self.grid.wrap_serial()
    }

    /// Check if cursor is visible.
    #[inline]
    #[must_use]
    pub const fn cursor_visible(&self) -> bool {
        self.modes.cursor_visible
    }

    /// The echo anchor: where the most recent PTY print run ended, as `(row, col)` in active-grid coordinates one past the last glyph (a pending wrap counts, so a glyph at the last column reports `col == cols`), plus a monotonic per-print sequence.
    #[inline]
    #[must_use]
    pub const fn print_anchor(&self) -> Option<(u16, u16, u64)> {
        match self.transient.print_anchor {
            Some((row, col)) => Some((row, col, self.transient.print_anchor_seq)),
            None => None,
        }
    }

    /// What the active grid holds now at the echo anchor run's last cell (`(row, col - 1)` of [`Self::print_anchor`], independent of scroll), in [`Self::row_cols_into`]'s per-column convention: the lead char, `'\0'` at a wide continuation, `' '` for a blank.
    #[inline]
    #[must_use]
    pub fn print_anchor_glyph(&self) -> Option<char> {
        let (row, col) = self.transient.print_anchor?;
        let col = col.checked_sub(1)?;
        (row < self.grid.rows() && col < self.grid.cols())
            .then(|| self.grid.screen_row_view(row).col_char(col))
    }

    /// The viewport row the cursor projects onto, on screen or not: its active-grid row pushed down by the scrollback offset.
    #[inline]
    #[must_use]
    pub const fn projected_cursor_row(&self) -> usize {
        (self.cursor().row as usize).saturating_add(self.grid().display_offset())
    }

    /// The viewport row the cursor occupies in a `rows`-tall frame, or `None` when nothing is drawn there (DECTCEM hid it, or scroll-back pushed it off the bottom).
    #[inline]
    #[must_use]
    pub const fn cursor_row_on_screen(&self, rows: usize) -> Option<usize> {
        let row = self.projected_cursor_row();
        if self.cursor_visible() && row < rows {
            Some(row)
        } else {
            None
        }
    }

    /// DECSCNM (mode 5) reverse video.
    #[inline]
    #[must_use]
    pub const fn reverse_video(&self) -> bool {
        self.modes.reverse_video
    }

    /// Monotonic repaint-blink epoch: counts DECTCEM hides processed inside DEC 2026 synchronized output, the per-keystroke repaint bracket of full-redraw TUIs.
    #[inline]
    #[must_use]
    pub const fn repaint_blink_epoch(&self) -> u64 {
        self.repaint_blink_epoch
    }

    /// Get current cursor style.
    #[inline]
    #[must_use]
    pub const fn cursor_style(&self) -> CursorStyle {
        self.modes.cursor_style
    }

    /// Set the host-preferred default cursor style, used before any DECSCUSR and restored on RIS/DECSTR.
    #[inline]
    pub fn set_default_cursor_style(&mut self, style: CursorStyle) {
        let was_default = self.modes.cursor_style == self.default_cursor_style;
        self.default_cursor_style = style;
        if was_default && self.modes.cursor_style != style {
            self.modes.cursor_style = style;
            self.modes.cursor_blink = style.blinks();
        }
    }

    /// Get number of rows.
    #[inline]
    #[must_use]
    pub const fn rows(&self) -> u16 {
        self.grid.rows()
    }

    /// Get number of columns.
    #[inline]
    #[must_use]
    pub const fn cols(&self) -> u16 {
        self.grid.cols()
    }

    /// Per-stage durations (parse, grid, total) of the last `process()` call.
    #[must_use]
    #[inline]
    pub const fn pipeline_timestamps(&self) -> &PipelineTimestamps {
        &self.transient.pipeline_timestamps
    }

    /// Process-unique identity stamped into every engine-filled RenderInput, so a cursor-effect witness can tell its later content observation comes from this terminal.
    #[must_use]
    #[inline]
    pub const fn render_identity(&self) -> u64 {
        self.extract_identity
    }
}

use super::TerminalModes;

impl Terminal {
    /// Get a mutable reference to the terminal modes.
    #[inline]
    pub const fn modes_mut(&mut self) -> &mut TerminalModes {
        &mut self.modes
    }

    /// Set the host's alternate-scroll default (DEC 1007): the live mode now, and the value RIS, DECSTR and [`Self::foreground_handback`] restore 1007 to.
    #[inline]
    pub fn set_host_alternate_scroll(&mut self, on: bool) {
        self.modes.alternate_scroll = on;
        self.host_alternate_scroll = on;
        self.refresh_mode_mirror();
    }

    /// Set the host's alternate-scroll baseline without overwriting `modes.alternate_scroll` (for restoring a checkpointed engine whose live DEC 1007 mode was already installed).
    #[inline]
    pub const fn set_host_alternate_scroll_default(&mut self, on: bool) {
        self.host_alternate_scroll = on;
    }

    /// The host's alternate-scroll baseline that RIS, DECSTR and [`Self::foreground_handback`] restore DEC 1007 to.
    #[inline]
    #[must_use]
    pub const fn host_alternate_scroll_default(&self) -> bool {
        self.host_alternate_scroll
    }
}

/// A DEC mode-2048 in-band size report, `CSI 48 ; rows ; cols ; pixH ; pixW t` (as xterm/ghostty).
pub(super) fn in_band_size_report(
    rows: u16,
    cols: u16,
    host_cell_px: Option<(u16, u16)>,
) -> super::stack_response::StackResponse<48> {
    use std::fmt::Write as _;
    let (cell_w, cell_h) = host_cell_px.unwrap_or((0, 0));
    let pix_h = u32::from(rows) * u32::from(cell_h);
    let pix_w = u32::from(cols) * u32::from(cell_w);
    let mut r = super::stack_response::StackResponse::<48>::new();
    let _ = write!(r, "\x1b[48;{rows};{cols};{pix_h};{pix_w}t");
    r
}

/// Queue a host call's [`in_band_size_report`] (a resize, a new cell box) with the reports the
/// terminal sends unasked ([`super::TransientState::push_report`], which holds the cap a resize
/// storm against a host that does not drain would test). A report the program's own output
/// provokes goes through `TerminalHandler::send_report` instead.
pub(super) fn push_in_band_size_report(
    transient: &mut super::TransientState,
    rows: u16,
    cols: u16,
    host_cell_px: Option<(u16, u16)>,
) {
    let _ = transient.push_report(in_band_size_report(rows, cols, host_cell_px).as_bytes());
}
