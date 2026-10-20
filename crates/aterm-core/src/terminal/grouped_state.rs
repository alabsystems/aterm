// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grouped sub-state structs for [`Terminal`](super::Terminal), each bundling
//! fields passed together to
//! [`TerminalHandler`](super::handler::TerminalHandler) during processing.

mod protocol;

use super::callbacks::{AdvancedNotificationCallback, ClipboardCallback, NotificationCallback};
use super::shell::{Annotation, CommandMark, OutputBlock, ShellState, TerminalMark};
use super::transient_state::{DEFAULT_BACKGROUND, DEFAULT_FOREGROUND};
use super::types::SavedCursorState;
use super::{PendingNotificationsMap, UserVarsMap};
use aterm_types::{ColorPalette, Rgb};
use std::collections::VecDeque;
use std::sync::Arc;

// Re-exports for TerminalHandler field types, so handler.rs imports via
// `super::` rather than reaching into external crates.
pub(super) use protocol::SixelState;
pub(super) use protocol::{DcsState, DcsType};

/// Grouped state for terminal marks and annotations.
///
/// Bundles marks (OSC 1337 SetMark) and annotations (OSC 1337 AddAnnotation)
/// with their ID counters. Accessed from `handler_osc.rs` and `shell_api.rs`.
pub(super) struct MarksState {
    /// User-created marks (OSC 1337 SetMark).
    pub(super) marks: VecDeque<TerminalMark>,
    /// Next mark ID to assign.
    pub(super) next_mark_id: u64,
    /// Annotations (OSC 1337 AddAnnotation).
    pub(super) annotations: VecDeque<Annotation>,
    /// Next annotation ID to assign.
    pub(super) next_annotation_id: u64,
}

impl MarksState {
    pub(super) fn new() -> Self {
        Self {
            marks: VecDeque::new(),
            next_mark_id: 0,
            annotations: VecDeque::new(),
            next_annotation_id: 0,
        }
    }
}

