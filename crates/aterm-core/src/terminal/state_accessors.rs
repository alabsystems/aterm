// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! State accessors for `Terminal`: keyboard, cursor, modes, title and the security
//! authorizations.

use super::{
    CurrentStyle, Cursor, CursorStyle, KittyKeyboardFlags, MAX_TITLE_BYTES, Terminal,
    XtermKeyboardState,
};
use crate::grid::ResizePolicy;
use aterm_types::PipelineTimestamps;
use std::sync::Arc;

/// Where the reported colour scheme (DSR 996 / mode 2031) comes from
/// ([`Terminal::color_scheme_source`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSchemeSource {
    /// The live default background is the host-configured one: the host's
    /// baseline appearance (`Terminal::set_color_scheme` /
    /// `TerminalConfig::color_scheme`).
    Configured,
    /// A program moved the default background (OSC 11/21, a colour-stack pop,
    /// a handoff replay): that background's polarity.
    Program,
}

/// Production accessors used by external crates and FFI.
impl Terminal {
    /// Get the current Kitty keyboard enhancement flags.
    #[must_use]
    pub fn kitty_keyboard_flags(&self) -> KittyKeyboardFlags {
        self.kitty_keyboard.flags()
    }

    /// Enable or disable the Kitty keyboard protocol (default on), for embedders whose
    /// platform consumes kitty sequences itself (Windows ConPTY). When disabled the
    /// terminal acts as if the protocol is unsupported: `CSI ? u` gets no reply,
    /// `CSI > u` / `= u` / `< u` are ignored, and [`keyboard_mode`](Self::keyboard_mode)
    /// carries no kitty flags, even ones negotiated earlier. Host policy: preserved across
    /// `reset()`/RIS.
    pub fn set_kitty_keyboard_enabled(&mut self, enabled: bool) {
        self.modes.kitty_keyboard_enabled = enabled;
        self.refresh_mode_mirror();
    }

    /// Declare whether this session sits behind a Windows console host (ConPTY), whose
    /// `CSI ? 9001 h` asks for win32-input-mode key records (default off). Elsewhere DEC 9001
    /// is ignored, so program output cannot turn Escape and Enter into records nothing reads.
    /// Behind the host the xterm modifyOtherKeys family is refused (`CSI > 4 ; Pv m` and
    /// `f` are ignored, `CSI ? 4 m` reads level 0, an earlier level stops reaching the
    /// encoder): conhost reads the key bytes and has no reader for those chords.
    /// Host policy: preserved across `reset()`/RIS.
    pub fn set_win32_input_enabled(&mut self, enabled: bool) {
        self.modes.win32_input_enabled = enabled;
        self.refresh_mode_mirror();
    }

    /// Declare whether the console host behind win32-input-mode is OpenConsole rather than
    /// Windows' inbox conhost (default off): the two read KP_Begin and F13-F20 differently
    /// (`KeyboardMode::OPENCONSOLE_HOST`). Meaningful only while
    /// [`set_win32_input_enabled`](Self::set_win32_input_enabled) holds. Host policy:
    /// preserved across `reset()`/RIS.
    pub fn set_win32_input_openconsole(&mut self, openconsole: bool) {
        self.modes.win32_input_openconsole = openconsole;
        self.refresh_mode_mirror();
    }

    /// Set the keyboard capabilities a console host decides from the same fact that picks
    /// the resize seam policy, so the two cannot disagree. Behind a Windows console host
    /// ([`ResizePolicy::ConPty`]) DEC 9001 is honored and the kitty protocol and xterm's
    /// modifyOtherKeys are off (conhost cannot turn CSI-u or `CSI 27 ; m ; c ~` reports into
    /// console key records); any other session gets
    /// the reverse. Sets both bits either way, so a checkpoint carrying another host's bits
    /// does not outlive it. A live session and every replay (`cast drift`) call this.
    pub fn set_console_host_posture(&mut self, policy: ResizePolicy) {
        let console_host = policy == ResizePolicy::ConPty;
        self.modes.win32_input_enabled = console_host;
        self.modes.kitty_keyboard_enabled = !console_host;
        self.refresh_mode_mirror();
    }