/// Grouped color state: the 256-color palette, default fg/bg, cursor color,
/// selection fg/bg, and the Kitty OSC 30001/30101 color stack.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-channel authority bits (fg, bg, cursor) plus an unrelated rendering flag, not a state machine"
)]
pub(super) struct ColorState {
    /// Maps 256 indexed colors to RGB values (OSC 4). Private to `grouped_state`
    /// so every mutation flows through [`Self::palette_mut`], invalidating
    /// `palette_hash`.
    palette: ColorPalette,
    /// Cached nonzero FxHash digest of `palette` (`0` = uncached).
    palette_hash: std::sync::atomic::AtomicU64,
    /// Saved color states for Kitty OSC 30001/30101 push/pop. Per the Kitty
    /// color stack protocol a push covers every dynamic color (palette,
    /// default fg/bg, cursor, both selection colors); each entry is kept as
    /// a diff from the configured baseline, so a pop after the theme moved
    /// lands on the new theme with the program's own colors over it.
    pub(super) stack: VecDeque<super::checkpoint_state::ColorOverridesRepr>,
    /// Default foreground color (OSC 10, reset via OSC 110).
    pub(super) default_foreground: Rgb,
    /// Default background color (OSC 11, reset via OSC 111).
    pub(super) default_background: Rgb,
    /// Theme-configured foreground color, set by `apply_config`; OSC 110
    /// resets `default_foreground` to it.
    pub(super) configured_foreground: Rgb,
    /// Whether a host setter/config or an OSC mutation made the foreground
    /// authoritative for render snapshots. A fresh `Terminal::new` leaves it
    /// false, so the renderer's theme applies.
    pub(super) frame_foreground_authoritative: bool,
    /// Theme-configured background color, set by `apply_config`; OSC 111
    /// resets `default_background` to it.
    pub(super) configured_background: Rgb,
    /// Host baseline: the appearance of `configured_background` as the host
    /// declares it (a theme's `appearance`), reported while no program has
    /// moved the background (see [`Self::reported_scheme`]). Host policy like
    /// `configured_*`, so `reset()` leaves it alone. Default `Dark`.
    pub(super) configured_color_scheme: aterm_types::Appearance,
    /// Whether a host setter/config or an OSC mutation made the background
    /// authoritative for render snapshots.
    pub(super) frame_background_authoritative: bool,
    /// Theme-configured palette, set by `apply_config` when the theme provides
    /// `custom_palette`. OSC 104 resets to it; `None` resets to xterm defaults.
    pub(super) configured_palette: Option<ColorPalette>,
    /// Theme-configured cursor color, set by `apply_config`, restored by OSC
    /// 112 and RIS. `None` falls back to the live default foreground.
    pub(super) configured_cursor: Option<Rgb>,
    /// Whether a host setter/config or an OSC mutation made the cursor policy
    /// authoritative for render snapshots. Distinct from
    /// `cursor_color.is_some()`: OSC 21 `cursor=` selects an authoritative
    /// dynamic foreground fallback.
    pub(super) frame_cursor_authoritative: bool,
    /// Cursor color (OSC 12, reset via OSC 112). None = use foreground.
    pub(super) cursor_color: Option<Rgb>,
    /// Host-configured selection background, restored by OSC 117 and RIS.
    pub(super) configured_selection_background: Option<Rgb>,
    /// Selection background color (OSC 21). None = renderer default.
    pub(super) selection_background: Option<Rgb>,
    /// Host-configured selection foreground, restored by OSC 119 and RIS.
    /// `None` leaves selected-text contrast to the renderer.
    pub(super) configured_selection_foreground: Option<Rgb>,
    /// Selection foreground color (OSC 19). None = renderer default.
    pub(super) selection_foreground: Option<Rgb>,
    /// Host style policy: SGR 1 promotes indexed 0–7 to bright 8–15.
    /// Config-level (like `configured_*`), so `reset()` leaves it alone.
    pub(super) bold_is_bright: bool,
    /// Host style policy: fraction of fg retained by SGR 2, blended
    /// toward the cell bg in linear light. Config-level; survives `reset()`.
    pub(super) faint_opacity: f32,
    /// Host style policy: the theme-pair contrast floor
    /// (`color_resolve::theme_pair_floor`), `1.0` off. Config-level;
    /// survives `reset()`.
    pub(super) indexed_minimum_contrast: f32,
    /// Whether an OSC 4 / numeric OSC 21 palette set has been refused by the
    /// `allow_palette_reconfigure` gate in this session. Session-level, so RIS
    /// leaves it: the host hears about the FIRST refusal only.
    pub(super) palette_set_refused: bool,
    /// Called once, at the first refused palette set (see
    /// `Terminal::set_palette_refused_callback`).
    pub(super) palette_refused_callback: Option<Box<dyn FnMut() + Send>>,
}

impl ColorState {
    pub(super) fn new() -> Self {
        Self {
            palette: ColorPalette::new(),
            palette_hash: std::sync::atomic::AtomicU64::new(0),
            stack: VecDeque::new(),
            default_foreground: DEFAULT_FOREGROUND,
            default_background: DEFAULT_BACKGROUND,
            configured_foreground: DEFAULT_FOREGROUND,
            frame_foreground_authoritative: false,
            configured_background: DEFAULT_BACKGROUND,
            configured_color_scheme: aterm_types::Appearance::Dark,
            frame_background_authoritative: false,
            configured_palette: None,
            configured_cursor: None,
            frame_cursor_authoritative: false,
            cursor_color: None,
            configured_selection_background: None,
            selection_background: None,
            configured_selection_foreground: None,
            selection_foreground: None,
            bold_is_bright: true,
            faint_opacity: aterm_types::DIM_FACTOR,
            indexed_minimum_contrast: 1.0,
            palette_set_refused: false,
            palette_refused_callback: None,
        }
    }

    /// Read-only access to the 256-color palette.
    #[inline]
    pub(super) fn palette(&self) -> &ColorPalette {
        &self.palette
    }

    /// Mutable access to the 256-color palette, invalidating the cached
    /// [`Self::palette_fingerprint`] digest.
    #[inline]
    pub(super) fn palette_mut(&mut self) -> &mut ColorPalette {
        self.palette_hash
            .store(0, std::sync::atomic::Ordering::Relaxed);
        &mut self.palette
    }

    /// Cached 64-bit FxHash digest of all 256 indexed palette entries.
    ///
    /// When `palette.overrides_count() == 0` (the built-in xterm 256-color
    /// palette), returns a `OnceLock` constant in O(1) without hashing or
    /// touching `palette_hash`. When overrides exist, computes the 256-entry
    /// hash once and caches it in `palette_hash` (`0` = uncached; every mutable
    /// borrow via [`Self::palette_mut`] resets `palette_hash` to `0`).
    pub(super) fn palette_fingerprint(&self) -> u64 {
        use std::hash::Hasher as _;
        use std::sync::OnceLock;
        use std::sync::atomic::Ordering;

        fn compute_palette_hash(palette: &ColorPalette) -> u64 {
            let mut h = aterm_hash::FxHasher::default();
            for i in 0..=u8::MAX {
                let c = palette.get(i);
                let packed = (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
                h.write_u32(packed);
            }
            let digest = h.finish();
            if digest == 0 { 1 } else { digest }
        }

        if self.palette.overrides_count() == 0 {
            static DEFAULT_HASH: OnceLock<u64> = OnceLock::new();
            return *DEFAULT_HASH.get_or_init(|| compute_palette_hash(&ColorPalette::new()));
        }
        let cached = self.palette_hash.load(Ordering::Relaxed);
        if cached != 0 {
            return cached;
        }
        let computed = compute_palette_hash(&self.palette);
        self.palette_hash.store(computed, Ordering::Relaxed);
        computed
    }

    /// The colour scheme a program is told now (DSR 996 / mode 2031): the
    /// host's baseline while the live default background is the configured
    /// one, else the live background's polarity under the chrome's one
    /// classifier ([`aterm_messages::ink::bg_is_light`]).
    ///
    /// A pure function of the colours, so it has no state to reset: it returns
    /// to the baseline exactly when the background does (OSC 111, OSC 21 bare
    /// `background`, a kitty pop, RIS, a reload where the override no longer
    /// differs). A foreground handback leaves dynamic colours alone, so a
    /// program that dies holding a light OSC 11 keeps reporting light, which is
    /// what is on screen.
    ///
    /// Reverse video (DECSCNM, SGR 7) is applied at render time in
    /// `color_resolve` and deliberately not consulted: a visual-bell flash
    /// must not emit a pair of 997 pushes. Neither is the host's query
    /// callback, which answers OSC 10/11 queries, not the scheme.
    pub(super) fn reported_scheme(&self) -> (aterm_types::Appearance, super::ColorSchemeSource) {
        use aterm_types::Appearance;
        if self.default_background == self.configured_background {
            return (
                self.configured_color_scheme,
                super::ColorSchemeSource::Configured,
            );
        }
        let bg = self.default_background;
        let scheme = if aterm_messages::ink::bg_is_light([bg.r, bg.g, bg.b]) {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        (scheme, super::ColorSchemeSource::Program)
    }

    /// Reset all color state to the theme-configured values (RIS /
    /// Terminal::reset): palette, dynamic colors, cursor color and color stack.
    /// Callbacks and configured defaults are kept (they are not terminal
    /// state).
    pub(super) fn reset(&mut self) {
        self.stack.clear();
        // Reset to the configured default, never a transient state: reverse
        // video (SGR 7 / DECSCNM) is applied at render time in color_resolve, so
        // it is neither captured nor preserved here (xterm semantics).
        self.restore_baseline(true);
    }
}

/// The last accepted OSC 133 shell integration marker (A/B/C/D), enforcing
/// the A→B→C→D order; out-of-order markers are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ShellIntegrationPhase {
    /// No marker has been accepted yet (initial state, or after reset).
    #[default]
    None,
    /// Prompt start marker accepted (OSC 133 ; A).
    PromptStart,
    /// Command input start marker accepted (OSC 133 ; B).
    CommandStart,
    /// Command execution start marker accepted (OSC 133 ; C).
    CommandExec,
    /// Command finished marker accepted (OSC 133 ; D).
    CommandFinished,
}

/// Grouped state for shell integration (OSC 133/633).
///
/// Bundles the shell state machine, command marks and output blocks.
/// Accessed from OSC 133/633 handlers, shell_api.rs and blocks_api.rs.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about who speaks (the shell's habit, its unmarked run, a re-key's stale phase, the host's foreground word, a carried cwd), not a state machine"
)]
pub(super) struct ShellIntegrationState {
    /// Shell integration state machine (OSC 133).
    pub(super) state: ShellState,
    /// Current phase in the A→B→C→D state machine: only `None`/
    /// `CommandFinished` → A, A → B, B → C, C → D; out-of-order markers are
    /// ignored.
    pub(super) phase: ShellIntegrationPhase,
    /// Current command mark being built (OSC 133).
    pub(super) current_mark: Option<CommandMark>,
    /// Completed command marks (FIFO eviction via VecDeque).
    pub(super) command_marks: VecDeque<CommandMark>,
    /// Output blocks (command+output units) for block-based model (FIFO eviction).
    pub(super) output_blocks: VecDeque<OutputBlock>,
    /// Current block being built (in progress).
    pub(super) current_block: Option<OutputBlock>,
    /// The block the latest accepted D completed, as it stood at that D
    /// (`None` before the first D, or when that D landed on no block). A
    /// scrollback erase or the block store's cap can drop the block itself
    /// once a prompt has archived it; this copy keeps its exit status for
    /// [`super::Terminal::last_finished_block`]. Kept across `reset()` (a RIS
    /// does not undo a finished command), never checkpointed.
    pub(super) last_finished: Option<OutputBlock>,
    /// Next block ID to assign.
    pub(super) next_block_id: u64,
    /// Monotonic count of completed commands (OSC 133;D). Unlike end
    /// timestamps it never repeats, so it answers "did a new command finish".
    /// Preserved across `reset()` like `next_block_id`.
    pub(super) completed_seq: u64,
    /// Monotonic count of commands the shell started (an OSC 133/633 C the
    /// A→B→C→D gate accepted), block or no block. A clear (ED 3, Cmd-K) or a
    /// RIS can take a block, and a RIS also swallows the D that would have
    /// ended it, but no clear takes a C already counted: so a reader that
    /// counts C's knows how many commands ran, wherever their blocks went.
    /// Preserved across `reset()` like `completed_seq`. Nonzero also says the
    /// shell marks what it runs, which the OSC 7 prompt rule relies on
    /// (`TerminalHandler::shell_sits_at_prompt`).
    pub(super) started_seq: u64,
    /// Number of block command/output reads that found the block's rows
    /// already evicted from scrollback. A `Cell` so the `&self` accessors can
    /// count.
    pub(super) eviction_reads: std::cell::Cell<u64>,
    /// The newest block whose evicted COMMAND, and whose evicted OUTPUT, a
    /// read has found (`blocks_api::BlockSide`'s order). An evicted block
    /// stays evicted, and so is every older one (ids and rows only grow), so a
    /// read of a block no newer than these is counted but is not news.
    pub(super) eviction_seen: [std::cell::Cell<Option<u64>>; 2],
    /// How many times a read found a NEWER block's side evicted (the news
    /// `eviction_seen` admits). Only the 1st, 2nd, 4th … of them is logged: a
    /// program can start blocks at will.
    pub(super) eviction_news: std::cell::Cell<u64>,
    /// Which integration body the shell runs: the 16 lowercase hex digits of
    /// its script folder, as the body signs before every prompt
    /// (`633;P;AtermIntegration=<rev>;id=<nonce>`). Recorded only from a mark
    /// that passed the nonce gate; `None` until the first. Kept across
    /// `reset()` (RIS does not change the shell's script) and carried across a
    /// seamless update like the nonce (`TerminalCheckpoint::shell_integration_rev`).
    pub(super) integration_rev: Option<[u8; 16]>,
    /// The warm spare's marks (`633;P;AtermSpare=`, `AtermStart*=`), held for
    /// the host's one-shot takes. Kept across `reset()`, never checkpointed.
    pub(super) spare: super::shell::SpareMarks,
    /// The latest OSC 7 that arrived while an authenticated shell that does
    /// not sign its own reports sat at its prompt
    /// (`TerminalHandler::shell_sits_at_prompt`; an integration from before
    /// the signature, which an adopted PowerShell keeps until it restarts):
    /// the shell's next `A` applies it, as every integration reports its own
    /// cwd immediately before that mark; its `B` or `C` drops it, as do its
    /// signed `633;P;Cwd=` and its signed `AtermCwd`. Cleared by `reset()`; carried
    /// by a checkpoint (`ShellRepr::held_cwd`).
    pub(super) held_cwd: Option<super::shell::CwdReport>,
    /// The shell signs its own cwd report (`633;P;AtermCwd=<uri>;id=<nonce>`
    /// before its OSC 7, which every integration sends since 2026-10-19): one
    /// verified. A report no one signed while it sits at its prompt is then
    /// never its own, so it is dropped rather than held for its `A`. Kept across `reset()` (RIS does
    /// not change the shell's script); carried (`ShellRepr::signs_cwd`).
    pub(super) signs_cwd: bool,
    /// The shell finished a command it never marked with `C`: a signed `D`
    /// arrived at its prompt, which the A→B→C→D order refuses (PowerShell
    /// after `Import-Module PSReadLine -Force`, which replaces aterm's readline
    /// shim, its only `C`; measured on pwsh 7.6.7). Its phase
    /// then says nothing of what runs, so no report waits for its prompt until
    /// its next `C` clears this. Kept across `reset()`; carried
    /// (`ShellRepr::unmarked_run`).
    pub(super) unmarked_run: bool,
    /// The phase is not the shell's last word: a mark put a key in use that
    /// was not (a re-key the shell took, the first mark a successor whose
    /// handoff lost the nonce verifies), so the marks it signed before were
    /// dropped. Cleared by its next accepted `A`, and by `reset()`. Carried
    /// (`ShellRepr::phase_stale`): the first accepted property can take the
    /// nonce before that `A`, while the phase is still stale.
    pub(super) phase_stale: bool,
    /// The host's word that a program other than the shell holds the
    /// terminal's foreground now (`Terminal::set_foreground_job`: a unix host
    /// compares the foreground process group of the bytes it feeds with the
    /// shell's). A report then is that program's, though the shell's marks
    /// say it sits at its prompt (a zsh widget, a bash `bind -x` or a fish
    /// binding runs one with no `C`). Kept across `reset()` (host knowledge);
    /// never carried, as the host sets it before the bytes it feeds.
    pub(super) foreground_job: bool,
    /// The cwd a program's first report replaced at the shell's prompt
    /// ([`Self::foreground_job`], [`Self::program_probe`]), as a report:
    /// restored once the host says no such program runs, as the shell never
    /// reports its cwd again after a program a key started. Dropped by a
    /// signed report (the shell's newer word); kept across `reset()`; carried
    /// (`ShellRepr::cwd_before_job`).
    pub(super) cwd_before_job: Option<super::shell::CwdReport>,
    /// The host's word on demand (`Terminal::set_program_probe`): whether a
    /// program the shell started at or after a wall-clock instant (unix epoch
    /// ms) runs now where [`Self::foreground_job`] cannot see it (in the
    /// shell's own process group, where PowerShell keeps its programs, or
    /// under ConPTY, which has no foreground group). Asked only for a report
    /// no one signed at the shell's prompt that no foreground word claims, and
    /// while such a report shows. Host-side: never carried, kept across
    /// `reset()`.
    pub(super) program_probe: Option<Box<dyn Fn(u64) -> bool + Send>>,
    /// The host the report that set the stored cwd named
    /// (`Terminal::reported_cwd_host`): an OSC 7 `file://<host>/…`'s host,
    /// verbatim; `None` for an empty host or `localhost`, for a cwd the
    /// shell's own `633;P;Cwd=` set, and with no cwd. Kept across `reset()`
    /// like the cwd it qualifies. Never checkpointed: a restored engine names
    /// no host until its shell's next OSC 7.
    pub(super) cwd_host: Option<String>,
    /// How many reports this engine stored a cwd from (or cleared it by):
    /// every OSC 7 and `633;P;Cwd=` that passed the gate
    /// (`Terminal::reported_cwd_seq`). Monotonic: kept across `reset()` and an
    /// in-place restore, so an embedder that noted one count never meets it
    /// again for another report. Never checkpointed.
    pub(super) cwd_reports: u64,
    /// The stored cwd is a checkpoint's carry, not a report this engine saw
    /// (`Terminal::reported_cwd_seq` is `None`): set by both restores, cleared
    /// by the next stored report.
    pub(super) cwd_carried: bool,
}