    /// Host knowledge, before the bytes it feeds next: whether a program other than the
    /// shell holds the terminal's foreground while it wrote them (a unix host compares the
    /// foreground process group it read for those bytes with the shell's pid; a host with
    /// no foreground group says `false`). Kept across RIS.
    ///
    /// An OSC 7 no one signed while the shell sits at its prompt by its marks is then that
    /// program's, not a background job's: a program a key binding started (a zsh widget, a
    /// bash `bind -x`, a fish binding; each gets the foreground with no `C`), so it lands.
    /// Once no program holds the foreground, and none the program probe sees runs
    /// ([`Self::set_program_probe`]), the cwd the first such report replaced comes back, as
    /// the shell reports its own again only at its next prompt.
    pub fn set_foreground_job(&mut self, job: bool) {
        self.shell.foreground_job = job;
        if !job
            && self.shell.cwd_before_job.is_some()
            && !self.shell.program_runs_since_prompt()
            && let Some(before) = self.shell.cwd_before_job.take()
        {
            let (_, mut handler) = self.split_for_process();
            handler.apply_cwd_report(before);
        }
    }

    /// Install the host's program probe: given a wall-clock instant (unix epoch ms), whether
    /// a program the shell started at or after it runs now where the foreground word
    /// ([`Self::set_foreground_job`]) cannot see it. PowerShell keeps the programs it runs
    /// in its own process group (measured on pwsh 7.6.7, macOS), so one a PSReadLine key
    /// handler starts holds no foreground of its own; ConPTY has no foreground group at
    /// all. The engine asks it lazily, with the instant the shell's prompt began: for an
    /// OSC 7 no one signed at the shell's prompt that no foreground word claims (it lands
    /// when the probe says such a program runs), and at each foreground word while a
    /// report it let land shows (the cwd it replaced comes back once the program is gone).
    /// Host-side, never carried: a host installs it on every engine it reads for.
    pub fn set_program_probe(&mut self, probe: Option<Box<dyn Fn(u64) -> bool + Send>>) {
        self.shell.program_probe = probe;
    }

    /// Host policy: the console host between this terminal and its app forwards the app's
    /// alternate-screen switches verbatim and never repaints the buffer it switches to
    /// (Windows Terminal's OpenConsole). A shell prompt on the alternate screen (a
    /// full-screen app died without its `?1049l`) then does not leave the screen locally; it
    /// raises ONE request per stuck screen for the host to leave its own dead buffer
    /// ([`take_alt_screen_resync_request`](Self::take_alt_screen_resync_request)), whose
    /// forwarded `?1049l` leaves both. Kept across RIS; off by default, which is the local
    /// leave every other host gets.
    pub fn set_host_resyncs_alt_screen(&mut self, on: bool) {
        self.transient.host_resyncs_alt_screen = on;
        if !on {
            self.transient.alt_screen_resync_requested = false;
            self.transient.dead_alt_screen = None;
        }
    }

    /// The embedder is about to make its console host leave the alternate screen a dead app
    /// left up (the `mainscreen` verb under the host policy,
    /// [`set_host_resyncs_alt_screen`](Self::set_host_resyncs_alt_screen)): the `?1049l` the
    /// host forwards then leaves it as a dead app's screen, releasing the modes the app left
    /// on as a prompt's request does, and a prompt on that screen asks the host nothing more.
    /// `false`, with nothing marked, when the primary screen is up.
    pub fn expect_host_alt_screen_leave(&mut self) -> bool {
        if !self.modes.alternate_screen {
            return false;
        }
        let screen = Some(self.transient.screen_replaced);
        self.transient.dead_alt_screen = screen;
        self.transient.alt_screen_resync_screen = screen;
        true
    }