impl ShellIntegrationState {
    pub(super) fn new() -> Self {
        Self {
            state: ShellState::Ground,
            phase: ShellIntegrationPhase::None,
            current_mark: None,
            command_marks: VecDeque::new(),
            output_blocks: VecDeque::new(),
            current_block: None,
            last_finished: None,
            next_block_id: 0,
            completed_seq: 0,
            started_seq: 0,
            eviction_reads: std::cell::Cell::new(0),
            eviction_seen: [const { std::cell::Cell::new(None) }; 2],
            eviction_news: std::cell::Cell::new(0),
            integration_rev: None,
            spare: super::shell::SpareMarks::default(),
            held_cwd: None,
            signs_cwd: false,
            unmarked_run: false,
            phase_stale: false,
            foreground_job: false,
            cwd_before_job: None,
            program_probe: None,
            cwd_host: None,
            cwd_reports: 0,
            cwd_carried: false,
        }
    }

    /// Reset data fields while preserving the block ID counter.
    pub(super) fn reset(&mut self) {
        self.state = ShellState::Ground;
        self.phase = ShellIntegrationPhase::None;
        self.current_mark = None;
        self.command_marks.clear();
        self.output_blocks.clear();
        self.current_block = None;
        self.held_cwd = None;
        self.phase_stale = false;
        // next_block_id, completed_seq and started_seq are not reset: block
        // ids stay unique and the counters monotonic across resets. Nor is
        // last_finished: the command it records did finish. Nor is cwd_host:
        // a RIS keeps the cwd it qualifies, and with it cwd_reports and
        // cwd_carried, which say where that cwd came from. Nor are the
        // shell's habits (signs_cwd, unmarked_run), the host's foreground_job
        // or the cwd a job's report replaced (cwd_before_job): a RIS changes
        // neither the shell's script nor who holds the terminal.
    }

    /// When the shell's prompt began by its own marks: its `A` (the current
    /// mark's `prompt_time_ms`), or, from its `D` until its next `A`, that `D`.
    /// `None` with no mark or no clock reading.
    pub(super) fn prompt_since_ms(&self) -> Option<u64> {
        match &self.current_mark {
            Some(mark) => mark.prompt_time_ms,
            None => self
                .command_marks
                .back()
                .and_then(|m| m.command_end_time_ms),
        }
    }

    /// Whether the host says a program the shell started since its prompt
    /// began runs now ([`Self::program_probe`]); `false` with no probe or no
    /// prompt time.
    pub(super) fn program_runs_since_prompt(&self) -> bool {
        match (&self.program_probe, self.prompt_since_ms()) {
            (Some(probe), Some(since)) => probe(since),
            _ => false,
        }
    }
}

/// Grouped cursor save/restore state: one save slot per screen, like xterm's
/// `screen->sc[whichBuf]`. DECSC/DECRC, SCOSC/SCORC, mode 1048 and mode 1049
/// share these slots, so a 1049 round trip leaves the slot saved for a later
/// bare DECRC.
pub(super) struct CursorSaveState {
    /// Saved cursor state for main screen (xterm sc[0]).
    pub(super) main: Option<SavedCursorState>,
    /// Saved cursor state for alt screen (xterm sc[1]).
    pub(super) alt: Option<SavedCursorState>,
}