    /// Take the pending alternate-screen resync request (see
    /// [`set_host_resyncs_alt_screen`](Self::set_host_resyncs_alt_screen)): `true` once per
    /// stuck screen, when the embedder should make its console host leave the alternate
    /// buffer. Always `false` while the policy is off.
    pub fn take_alt_screen_resync_request(&mut self) -> bool {
        std::mem::take(&mut self.transient.alt_screen_resync_requested)
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

    /// Set the window title, truncated at a char boundary to `MAX_TITLE_BYTES`.
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

    /// The monotonic count of desktop notifications the program asked for
    /// (OSC 9, a final OSC 99 chunk, OSC 777 `notify`), whether or not
    /// notifications are allowed: the session log counts them
    /// (`mark - notify n=`), never their text.
    #[must_use]
    pub fn notify_total(&self) -> u64 {
        self.notify_total
    }

    /// The window title as a shared [`Arc<str>`] handle: a reference-count bump,
    /// so callers can hold the title without copying or borrowing the terminal.
    #[must_use]
    pub fn title_arc(&self) -> std::sync::Arc<str> {
        Arc::clone(&self.title.window)
    }

    /// Monotonic tab-label change epoch: bumped when the window title (OSC 0/2 or
    /// [`set_title`](Self::set_title)) or the shell-reported cwd (OSC 7 / OSC 633
    /// `P;Cwd=`, shown for an unset title) changes value. A lock-free `Relaxed` load, so
    /// hosts poll it without the terminal lock; it wraps, so compare against a cached value.
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

    /// Enable or disable secure keyboard entry mode. Advisory: the UI layer activates the
    /// platform's secure input (macOS `EnableSecureEventInput()`).
    #[cfg(test)]
    pub fn set_secure_keyboard_entry(&mut self, enabled: bool) {
        self.secure_keyboard_entry = enabled;
    }

    /// Set the host's baseline colour scheme: the appearance of the
    /// host-configured default background (a theme's declared appearance).
    /// Programs read the REPORTED scheme via DSR `CSI ? 996 n`, which is this
    /// baseline while the background is the configured one and the live
    /// background's polarity while a program holds its own (OSC 11; see
    /// [`color_scheme`](Self::color_scheme)). When the reported scheme changes
    /// and DEC mode 2031 is set, the engine queues `CSI ? 997 ; Ps n`
    /// (1 = dark, 2 = light) for the host to drain via
    /// [`take_response`](Self::take_response).
    ///
    /// Hosts with a [`TerminalConfig`](crate::config::TerminalConfig) pass the
    /// baseline as its `color_scheme` instead, so one `apply_config` lands the
    /// colours and the scheme together.
    pub fn set_color_scheme(&mut self, baseline: aterm_types::Appearance) {
        self.color.configured_color_scheme = baseline;
        let _ = self.reconcile_color_scheme();
    }

    /// The colour scheme DSR 996 answers now: the background on screen. The
    /// host's baseline ([`set_color_scheme`](Self::set_color_scheme)) while the
    /// live default background is the configured one, else that background's
    /// polarity under [`aterm_messages::ink::bg_is_light`].
    #[must_use]
    pub fn color_scheme(&self) -> aterm_types::Appearance {
        self.color.reported_scheme().0
    }

    /// Where [`color_scheme`](Self::color_scheme) comes from.
    #[must_use]
    pub fn color_scheme_source(&self) -> ColorSchemeSource {
        self.color.reported_scheme().1
    }

    /// [`Self::reconcile_color_scheme`] at the end of a process batch, where
    /// the change came from the program's own output (OSC 10/11/110/111,
    /// OSC 21, a colour-stack pop, RIS, DECSTR). Such a push is a reply the
    /// program provoked, so it passes the gates every program reply passes
    /// ([`program_response_admitted`](super::handler::program_response_admitted): the policy's `response any`
    /// verdict and the response rate limit). Refused, the latch stays where it
    /// was, so the next reconcile (host-driven or a later batch) can still
    /// announce the change. Host entry points keep the ungated reconcile.
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

    /// Bring the announced latch (`TerminalModes::color_scheme`) to the derived
    /// scheme, queueing the `CSI ? 997 ; Ps n` push when it moved and DEC 2031
    /// is set. This ungated form is for host entry points (apply_config,
    /// rebase, set_color_scheme, a checkpoint restore, a host reset); the end
    /// of a process batch goes through
    /// [`Self::reconcile_color_scheme_after_output`], which first applies the
    /// program-reply gates. The push is a report sent unasked, so it is queued
    /// with the reports ([`Self::take_reports`]), and like any advisory report it
    /// is dropped when what is owed to the peer is at its cap. Called at host entry
    /// points and the end of each process batch, never per sequence, so
    /// several background writes in one batch push at most once, and only for
    /// a net change. Returns whether the latch moved.
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

    /// Whether an OSC 52 query the program sends now reaches the host's clipboard
    /// callback: one is installed and the query gate opens (authorized, or allowed by the
    /// installed policy). aterm-gui's `host_io_prefetch` reads the pasteboard, before it
    /// takes the terminal lock, only when this says yes.
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

    /// Authorize a clipboard access class for parser-origin sequences: `Write` covers
    /// OSC 52 set/clear and OSC 1337 `CopyToClipboard` / `EndCopy` / `Copy=`; `Query`
    /// covers OSC 52 queries. The only way to mint a clipboard capability.
    pub fn authorize_clipboard_access(&mut self, access: super::clipboard_auth::ClipboardAccess) {
        use super::clipboard_auth::ClipboardAccess;
        match access {
            ClipboardAccess::Write => {
                self.clipboard_auth.authorize_write();
                // Mirror the bit on modes so checkpoints and FFI observers agree.
                self.modes.allow_osc52_set = true;
            }
            ClipboardAccess::Query => {
                self.clipboard_auth.authorize_query();
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

    /// Authorize OSC 133 / OSC 633 shell integration with a 32-byte capability nonce.
    ///
    /// While [`TerminalModes::require_shell_integration_nonce`] is set, OSC 133 A/B/C/D and
    /// OSC 633 A-H/P sequences dispatch only if their `id=<64-hex>` equals `nonce`
    /// (constant-time compare); others are dropped silently. The host generates the nonce
    /// with a CSPRNG, installs it here, passes it to the shell (`ATERM_SHELL_NONCE`) so
    /// the preamble emits `id=<hex>`, and turns enforcement on once the preamble is in place.
    pub fn authorize_shell_integration(&mut self, nonce: [u8; 32]) {
        self.shell_integration_auth.authorize(nonce);
    }

    /// [`authorize_shell_integration`][Self::authorize_shell_integration] for a nonce the
    /// shell has not taken yet: the re-key of an adopted shell whose nonce did not survive an
    /// update, delivered through a file the shell reads at its next prompt (aterm-gui
    /// `shell_rekey`). Marks signed with it verify at once, but
    /// [`shell_integration_posture`][Self::shell_integration_posture] reads `Degraded` and
    /// the seamless-handoff carry leaves it behind until the first arrives.
    pub fn authorize_shell_integration_on_first_mark(&mut self, nonce: [u8; 32]) {
        self.shell_integration_auth.authorize_on_first_mark(nonce);
    }

    /// [`authorize_shell_integration_on_first_mark`][Self::authorize_shell_integration_on_first_mark]
    /// for a typed re-key (aterm-gui `shell_rekey::typed`): a nonce left in a one-use file
    /// for a line typed at the prompt. The replaced authorization is kept until
    /// [`withdraw_shell_integration_rekey`][Self::withdraw_shell_integration_rekey] restores
    /// it (the line never ran) or
    /// [`settle_shell_integration_rekey`][Self::settle_shell_integration_rekey] forgets it
    /// (the shell read the file). Marks verify at once; it is `On` from the first of them.
    pub fn authorize_shell_integration_rekey(&mut self, nonce: [u8; 32]) {
        self.shell_integration_auth.authorize_rekey(nonce);
    }

    /// Take back the typed re-key `nonce`, restoring the authorization it replaced. `false`,
    /// changing nothing, when `nonce` is not that pending key (a mark signed with it already
    /// arrived, or it was replaced or settled).
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

    /// Number of OSC 133 / OSC 633 sequences whose capability nonce verified since
    /// construction (saturating; a pure counter, only moved while
    /// `modes.require_shell_integration_nonce` is `true`).
    ///
    /// The crash-restore journal reads it around each slice: a restore replays the raw
    /// output past its keyframe into an engine that does not hold the live nonce (a
    /// restored shell re-keys), where a verified mark would be dropped, so the journal
    /// keyframes a slice that moved this count and counts the marks a rate limit left in
    /// its tail.
    #[must_use]
    pub fn shell_marks_verified(&self) -> u64 {
        self.shell_integration_auth.verified_count()
    }

    /// Number of kitty-graphics APC strings (`ESC _ G … ST`) that ended since construction,
    /// complete or cut short by CAN/SUB, whatever each did: a pure counter, never reset.
    ///
    /// The crash-restore journal's recovery reads it around its replay: what a kitty
    /// command does depends on state no checkpoint carries and no scratch engine has (the
    /// image store, a chunked transmission under way, the image numbers, and the host's
    /// file and shared-memory medium resolver), so a command the tail holds may place an
    /// image and move the cursor live and do neither in the replay. A replay that ran one
    /// is not called exact.
    #[must_use]
    pub fn kitty_graphics_commands(&self) -> u64 {
        self.transient.kitty_commands
    }

    /// Number of Kitty unscrolls (`CSI n + T`) run on the main screen since
    /// construction, whatever each pulled: a pure counter, never reset. One on the
    /// alternate screen is not counted: that grid keeps no history, so it scrolls in
    /// blank rows wherever it runs.
    ///
    /// The crash-restore journal's recovery reads it around its replay: an unscroll
    /// pulls the newest history lines onto the screen, and the scratch engine holds
    /// only the keyframe's bridge (none at all once the history ring has caught up),
    /// in its store where the live engine read its ring first, so the replay may
    /// scroll in blank rows, or other lines, where the live one pulled history. A
    /// replay that ran one is not called exact.
    #[must_use]
    pub fn kitty_unscrolls(&self) -> u64 {
        self.transient.kitty_unscrolls
    }

    /// Enable or disable OSC 133 / OSC 633 capability-nonce enforcement (default off):
    /// when on, every sequence must carry a matching `id=<64-hex>` (see
    /// [`Self::authorize_shell_integration`]). Preserved across RIS and
    /// [`Terminal::reset`], so a program cannot turn enforcement off with a reset.
    pub fn set_require_shell_integration_nonce(&mut self, required: bool) {
        self.modes.require_shell_integration_nonce = required;
    }

    /// Whether OSC 133 / OSC 633 capability-nonce enforcement is
    /// currently enabled.
    #[must_use]
    pub fn is_require_shell_integration_nonce(&self) -> bool {
        self.modes.require_shell_integration_nonce
    }

    /// Enable or disable OSC 9 / 99 / 777 desktop notifications (default off): while off,
    /// the handlers in `handler_osc_notify.rs` never reach the callback, so a host that
    /// wires one must also call this with `true`. Preserved across `reset()` and RIS, so a
    /// program cannot re-enable notifications with a reset.
    pub fn set_allow_notifications(&mut self, allowed: bool) {
        self.modes.allow_notifications = allowed;
    }

    /// Authorize OSC 9 / 99 / 777 desktop notifications (`modes.allow_notifications`).
    /// The pending-notification cap applies regardless.
    #[cfg(test)]
    pub fn authorize_notifications(&mut self) {
        self.modes.allow_notifications = true;
    }

    /// Add an extra OSC 8 URI scheme to the safe allowlist (see [`super::hyperlink_auth`];
    /// what a person may open is checked by the host at click time).
    ///
    /// Returns `false`, state unchanged, when `scheme` is over 32 bytes, not RFC 3986
    /// scheme-shaped, in the never-allow set (`javascript`/`data`/`file`/`vbscript`/
    /// `about`/`blob`, case- and suffix-insensitively), or when 4 extra schemes are already
    /// live; `true` when stored (idempotent, lowercased). The other OSC 8 guards still
    /// apply. Extra schemes survive soft and hard resets and are not checkpointed.
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

    /// Remove a host-minted extra scheme (case-insensitive): OSC 8 URIs with it
    /// are refused again. Absent schemes are a no-op.
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

    /// Enable or disable OSC 4 / OSC 21 indexed palette reconfigure (engine
    /// default off; the aterm app turns it on unless configured false): while
    /// disabled, PTY-origin attempts to change the 256-entry palette
    /// are ignored. Equivalent to setting
    /// `modes_mut().allow_palette_reconfigure`; preserved across
    /// `Terminal::reset()` and RIS so a program cannot re-enable it with a reset.
    ///
    /// Opening a closed gate re-arms the refusal report
    /// ([`Self::palette_set_refused`] reads false again), so a refusal after
    /// the gate closes again calls the host's `palette_refused_callback` anew.
    pub fn set_allow_palette_reconfigure(&mut self, allowed: bool) {
        if allowed && !self.modes.allow_palette_reconfigure {
            self.color.palette_set_refused = false;
        }
        self.modes.allow_palette_reconfigure = allowed;
    }

    /// Whether programs may set the 256 indexed colours (OSC 4 / numeric
    /// OSC 21) in this session (see [`Self::set_allow_palette_reconfigure`]).
    #[must_use]
    pub fn allow_palette_reconfigure(&self) -> bool {
        self.modes.allow_palette_reconfigure
    }

    /// What programs set over the configured colours (OSC 4/10/11/12/17/19/
    /// 21): the diff `apply_config` replays over a new theme and a handoff
    /// carries. Empty once OSC 104/110-119, RIS or a manual handback put the
    /// theme's colours back. The colour stack (OSC 30001) is not included.
    #[must_use]
    pub fn program_color_overrides(&self) -> super::ColorOverridesRepr {
        self.color.capture_overrides()
    }

    /// Get cursor position.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        self.grid.cursor()
    }

    /// Monotonic autowrap serial of the active grid: advances on each autowrap line
    /// advance, never on a hard newline. A host diffs it against a baseline read under the
    /// same lock as [`Self::cursor`] to tell a wrap from `Home` pressed at the last column.
    /// Not checkpointed and kept per grid across main/alt swaps, so a restore or buffer
    /// switch costs at most one spurious read.
    #[must_use]
    pub fn wrap_serial(&self) -> u64 {
        self.grid.wrap_serial()
    }

    /// Check if cursor is visible.
    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.modes.cursor_visible
    }

    /// The echo anchor: where the most recent PTY print run ended, as `(row, col)` in
    /// active-grid coordinates one past the last glyph (a pending wrap counts, so a glyph at
    /// the last column reports `col == cols`), plus a monotonic per-print sequence. `None`
    /// before the first print or after a reset. Read only by the cursor-effect host.
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

    /// Monotonic repaint-blink epoch: counts DECTCEM hides processed inside DEC 2026
    /// synchronized output, the per-keystroke repaint bracket of full-redraw TUIs. Hosts
    /// diff it to classify such apps. Never reset, never gates bytes.
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

    /// Per-stage durations (parse, grid, total) of the last `process()` call.
    #[must_use]
    #[inline]
    pub fn pipeline_timestamps(&self) -> &PipelineTimestamps {
        &self.transient.pipeline_timestamps
    }

    /// Process-unique identity stamped into every engine-filled RenderInput, so a
    /// cursor-effect witness can tell its later content observation comes from this terminal.
    #[must_use]
    #[inline]
    pub fn render_identity(&self) -> u64 {
        self.extract_identity
    }
}

use super::TerminalModes;

impl Terminal {
    /// Get a mutable reference to the terminal modes.
    pub fn modes_mut(&mut self) -> &mut TerminalModes {
        &mut self.modes
    }

    /// Set the host's alternate-scroll default (DEC 1007): the live mode now,
    /// and the value RIS, DECSTR and [`Self::foreground_handback`] restore 1007
    /// to. Programs may still change the live mode with `CSI ? 1007 h/l`.
    pub fn set_host_alternate_scroll(&mut self, on: bool) {
        self.modes.alternate_scroll = on;
        self.host_alternate_scroll = on;
        // 1007 rides the lock-free mode word (the wheel's route reads it).
        self.refresh_mode_mirror();
    }
}

/// Queue a DEC mode-2048 in-band size report, `CSI 48 ; rows ; cols ; pixH ; pixW t`
/// (as xterm/ghostty), with the reports the terminal sends unasked
/// ([`super::TransientState::push_report`], which holds the cap a toggle flood or a
/// resize storm against a host that does not drain would test).
///
/// `host_cell_px` is the cell box a host measured (`Terminal::host_cell_pixel_size`):
/// pixels are `0` until one has, as the PTY's winsize says, never the 8x16
/// placeholder passed off as a measurement. Pixels are computed in `u32`. Shared by
/// the DECSET-2048 enable path, `Terminal::resize` and `Terminal::set_cell_pixel_size`.
pub(super) fn push_in_band_size_report(
    transient: &mut super::TransientState,
    rows: u16,
    cols: u16,
    host_cell_px: Option<(u16, u16)>,
) {
    use std::fmt::Write as _;
    // The report's capacity.
    const MAX_LEN: usize = 48;
    let (cell_w, cell_h) = host_cell_px.unwrap_or((0, 0));
    let pix_h = u32::from(rows) * u32::from(cell_h);
    let pix_w = u32::from(cols) * u32::from(cell_w);
    let mut r = super::stack_response::StackResponse::<MAX_LEN>::new();
    let _ = write!(r, "\x1b[48;{rows};{cols};{pix_h};{pix_w}t");
    let _ = transient.push_report(r.as_bytes());
}