impl CursorSaveState {
    pub(super) fn new() -> Self {
        Self {
            main: None,
            alt: None,
        }
    }

    /// Reset all saved cursor positions (called during terminal reset).
    pub(super) fn reset(&mut self) {
        *self = Self::new();
    }
}

/// One XTWINOPS title stack entry, `(icon name, window title)`: `None` is a slot
/// its push did not save (22;1t or 22;2t), as xterm's NULL.
pub(super) type TitleStackEntry = (Option<Arc<str>>, Option<Arc<str>>);

/// Window title (OSC 0/2), icon name (OSC 0/1), the title-change epoch and
/// the XTWINOPS title stack.
pub(super) struct TitleState {
    /// Window title (OSC 0 or OSC 2).
    pub(super) window: Arc<str>,
    /// Icon name (OSC 0 or OSC 1).
    pub(super) icon: Arc<str>,
    /// Title stack for CSI 22/23 t, capped at `TITLE_STACK_MAX_DEPTH`.
    pub(super) stack: Vec<TitleStackEntry>,
    /// Monotonic counter bumped each time a tab-label value changes (not on
    /// no-op re-sets): the window title, or the shell-reported cwd (OSC 7 / OSC
    /// 633 `P;Cwd=`) shown when no title is set. Hosts read it with a
    /// lock-free `Relaxed` load. See
    /// [`Terminal::title_epoch`](super::Terminal::title_epoch).
    pub(super) epoch: std::sync::atomic::AtomicU64,
}

impl TitleState {
    pub(super) fn new() -> Self {
        Self {
            window: Arc::from(""),
            icon: Arc::from(""),
            stack: Vec::new(),
            epoch: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Reset title data (xterm clears titles on RIS). Hosts see the clear
    /// through [`Terminal::title_epoch`](super::Terminal::title_epoch).
    pub(super) fn reset(&mut self) {
        // Clearing a non-empty window title is a real title change: bump the
        // epoch so hosts see the RIS clear.
        if !self.window.is_empty() {
            self.epoch
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.window = Arc::from("");
        self.icon = Arc::from("");
        self.stack.clear();
    }
}

/// Notification callbacks (OSC 9, OSC 99/777) and the OSC 99 multi-part
/// accumulation map.
pub(super) struct NotificationState {
    /// Simple desktop notification callback (OSC 9).
    pub(super) callback: Option<NotificationCallback>,
    /// Advanced desktop notification callback (OSC 99 - kitty protocol).
    pub(super) advanced_callback: Option<AdvancedNotificationCallback>,
    /// In-progress notifications being built from OSC 99 chunks.
    pub(super) pending: PendingNotificationsMap,
}

// OSC 9/99/777 notifications are gated by `modes.allow_notifications`, and the
// multi-part pending map is bounded by `MAX_PENDING_NOTIFICATIONS`; there is no
// separate callback-rate limiter.

impl NotificationState {
    pub(super) fn new() -> Self {
        Self {
            callback: None,
            advanced_callback: None,
            pending: PendingNotificationsMap::default(),
        }
    }

    /// Reset pending notifications while preserving callbacks.
    pub(super) fn reset(&mut self) {
        self.pending.clear();
    }
}

/// The OSC 52 clipboard callback.
pub(super) struct ClipboardState {
    /// Clipboard callback for OSC 52 operations.
    pub(super) callback: Option<ClipboardCallback>,
}

impl ClipboardState {
    pub(super) fn new() -> Self {
        Self { callback: None }
    }
}

/// Grouped state for Terminal OSC 1337 protocol extensions.
///
/// Bundles the OSC 1337 state the engine keeps: user variables, cursor line
/// highlight, shell integration version, remote host, and the cell pixel size.
pub(super) struct Iterm2State {
    /// User variables (OSC 1337 SetUserVar).
    pub(super) user_vars: UserVarsMap,
    /// First-insertion order of `user_vars` keys, for deterministic
    /// oldest-first eviction at capacity (`HashMap` iteration order is not
    /// deterministic). Re-inserting a key keeps its position.
    pub(super) user_vars_order: std::collections::VecDeque<String>,
    /// Cursor line highlight state (OSC 1337 HighlightCursorLine); `None`
    /// until a command is received.
    pub(super) highlight_cursor_line: Option<bool>,
    /// Last reported shell integration version (OSC 1337 ShellIntegrationVersion).
    pub(super) shell_integration_version: Option<super::types::Iterm2ShellIntegrationVersion>,
    /// Current remote host (OSC 1337 RemoteHost).
    pub(super) remote_host: Option<super::types::RemoteHost>,
    /// Cell pixel size `(width, height)` for converting pixel/auto image
    /// dimensions (OSC 1337 `File=`) into a cell footprint. Set from the
    /// frontend's font metrics via [`Terminal::set_cell_pixel_size`];
    /// [`DEFAULT_CELL_PX`] otherwise. Cell-unit dimensions ignore it.
    pub(super) cell_px: (u16, u16),
    /// Whether `cell_px` was measured by a host
    /// ([`Terminal::set_cell_pixel_size`]) rather than [`DEFAULT_CELL_PX`]. The
    /// placeholder is fine for footprint arithmetic but not as an answer: the
    /// XTWINOPS pixel reports (CSI 14 t / 16 t) read
    /// [`Terminal::host_cell_pixel_size`] and stay silent while this is false.
    pub(super) cell_px_from_host: bool,
}

/// Default cell pixel size for inline-image footprint math before the frontend
/// reports real metrics; affects only pixel/auto-sized images.
pub(super) const DEFAULT_CELL_PX: (u16, u16) = (8, 16);

impl Iterm2State {
    pub(super) fn new() -> Self {
        Self {
            user_vars: UserVarsMap::default(),
            user_vars_order: std::collections::VecDeque::new(),
            highlight_cursor_line: None,
            shell_integration_version: None,
            remote_host: None,
            cell_px: DEFAULT_CELL_PX,
            cell_px_from_host: false,
        }
    }

    /// The cell box a host measured, or `None` for the placeholder: the one
    /// predicate behind [`Terminal::host_cell_pixel_size`] and the XTWINOPS
    /// pixel reports. A zero axis counts as unreported.
    pub(super) fn host_cell_px(&self) -> Option<(u16, u16)> {
        let (w, h) = self.cell_px;
        (self.cell_px_from_host && w > 0 && h > 0).then_some((w, h))
    }

    /// Reset the program-set fields; the host's cell size survives.
    pub(super) fn reset(&mut self) {
        self.remote_host = None;
        self.highlight_cursor_line = None;
        self.shell_integration_version = None;
        self.user_vars.clear();
        self.user_vars_order.clear();
    }
}

/// Grouped BiDi (bidirectional text) state: the configuration and the reorder
/// scratch.
pub(super) struct BiDiGroupState {
    /// BiDi configuration (mode, direction, alternate screen, etc.).
    pub(super) config: crate::config::BiDiConfig,
    /// Reusable per-row scratch buffers for the render-time visual reorder,
    /// kept so their capacity persists across rows and frames.
    pub(super) scratch: BidiScratch,
}

impl BiDiGroupState {
    pub(super) fn new() -> Self {
        Self {
            config: crate::config::BiDiConfig::default(),
            scratch: BidiScratch::new(),
        }
    }
}

/// Reusable per-row scratch for BiDi visual reordering:
/// `Terminal::apply_bidi_reorder` clears and refills these for each RTL row
/// instead of allocating.
pub(super) struct BidiScratch {
    /// Per-cell BiDi classes for the current row (computed once per row).
    pub(super) classes: Vec<aterm_bidi::BidiClass>,
    /// Per-cell wide-continuation flags for the current row.
    pub(super) wide: Vec<bool>,
    /// Inverse permutation: `inv[logical] = visual column`.
    pub(super) inv: Vec<usize>,
    /// Holds the reordered (visual-order) row while it is swapped into the frame.
    pub(super) row_tmp: Vec<crate::terminal::RenderCell>,
    /// Per-cell paired-bracket properties for the current row (UAX #9 N0).
    pub(super) brackets: Vec<aterm_bidi::Bracket>,
    /// UAX #9 working memory for
    /// [`aterm_bidi::reorder_cells_with_classes_into`], reused across rows.
    pub(super) bidi: aterm_bidi::Scratch,
    /// The resulting visual→logical CELL permutation.
    pub(super) cell_order: Vec<usize>,
}

impl BidiScratch {
    pub(super) fn new() -> Self {
        Self {
            classes: Vec::new(),
            wide: Vec::new(),
            inv: Vec::new(),
            row_tmp: Vec::new(),
            brackets: Vec::new(),
            bidi: aterm_bidi::Scratch::default(),
            cell_order: Vec::new(),
        }
    }
}
